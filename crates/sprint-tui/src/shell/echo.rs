//! The echo thread: prints what the printer says while the shell waits for
//! input, and tells waiting commands when their answer has arrived.
//!
//! It is the only reader of the printer's events, so the order of console
//! lines and the events that finish a request is kept: a report command
//! registers an [`Awaited`] answer before sending its request; until the
//! answer has arrived and the request was acknowledged (the link is idle)
//! the raw reply lines are not echoed (the command prints them as a table
//! instead), then the command is woken.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use sprint_host::{ConsoleEntry, ConsoleKind, Event, HostPrompt, JobStatus, Printer};

use super::output::Out;
use crate::report::{ReportLine, Tone};
use crate::session::{IDLE_SETTLE, link_idle};

/// How often the echo thread looks at the link when no event arrives.
const TICK: Duration = Duration::from_millis(20);
/// After the answer, how long to wait for the link to go idle (it may
/// never, while a print streams).
const ANSWER_GRACE: Duration = Duration::from_secs(1);
/// The longest [`Echo::settle`] waits.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(1);

/// What a waiting command expects from the printer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Awaited {
    /// The handshake and the setup queries (M115, M503, M114): quiet until
    /// the connect command, which reports the outcome, ends the wait.
    Connect,
    /// M503 settings.
    Settings,
    /// M115 firmware information.
    Firmware,
    /// M119 endstops.
    Endstops,
    /// A bed mesh report.
    Mesh,
    /// M78 statistics.
    Stats,
    /// M105 temperatures.
    Temperatures,
    /// The SD card listing.
    SdFiles,
    /// Nothing in particular: the link going idle.
    Idle,
}

impl Awaited {
    /// Whether `event` is the answer.
    fn answered_by(self, event: &Event) -> bool {
        matches!(
            (self, event),
            (Awaited::Settings, Event::SettingsUpdated)
                | (Awaited::Firmware, Event::Firmware(_))
                | (Awaited::Endstops, Event::Endstops(_))
                | (Awaited::Mesh, Event::Mesh(_))
                | (Awaited::Stats, Event::Stats(_))
                | (Awaited::Temperatures, Event::Temperatures(_))
                | (Awaited::SdFiles, Event::SdFiles(_))
        )
    }
}

struct Expect {
    what: Awaited,
    /// An idle link (after the request went out) also ends the wait: the
    /// answer may bring no news (no mesh, no card).
    idle_completes: bool,
    since: Instant,
    /// When the answering event arrived.
    answered: Option<Instant>,
    idle_since: Option<Instant>,
    done: Sender<()>,
}

impl Expect {
    /// Whether the wait is over, given whether the link is idle now.
    fn finished(&mut self, idle: bool, now: Instant) -> bool {
        if self.what == Awaited::Connect {
            return false;
        }
        if !idle {
            self.idle_since = None;
            // The answer came while other traffic keeps the link busy.
            return self
                .answered
                .is_some_and(|t| now.saturating_duration_since(t) >= ANSWER_GRACE);
        }
        if self.answered.is_none() && !self.idle_completes {
            return false;
        }
        if now.saturating_duration_since(self.since) < IDLE_SETTLE {
            return false;
        }
        let since = *self.idle_since.get_or_insert(now);
        now.saturating_duration_since(since) >= IDLE_SETTLE
    }
}

/// State shared with the echo thread.
struct Shared {
    /// Echo everything (sent lines, polling, `ok`s).
    log_all: AtomicBool,
    expect: Mutex<Option<Expect>>,
    /// Commands waiting for the echo thread to catch up.
    settle: Mutex<Vec<Sender<()>>>,
    stop: AtomicBool,
}

/// Handle to the echo thread; dropping it stops the thread.
pub(crate) struct Echo {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Echo {
    /// Start echoing `printer`'s events to `out`.
    pub(crate) fn start(printer: &Printer, out: Out) -> Echo {
        let shared = Arc::new(Shared {
            log_all: AtomicBool::new(false),
            expect: Mutex::new(None),
            settle: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let events = printer.subscribe();
        let worker = Worker {
            printer: printer.clone(),
            out,
            shared: Arc::clone(&shared),
            user_wait_told: std::cell::Cell::new(false),
        };
        let thread = std::thread::Builder::new()
            .name("sprint-shell-echo".into())
            .spawn(move || worker.run(&events))
            .map_err(|e| log::warn!("cannot start the echo thread: {e}"))
            .ok();
        Echo { shared, thread }
    }

    /// Echo all traffic (`log on`) or only what matters (`log off`).
    pub(crate) fn set_log_all(&self, on: bool) {
        self.shared.log_all.store(on, Ordering::Relaxed);
    }

    /// Whether all traffic is echoed.
    pub(crate) fn log_all(&self) -> bool {
        self.shared.log_all.load(Ordering::Relaxed)
    }

    /// Expect `what` (call before sending the request): the replies are
    /// not echoed until it arrives, then the returned channel is signalled.
    pub(crate) fn expect(&self, what: Awaited, idle_completes: bool) -> Receiver<()> {
        let (done, rx) = crossbeam_channel::bounded(1);
        *self.lock() = Some(Expect {
            what,
            idle_completes: idle_completes || what == Awaited::Idle,
            since: Instant::now(),
            answered: None,
            idle_since: None,
            done,
        });
        rx
    }

    /// Stop expecting (the command is done or gave up).
    pub(crate) fn cancel_expect(&self) {
        *self.lock() = None;
    }

    /// Wait (briefly) until the echo thread has handled every event the
    /// printer emitted before this call, so replies that are still queued
    /// are not echoed after a wait ends.
    pub(crate) fn settle(&self) {
        if self.thread.is_none() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.shared
            .settle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tx);
        let _ = rx.recv_timeout(SETTLE_TIMEOUT);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Expect>> {
        self.shared
            .expect
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Echo {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Worker {
    printer: Printer,
    out: Out,
    shared: Arc<Shared>,
    /// The current wait for the user (without a prompt) was announced.
    user_wait_told: std::cell::Cell<bool>,
}

impl Worker {
    fn run(&self, events: &Receiver<Event>) {
        while !self.shared.stop.load(Ordering::Relaxed) {
            match events.recv_timeout(TICK) {
                Ok(event) => self.on_event(&event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            // Everything emitted so far has been handled.
            if events.is_empty() {
                self.check_idle();
                self.release_settled();
            }
        }
        self.release_settled();
    }

    fn release_settled(&self) {
        let waiting = std::mem::take(
            &mut *self
                .shared
                .settle
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for tx in waiting {
            let _ = tx.try_send(());
        }
    }

    fn awaited(&self) -> Option<Awaited> {
        self.shared
            .expect
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|e| e.what)
    }

    /// Wake the waiting command.
    fn complete(&self) {
        let expect = self
            .shared
            .expect
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(expect) = expect {
            let _ = expect.done.try_send(());
        }
    }

    /// With no events queued, everything the printer sent so far has been
    /// echoed: an idle link now ends waits that allow it.
    fn check_idle(&self) {
        let finished = {
            let mut guard = self
                .shared
                .expect
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(expect) = guard.as_mut() else {
                return;
            };
            let idle = self.printer.with_state(link_idle);
            expect.finished(idle, Instant::now())
        };
        if finished {
            self.complete();
        }
    }

    /// Note that the awaited answer arrived.
    fn answered(&self) {
        if let Some(expect) = self
            .shared
            .expect
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            expect.answered.get_or_insert_with(Instant::now);
        }
    }

    fn on_event(&self, event: &Event) {
        let awaited = self.awaited();
        if awaited.is_some_and(|a| a.answered_by(event)) {
            // The lines after it (and the request's ok) are still part of
            // the answer: the wait ends once the link is idle.
            self.answered();
            return;
        }
        // The printer's console already narrates connections, job ends,
        // errors and autotune results; only what it leaves out is added.
        match event {
            Event::Console(entry) => {
                if let Some(line) = self.console_line(entry, awaited.is_some()) {
                    self.out.line(line);
                }
                if entry.kind == ConsoleKind::Received {
                    self.tell_user_wait();
                }
            }
            Event::Job(JobStatus::Paused) => {
                let name = self
                    .printer
                    .with_state(|s| s.job.as_ref().map(|j| j.name.clone()))
                    .unwrap_or_default();
                self.out.warn(format!("Paused {name} (resume continues)"));
            }
            Event::Prompt(Some(prompt)) => self.out.line(prompt_line(prompt)),
            Event::AutotuneFinished { .. } => {
                self.out
                    .dim("The firmware uses the new PID values now; eeprom save keeps them");
            }
            _ => {}
        }
    }

    /// Say once per wait that the firmware waits for the user without a
    /// host prompt (stock Marlin without HOST_PROMPT_SUPPORT after M0 or
    /// M600), and how to let it go on.
    fn tell_user_wait(&self) {
        let waiting = self.printer.with_state(crate::actions::waiting_for_user);
        if waiting && !self.user_wait_told.get() {
            self.out
                .warn("The printer waits for the user: resume (or M108) continues");
        }
        self.user_wait_told.set(waiting);
    }

    /// How a console entry is echoed, if at all. `quiet` while a command
    /// waits for a report: its raw replies are left out.
    fn console_line(&self, entry: &ConsoleEntry, quiet: bool) -> Option<ReportLine> {
        let log_all = self.shared.log_all.load(Ordering::Relaxed);
        console_line(entry, log_all, quiet)
    }
}

/// How a console entry is echoed (see [`Worker::console_line`]).
pub(crate) fn console_line(entry: &ConsoleEntry, log_all: bool, quiet: bool) -> Option<ReportLine> {
    let text = entry.text.as_str();
    match entry.kind {
        ConsoleKind::Sent => log_all.then(|| ReportLine::toned(format!("> {text}"), Tone::Dim)),
        ConsoleKind::Received => {
            if is_error_reply(text) {
                return Some(ReportLine::toned(text, Tone::Bad));
            }
            if log_all {
                let tone = if entry.auto { Tone::Dim } else { Tone::Plain };
                return Some(ReportLine::toned(text, tone));
            }
            let hidden = entry.auto || quiet || is_bare_ok(text) || is_chatter(text);
            (!hidden).then(|| ReportLine::plain(text))
        }
        ConsoleKind::Info => Some(ReportLine::toned(text, Tone::Dim)),
        ConsoleKind::Warning => Some(ReportLine::toned(text, Tone::Warn)),
        ConsoleKind::Error => Some(ReportLine::toned(text, Tone::Bad)),
    }
}

/// An `ok` without content (`ok`, `ok N12 P15 B3`).
pub(crate) fn is_bare_ok(text: &str) -> bool {
    let text = text.trim();
    let Some(rest) = text.strip_prefix("ok") else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    if !rest.starts_with(' ') {
        return false;
    }
    rest.split_whitespace().all(|word| {
        let mut chars = word.chars();
        matches!(chars.next(), Some('N' | 'P' | 'B'))
            && chars.as_str().chars().all(|c| c.is_ascii_digit())
    })
}

/// Keep-alive lines that only say the printer is working, and host
/// action lines (prompts are narrated instead).
fn is_chatter(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower == "wait"
        || lower.starts_with("echo:busy:")
        || lower.starts_with("busy:")
        || lower.starts_with("//action:")
}

/// "The printer asks: Nozzle parked  (answer M876 S0 = Continue)".
fn prompt_line(prompt: &HostPrompt) -> ReportLine {
    let mut line =
        ReportLine::toned("The printer asks: ", Tone::Warn).with(&prompt.message, Tone::Plain);
    if prompt.buttons.is_empty() {
        line.push("  (M876 S0 continues)", Tone::Dim);
    } else {
        let answers: Vec<String> = prompt
            .buttons
            .iter()
            .enumerate()
            .map(|(i, b)| format!("M876 S{i} = {b}"))
            .collect();
        line.push(format!("  (answer {})", answers.join(", ")), Tone::Dim);
    }
    line
}

/// Firmware errors: always shown.
fn is_error_reply(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("error") || lower.starts_with("!!")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: ConsoleKind, text: &str, auto: bool) -> ConsoleEntry {
        ConsoleEntry {
            seq: 1,
            time: 0.0,
            kind,
            text: text.to_string(),
            auto,
            line: None,
            stream: false,
        }
    }

    fn shown(
        kind: ConsoleKind,
        text: &str,
        auto: bool,
        log_all: bool,
        quiet: bool,
    ) -> Option<String> {
        console_line(&entry(kind, text, auto), log_all, quiet).map(|l| l.text())
    }

    #[test]
    fn what_is_echoed() {
        use ConsoleKind::*;
        // (kind, text, auto, log_all, quiet) -> shown
        type Case<'a> = (ConsoleKind, &'a str, bool, bool, bool, Option<&'a str>);
        let cases: &[Case] = &[
            (
                Received,
                "echo:Unknown command",
                false,
                false,
                false,
                Some("echo:Unknown command"),
            ),
            (Received, "ok", false, false, false, None),
            (Received, "ok N12 P15 B3", false, false, false, None),
            (
                Received,
                "ok T:20.0 /0.0",
                false,
                false,
                false,
                Some("ok T:20.0 /0.0"),
            ),
            (Received, "T:20.0 /0.0 B:20 /0", true, false, false, None),
            (Received, "echo:busy: processing", false, false, false, None),
            (Received, "wait", false, false, false, None),
            (
                Received,
                "//action:prompt_begin Hi",
                false,
                false,
                false,
                None,
            ),
            (Received, "echo:  M92 X80", false, false, true, None),
            (
                Received,
                "Error:Printer halted",
                false,
                false,
                true,
                Some("Error:Printer halted"),
            ),
            (Received, "!! Kill", true, false, false, Some("!! Kill")),
            (Sent, "G28", false, false, false, None),
            (
                Info,
                "Connecting to Virtual printer",
                false,
                false,
                true,
                Some("Connecting to Virtual printer"),
            ),
            (
                Warning,
                "Resend: 12",
                false,
                false,
                false,
                Some("Resend: 12"),
            ),
            (
                Error,
                "Lost the link",
                false,
                false,
                false,
                Some("Lost the link"),
            ),
            // log on: everything.
            (Sent, "G28", false, true, false, Some("> G28")),
            (Received, "ok", false, true, false, Some("ok")),
            (
                Received,
                "T:20.0 /0.0",
                true,
                true,
                true,
                Some("T:20.0 /0.0"),
            ),
        ];
        for &(kind, text, auto, log_all, quiet, want) in cases {
            assert_eq!(
                shown(kind, text, auto, log_all, quiet).as_deref(),
                want,
                "{kind:?} {text:?} auto={auto} log={log_all} quiet={quiet}"
            );
        }
    }

    #[test]
    fn bare_oks() {
        for ok in ["ok", " ok ", "ok N1", "ok P15 B3"] {
            assert!(is_bare_ok(ok), "{ok}");
        }
        for not in ["okay", "ok T:20", "ok X", "echo:ok", ""] {
            assert!(!is_bare_ok(not), "{not}");
        }
    }

    #[test]
    fn waits_end_after_the_answer_and_an_idle_link() {
        let (done, _rx) = crossbeam_channel::bounded(1);
        let start = Instant::now();
        let mut expect = Expect {
            what: Awaited::Settings,
            idle_completes: false,
            since: start,
            answered: None,
            idle_since: None,
            done,
        };
        let at = |ms: u64| start + Duration::from_millis(ms);
        // Idle but unanswered: not yet.
        assert!(!expect.finished(true, at(1000)));
        expect.answered = Some(at(1000));
        assert!(!expect.finished(true, at(1100)));
        assert!(expect.finished(true, at(1100) + IDLE_SETTLE));
        // Answered while the link stays busy: after a grace period.
        expect.idle_since = None;
        assert!(!expect.finished(false, at(1500)));
        assert!(expect.finished(false, at(1000) + ANSWER_GRACE));
        // Idle completion needs no answer, but not right after the request.
        expect.answered = None;
        expect.idle_completes = true;
        expect.idle_since = None;
        assert!(!expect.finished(true, at(10)));
        assert!(!expect.finished(true, at(300)));
        assert!(expect.finished(true, at(300) + IDLE_SETTLE));
        // A connect never ends by itself.
        expect.what = Awaited::Connect;
        assert!(!expect.finished(true, at(100_000)));
    }

    #[test]
    fn prompts_name_their_answers() {
        let prompt = HostPrompt {
            message: "Nozzle parked".into(),
            buttons: vec!["Continue".into(), "Purge".into()],
        };
        assert_eq!(
            prompt_line(&prompt).text(),
            "The printer asks: Nozzle parked  (answer M876 S0 = Continue, M876 S1 = Purge)"
        );
        let bare = HostPrompt {
            message: "Paused".into(),
            buttons: vec![],
        };
        assert!(prompt_line(&bare).text().ends_with("(M876 S0 continues)"));
    }

    #[test]
    fn answers_match_events() {
        assert!(Awaited::Settings.answered_by(&Event::SettingsUpdated));
        assert!(!Awaited::Firmware.answered_by(&Event::SettingsUpdated));
        assert!(!Awaited::Idle.answered_by(&Event::SettingsUpdated));
        assert!(!Awaited::Connect.answered_by(&Event::Stats(Default::default())));
    }
}
