//! The printer's questions during `sprint print`: host prompts
//! (`//action:prompt_*`, answered with M876), waits for the user that
//! come without one (M0/M1 on firmware without host prompt support,
//! released with M108) and a print the printer paused by itself (resumed
//! by the host).
//!
//! A question is printed when it appears. With `--answer-prompts ask`
//! (the default) the answer is a line on standard input: the number of a
//! button, or an empty line for the first one; when standard input has
//! ended, the question can still be answered at the printer. On a
//! terminal, lines typed before a question are forgotten (a stray Enter
//! must not answer it); piped answers are used in order. With
//! `--answer-prompts continue` the answer that lets the print go on is
//! given at once, except to a pause made at the printer (its menu's
//! pause, which offers only Resume, or a pause without a question): that
//! is left to the person there. Each question is answered once: while it still shows
//! after its answer (a wait for the user ends with the printer's next busy
//! message) no further line is spent on it, and one that is still there
//! after a few seconds is asked again.

use std::io::{BufRead, IsTerminal};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use sprint_host::HostPrompt;

use crate::cli::AnswerPrompts;

/// How long a question may stay up after it was answered before it is
/// asked again. A wait for the user shows until the printer's next busy
/// message (every two seconds on Marlin), so it lingers a moment after its
/// answer; still there after this, the answer did not take.
const REASK: Duration = Duration::from_secs(5);

/// What the printer waits for, if anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Question<'a> {
    /// A host prompt with buttons.
    Prompt(&'a HostPrompt),
    /// The firmware waits for the user without a prompt (`busy: paused
    /// for user`, see [`sprint_tui::actions::waiting_for_user`]).
    WaitingForUser,
    /// The printer paused the print by itself (`//action:paused`, a
    /// runout) and shows no prompt.
    Paused,
}

/// What [`PromptWatch::update`] asks the caller to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Tell the user (stderr).
    Say(String),
    /// Answer the host prompt with this button (M876 S<index>).
    Answer(usize),
    /// Let the printer go on: release a firmware waiting for the user
    /// (M108) or resume a print the printer paused.
    Continue,
}

/// Which question is on screen.
#[derive(Clone, Debug, PartialEq)]
enum Shown {
    Prompt(HostPrompt),
    WaitingForUser,
    Paused,
}

impl Shown {
    fn of(question: Question<'_>) -> Shown {
        match question {
            Question::Prompt(prompt) => Shown::Prompt(prompt.clone()),
            Question::WaitingForUser => Shown::WaitingForUser,
            Question::Paused => Shown::Paused,
        }
    }
}

/// Opens the lines of standard input (see [`stdin_lines`]).
type OpenInput = Box<dyn FnOnce() -> Receiver<String>>;

/// Follows the printer's questions; see the module docs.
pub struct PromptWatch {
    policy: AnswerPrompts,
    /// Lines typed on standard input, once they are read.
    answers: Option<Receiver<String>>,
    /// Starts reading them (once).
    open_input: Option<OpenInput>,
    /// Standard input has ended (nothing more can be typed).
    input_closed: bool,
    /// The question shown last (until it goes away).
    shown: Option<Shown>,
    /// Forget lines typed while nothing was asked (a person at a
    /// terminal); piped answers are kept for the questions to come.
    forget_early_lines: bool,
    /// When the question shown was answered: it is not answered again
    /// while it lingers (a second M108 could break the next heat wait,
    /// and a piped answer belongs to the next question), for `reask`.
    answered: Option<Instant>,
    /// How long an answered question may linger ([`REASK`]).
    reask: Duration,
}

impl PromptWatch {
    /// A watch answering as `policy` says; answers are read from standard
    /// input.
    pub fn new(policy: AnswerPrompts) -> PromptWatch {
        PromptWatch::with_input(
            policy,
            Box::new(stdin_lines),
            std::io::stdin().is_terminal(),
        )
    }

    /// A watch reading its answers from what `open` returns, typed at a
    /// terminal (`terminal`) or piped.
    fn with_input(policy: AnswerPrompts, open: OpenInput, terminal: bool) -> PromptWatch {
        let mut watch = PromptWatch {
            policy,
            answers: None,
            open_input: Some(open),
            input_closed: false,
            shown: None,
            forget_early_lines: terminal,
            answered: None,
            reask: REASK,
        };
        // On a terminal, read from the start: a line typed before a
        // question is then seen, and forgotten, instead of waiting in the
        // terminal to answer the first question. Piped answers are read
        // when the first question comes.
        if policy == AnswerPrompts::Ask && terminal {
            watch.start_input();
        }
        watch
    }

    /// A watch reading its answers from `answers` (tests), typed at a
    /// terminal (`terminal`) or piped.
    #[cfg(test)]
    pub fn with_answers(
        policy: AnswerPrompts,
        answers: Receiver<String>,
        terminal: bool,
    ) -> PromptWatch {
        let mut watch = PromptWatch::with_input(policy, Box::new(move || answers), terminal);
        watch.start_input();
        watch
    }

    /// Look at what the printer waits for now (`None`: nothing).
    pub fn update(&mut self, question: Option<Question<'_>>) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(question) = question else {
            self.shown = None;
            self.answered = None;
            // An early Enter must not answer a later question.
            self.drain();
            return effects;
        };
        let now = Shown::of(question);
        if self.shown.as_ref() != Some(&now) {
            self.shown = Some(now);
            self.answered = None;
            self.ask(question, &mut effects);
        } else if let Some(at) = self.answered {
            if at.elapsed() >= self.reask {
                self.answered = None;
                effects.push(Effect::Say(
                    "The printer still waits for an answer".to_string(),
                ));
                self.ask(question, &mut effects);
            }
        } else if let Some(line) = self.next_line(&mut effects) {
            match question {
                Question::Prompt(prompt) => answer_line(prompt, &line, &mut effects),
                Question::WaitingForUser | Question::Paused => {
                    effects.push(Effect::Continue);
                    effects.push(Effect::Say(continuing(question).to_string()));
                }
            }
        }
        if effects
            .iter()
            .any(|e| matches!(e, Effect::Answer(_) | Effect::Continue))
        {
            self.answered = Some(Instant::now());
        }
        effects
    }

    fn ask(&mut self, question: Question<'_>, effects: &mut Vec<Effect>) {
        match question {
            Question::Prompt(prompt) => {
                effects.push(Effect::Say(format!(
                    "The printer asks: {}",
                    printable(&prompt.message)
                )));
                for (i, button) in prompt.buttons.iter().enumerate() {
                    effects.push(Effect::Say(format!("  [{}] {}", i + 1, printable(button))));
                }
            }
            Question::WaitingForUser => effects.push(Effect::Say(
                "The printer waits for the user (M0/M1 or a filament change)".to_string(),
            )),
            Question::Paused => effects.push(Effect::Say(
                "The printer paused the print (a filament runout, or paused at the printer)"
                    .to_string(),
            )),
        }
        match self.policy {
            AnswerPrompts::Continue => match question {
                // Someone at the printer paused it (or a runout did):
                // resuming at once would undo that.
                Question::Prompt(prompt) if resumes_a_pause(prompt) => {
                    effects.push(Effect::Say(PAUSED_AT_THE_PRINTER.to_string()));
                }
                Question::Paused => {
                    effects.push(Effect::Say(PAUSED_AT_THE_PRINTER.to_string()));
                }
                Question::Prompt(prompt) => {
                    let index = continue_button(&prompt.buttons);
                    effects.push(Effect::Answer(index));
                    effects.push(Effect::Say(format!(
                        "Answered {} (--answer-prompts continue)",
                        button_name(prompt, index)
                    )));
                }
                Question::WaitingForUser => {
                    effects.push(Effect::Continue);
                    effects.push(Effect::Say(format!(
                        "{} (--answer-prompts continue)",
                        continuing(question)
                    )));
                }
            },
            AnswerPrompts::Ask => {
                self.drain();
                self.start_input();
                let typed = match question {
                    Question::Prompt(prompt) if prompt.buttons.len() > 1 => {
                        "Type the number of an answer and Enter (Enter alone answers 1)"
                    }
                    Question::Prompt(_) => "Press Enter to answer",
                    Question::WaitingForUser => "Press Enter to continue (sends M108)",
                    Question::Paused => "Press Enter to resume the print",
                };
                effects.push(Effect::Say(self.how_to_answer(typed)));
            }
        }
    }

    /// The hint for answering, or where to answer when nothing can be
    /// typed.
    fn how_to_answer(&self, typed: &str) -> String {
        if self.input_closed {
            "Nothing can be typed here (standard input has ended): answer at the printer, \
             or print with --answer-prompts continue; Ctrl+C cancels the print"
                .to_string()
        } else {
            format!("{typed}, or answer at the printer; Ctrl+C cancels the print")
        }
    }

    /// Read lines from standard input from now on.
    fn start_input(&mut self) {
        if self.answers.is_none()
            && !self.input_closed
            && let Some(open) = self.open_input.take()
        {
            self.answers = Some(open());
        }
    }

    /// The next typed line, if one came.
    fn next_line(&mut self, effects: &mut Vec<Effect>) -> Option<String> {
        let answers = self.answers.as_ref()?;
        match answers.try_recv() {
            Ok(line) => Some(line),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.answers = None;
                self.input_closed = true;
                if self.policy == AnswerPrompts::Ask {
                    effects.push(Effect::Say(self.how_to_answer("")));
                }
                None
            }
        }
    }

    /// Forget lines typed at a terminal while nothing was asked.
    fn drain(&mut self) {
        if let Some(answers) = self.answers.as_ref().filter(|_| self.forget_early_lines) {
            while answers.try_recv().is_ok() {}
        }
    }
}

/// What `--answer-prompts continue` says about a pause made at the
/// printer.
const PAUSED_AT_THE_PRINTER: &str = "Paused at the printer: resume it at the printer \
     (--answer-prompts continue does not); Ctrl+C cancels the print";

/// Whether `prompt` only offers to resume a print paused at the printer
/// (Marlin's "UI Pause" from its own menu): no Continue to press.
fn resumes_a_pause(prompt: &HostPrompt) -> bool {
    let offers = |word: &str| {
        prompt
            .buttons
            .iter()
            .any(|b| b.to_ascii_lowercase().contains(word))
    };
    offers("resume") && !offers("continue")
}

/// What is said when the printer is told to go on.
fn continuing(question: Question<'_>) -> &'static str {
    match question {
        Question::Paused => "Resuming the print",
        _ => "Continuing (M108)",
    }
}

/// Answer `prompt` with a typed `line`: a button number (1-based), or
/// nothing for the first button.
fn answer_line(prompt: &HostPrompt, line: &str, effects: &mut Vec<Effect>) {
    let text = line.trim();
    let count = prompt.buttons.len().max(1);
    let index = if text.is_empty() {
        Some(0)
    } else {
        text.parse::<usize>()
            .ok()
            .filter(|n| (1..=count).contains(n))
            .map(|n| n - 1)
    };
    match index {
        Some(index) => {
            effects.push(Effect::Answer(index));
            effects.push(Effect::Say(format!(
                "Answered {}",
                button_name(prompt, index)
            )));
        }
        None => effects.push(Effect::Say(format!(
            "'{}' is not one of the answers (1 to {count})",
            printable(text)
        ))),
    }
}

/// "Continue", or "1" for a prompt without buttons.
fn button_name(prompt: &HostPrompt, index: usize) -> String {
    prompt
        .buttons
        .get(index)
        .map_or_else(|| (index + 1).to_string(), |b| printable(b))
}

/// The button that lets the print go on: Continue or Resume, else OK or
/// Dismiss, else the first one. (Marlin's filament change offers "Purge
/// More" before "Continue".)
pub fn continue_button(buttons: &[String]) -> usize {
    let find = |words: &[&str]| {
        buttons.iter().position(|b| {
            let lower = b.to_ascii_lowercase();
            words.iter().any(|w| lower.contains(w))
        })
    };
    find(&["continue", "resume"])
        .or_else(|| find(&["ok", "dismiss", "done"]))
        .unwrap_or(0)
}

/// `text` with control characters (which could move the cursor or change
/// colors) shown as `·`.
pub fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '·' } else { c })
        .collect()
}

/// Lines of standard input, read on a helper thread; the channel closes
/// at the end of the input.
fn stdin_lines() -> Receiver<String> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let spawned = std::thread::Builder::new()
        .name("sprint-print-input".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else {
                    break;
                };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("cannot read answers from standard input: {e}");
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(message: &str, buttons: &[&str]) -> HostPrompt {
        HostPrompt {
            message: message.into(),
            buttons: buttons.iter().map(|b| (*b).to_string()).collect(),
        }
    }

    fn said(effects: &[Effect]) -> String {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Say(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_prompt_is_shown_once_and_answered_from_the_input() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut watch = PromptWatch::with_answers(AnswerPrompts::Ask, rx, true);
        let magnets = prompt("Insert magnets\x1b[2J", &["Continue", "Skip"]);
        let ask = || Some(Question::Prompt(&magnets));
        // Typed before the question: ignored.
        tx.send(String::new()).expect("send");
        let effects = watch.update(ask());
        let text = said(&effects);
        assert!(
            text.contains("The printer asks: Insert magnets·[2J"),
            "{text}"
        );
        assert!(
            text.contains("[1] Continue") && text.contains("[2] Skip"),
            "{text}"
        );
        assert!(text.contains("Type the number of an answer"), "{text}");
        assert!(!effects.iter().any(|e| matches!(e, Effect::Answer(_))));
        // Not shown again while it stays open.
        assert_eq!(watch.update(ask()), []);
        tx.send("7".into()).expect("send");
        assert!(said(&watch.update(ask())).contains("not one of the answers"));
        tx.send(" 2 ".into()).expect("send");
        let effects = watch.update(ask());
        assert!(effects.contains(&Effect::Answer(1)), "{effects:?}");
        assert!(said(&effects).contains("Answered Skip"));
        // Closed, then asked again: shown again; Enter answers the first.
        assert_eq!(watch.update(None), []);
        assert!(!watch.update(ask()).is_empty());
        tx.send(String::new()).expect("send");
        assert!(watch.update(ask()).contains(&Effect::Answer(0)));
    }

    #[test]
    fn a_line_typed_before_the_first_question_does_not_answer_it() {
        let (tx, rx) = crossbeam_channel::unbounded();
        // Typed at the terminal long before anything was asked.
        tx.send(String::new()).expect("send");
        let mut watch = PromptWatch::with_input(AnswerPrompts::Ask, Box::new(move || rx), true);
        assert_eq!(watch.update(None), []);
        let magnets = prompt("Insert magnets", &["Continue"]);
        let ask = || Some(Question::Prompt(&magnets));
        assert!(!watch.update(ask()).is_empty());
        let effects = watch.update(ask());
        assert!(!effects.contains(&Effect::Answer(0)), "{effects:?}");
        // What is typed now answers it.
        tx.send(String::new()).expect("send");
        assert!(watch.update(ask()).contains(&Effect::Answer(0)));
    }

    #[test]
    fn piped_answers_are_used_in_order() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut watch = PromptWatch::with_answers(AnswerPrompts::Ask, rx, false);
        for line in ["", "2"] {
            tx.send(line.into()).expect("send");
        }
        drop(tx);
        // Lines that came before the questions answer them, in order.
        assert_eq!(watch.update(None), []);
        assert!(!watch.update(Some(Question::WaitingForUser)).is_empty());
        assert!(
            watch
                .update(Some(Question::WaitingForUser))
                .contains(&Effect::Continue)
        );
        let change = prompt("Nozzle Parked", &["Purge More", "Continue"]);
        let ask = || Some(Question::Prompt(&change));
        assert!(!watch.update(ask()).contains(&Effect::Answer(1)));
        assert!(watch.update(ask()).contains(&Effect::Answer(1)));
        // Then the input has ended: the next question is answered at the
        // printer.
        assert_eq!(watch.update(None), []);
        assert!(!watch.update(ask()).is_empty());
        assert!(said(&watch.update(ask())).contains("answer at the printer"));
    }

    #[test]
    fn an_answered_wait_is_not_answered_again_while_it_lingers() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut watch = PromptWatch::with_answers(AnswerPrompts::Ask, rx, false);
        for line in ["", ""] {
            tx.send(line.into()).expect("send");
        }
        let wait = || Some(Question::WaitingForUser);
        assert!(!watch.update(wait()).contains(&Effect::Continue));
        assert!(watch.update(wait()).contains(&Effect::Continue));
        // The printer still reports the old wait until its next busy
        // message: the second answer is not spent on it.
        for _ in 0..5 {
            assert_eq!(watch.update(wait()), []);
        }
        // It is there for the next question.
        assert_eq!(watch.update(None), []);
        assert!(!watch.update(wait()).is_empty());
        assert!(watch.update(wait()).contains(&Effect::Continue));
    }

    #[test]
    fn a_wait_that_outlasts_its_answer_is_asked_again() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut watch = PromptWatch::with_answers(AnswerPrompts::Ask, rx, true);
        watch.reask = Duration::ZERO;
        let wait = || Some(Question::WaitingForUser);
        assert!(!watch.update(wait()).is_empty());
        tx.send(String::new()).expect("send");
        assert!(watch.update(wait()).contains(&Effect::Continue));
        // Still waiting long after the answer: the answer was lost.
        let text = said(&watch.update(wait()));
        assert!(text.contains("still waits for an answer"), "{text}");
        assert!(text.contains("Press Enter to continue"), "{text}");
        tx.send(String::new()).expect("send");
        assert!(watch.update(wait()).contains(&Effect::Continue));
    }

    #[test]
    fn continue_answers_at_once() {
        let mut watch = PromptWatch::new(AnswerPrompts::Continue);
        let purge = prompt("Paused", &["PurgeMore", "Continue"]);
        let effects = watch.update(Some(Question::Prompt(&purge)));
        // Not the first button: that would purge for ever.
        assert!(effects.contains(&Effect::Answer(1)), "{effects:?}");
        assert!(said(&effects).contains("Answered Continue"));
        let effects = watch.update(Some(Question::WaitingForUser));
        assert!(effects.contains(&Effect::Continue), "{effects:?}");
        assert!(said(&effects).contains("waits for the user"));
        // Announced once per wait.
        assert_eq!(watch.update(Some(Question::WaitingForUser)), []);
        assert_eq!(continue_button(&[]), 0);
        assert_eq!(continue_button(&["Resume".into()]), 0);
        assert_eq!(continue_button(&["Purge".into(), "Dismiss".into()]), 1);
    }

    #[test]
    fn continue_leaves_a_pause_made_at_the_printer_alone() {
        let mut watch = PromptWatch::new(AnswerPrompts::Continue);
        // Paused at the printer (or by a runout) without a question.
        let effects = watch.update(Some(Question::Paused));
        assert!(!effects.contains(&Effect::Continue), "{effects:?}");
        assert!(
            said(&effects).contains("resume it at the printer"),
            "{effects:?}"
        );
        // Marlin's pause from its own menu, with host prompts.
        assert_eq!(watch.update(None), []);
        let ui_pause = prompt("UI Pause", &["Resume"]);
        let effects = watch.update(Some(Question::Prompt(&ui_pause)));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Answer(_))),
            "{effects:?}"
        );
        assert!(
            said(&effects).contains("resume it at the printer"),
            "{effects:?}"
        );
        // A filament change's questions are still answered.
        assert_eq!(watch.update(None), []);
        let parked = prompt("Nozzle Parked", &["Continue"]);
        assert!(
            watch
                .update(Some(Question::Prompt(&parked)))
                .contains(&Effect::Answer(0))
        );
    }

    #[test]
    fn a_wait_without_a_prompt_is_released_by_enter() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut watch = PromptWatch::with_answers(AnswerPrompts::Ask, rx, true);
        let effects = watch.update(Some(Question::WaitingForUser));
        let text = said(&effects);
        assert!(text.contains("waits for the user"), "{text}");
        assert!(
            text.contains("Press Enter to continue (sends M108)"),
            "{text}"
        );
        assert!(!effects.contains(&Effect::Continue));
        tx.send(String::new()).expect("send");
        assert!(
            watch
                .update(Some(Question::WaitingForUser))
                .contains(&Effect::Continue)
        );
        // A pause by the printer is a new question.
        let text = said(&watch.update(Some(Question::Paused)));
        assert!(text.contains("Press Enter to resume"), "{text}");
        // The input ends: the user is told to answer at the printer.
        drop(tx);
        let text = said(&watch.update(Some(Question::Paused)));
        assert!(text.contains("answer at the printer"), "{text}");
        assert!(text.contains("--answer-prompts continue"), "{text}");
        assert_eq!(printable("a\tb\x07"), "a·b·");
    }
}
