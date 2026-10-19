//! `sprint print`: stream a file to a printer without a UI.
//!
//! The file is loaded and analyzed first (so progress, ETA and layers are
//! known), then Sprint connects, waits for the printer's setup and starts
//! the job. A progress line (percent, layer, ETA, temperatures) goes to
//! stderr: redrawn in place on a terminal, a line per 10% otherwise. The
//! first Ctrl+C cancels the job (the cancel script runs); a second one
//! stops waiting for the cancel to finish, but the heaters are still
//! turned off before the link closes. Exit status: 0 when the print
//! completed, 1 when it failed or was cancelled by the printer, 130 after
//! Ctrl+C.
//!
//! The printer's questions (a filament change, M0 "insert magnets") and
//! messages are printed as they come; see [`crate::prompts`] for how they
//! are answered.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use sprint_gcode::{ResumeOptions, resume_preamble};
use sprint_host::{Event, JobStatus, PrintOptions, Printer, PrinterProfile};
use sprint_tui::actions::{PausePoint, layers_text, pause_points};
use sprint_tui::format;
use sprint_tui::interrupt::{self, Interrupt};
use sprint_tui::loader::LoadedFile;
use sprint_tui::report;
use sprint_tui::session;

use crate::analyze;
use crate::cli::{PrintArgs, TargetArg};
use crate::headless::{self, Connected, EXIT_INTERRUPTED};
use crate::prompts::{self, Effect, PromptWatch, Question};

/// How often the job is looked at.
const POLL: Duration = Duration::from_millis(100);
/// How often the progress line is redrawn on a terminal.
const REDRAW: Duration = Duration::from_millis(250);
/// How long a cancel may take before Sprint stops waiting.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the printer may keep a print paused, idle and without a
/// question before Sprint offers to resume it.
const PAUSE_GRACE: Duration = Duration::from_secs(3);

/// Run `sprint print`.
pub fn run(args: &PrintArgs) -> Result<ExitCode> {
    let (config, profile) = headless::load_profile(args.profile.as_deref())?;
    let target = args.connect.resolve(&profile);
    let interrupt = interrupt::global();
    let stderr_tty = io::stderr().is_terminal();
    let show_progress = !args.no_progress;

    let loaded = match load(
        &args.file,
        &profile,
        show_progress && stderr_tty,
        &interrupt,
    ) {
        Ok(loaded) => loaded,
        // Ctrl+C during the analysis.
        Err(_) if interrupt.is_raised() => {
            eprintln!("Interrupted");
            return Ok(ExitCode::from(EXIT_INTERRUPTED));
        }
        Err(e) => return Err(e),
    };
    let options = print_options(args, &loaded, &profile)?;
    let pauses = pause_points(&loaded, &args.pause_at_layer, options.start_line)
        .map_err(anyhow::Error::msg)?;

    let printer = Printer::with_console_capacity(config.console_max_lines);
    printer.set_profile(profile.clone());
    let label = target.label();
    if show_progress {
        eprintln!("Connecting to {label}…");
    }
    let connected = headless::connect(&printer, target, profile.options.clone(), &interrupt)?;
    if connected == Connected::Interrupted {
        eprintln!("Interrupted");
        return Ok(ExitCode::from(EXIT_INTERRUPTED));
    }
    printer
        .start_print(loaded.file.clone(), options)
        .map_err(|e| anyhow!("cannot print {}: {e}", loaded.name()))?;
    if !pauses.is_empty() {
        let lines: Vec<usize> = pauses.iter().map(|p| p.line).collect();
        printer.pause_at_lines(&lines);
    }
    if show_progress {
        let stats = &loaded.toolpath.stats;
        eprintln!(
            "Printing {} on {label}: {} layers, {} estimated (Ctrl+C cancels)",
            loaded.name(),
            loaded.toolpath.layers.len(),
            format::duration(f64::from(stats.estimated_time))
        );
        if !pauses.is_empty() {
            eprintln!(
                "Pausing before layer {} (Enter resumes)",
                layers_text(&pauses)
            );
        }
    }

    let mut progress = Progress::new(show_progress, stderr_tty);
    let mut prompts = PromptWatch::new(args.answer_prompts);
    let outcome = follow_or_close(&printer, &interrupt, &mut prompts, &mut progress, &pauses);
    progress.finish();
    let heaters = stop_safely(&printer, &outcome, &mut |text| eprintln!("{text}"));
    printer.disconnect();

    let name = loaded.name();
    let resume = outcome
        .stopped_at
        .map(|line| format!("\nresume with: {}", resume_command(args, line)))
        .unwrap_or_default();
    let (status, elapsed, percent) = outcome.last;
    if outcome.abandoned {
        let heaters = match heaters {
            Some(true) => "the heaters are off",
            _ => "the heaters may still be on: check the printer",
        };
        eprintln!("Abandoned {name} at {percent}: the cancel did not finish ({heaters}){resume}");
        return Ok(ExitCode::from(EXIT_INTERRUPTED));
    }
    match status {
        JobStatus::Completed => {
            let mut out = io::stdout().lock();
            let _ = writeln!(
                out,
                "Completed {name}: {percent} in {}",
                format::clock_of(elapsed)
            );
            Ok(ExitCode::SUCCESS)
        }
        JobStatus::Cancelled if outcome.cancel_sent => {
            eprintln!("Cancelled {name} at {percent}{resume}");
            Ok(ExitCode::from(EXIT_INTERRUPTED))
        }
        JobStatus::Failed(why) => {
            let heaters = match heaters {
                Some(true) => "; the heaters are off",
                Some(false) => "; the heaters may still be on: check the printer",
                None => "",
            };
            Err(anyhow!(
                "{name} failed at {percent}: {why}{heaters}{resume}"
            ))
        }
        JobStatus::Cancelled => Err(anyhow!("the printer cancelled {name} at {percent}{resume}")),
        other => Err(anyhow!(
            "{name} stopped at {percent} ({}){resume}",
            other.label()
        )),
    }
}

/// The print's options: the profile's (its `emit_progress` included, see
/// [`sprint_tui::actions::print_options`]), M73 progress also with
/// `--progress-on-printer`, and with `--start-line` the line to start from
/// and the resume preamble.
fn print_options(
    args: &PrintArgs,
    loaded: &LoadedFile,
    profile: &PrinterProfile,
) -> Result<PrintOptions> {
    let mut options = sprint_tui::actions::print_options(loaded, profile);
    options.emit_progress |= args.progress_on_printer;
    if let Some(line) = args.start_line {
        let total = loaded.file.len();
        let index = usize::try_from(line - 1).unwrap_or(usize::MAX);
        if index >= total {
            bail!(
                "--start-line {line} is past the end of {} ({total} lines)",
                loaded.name()
            );
        }
        options.start_line = index;
        options.preamble = resume_preamble(&loaded.file, index, &resume_options(args));
    }
    Ok(options)
}

/// The command that resumes this print from `stopped_at` (the 0-based
/// first line the printer did not acknowledge): the same file, printer
/// and profile, with `--start-line`.
fn resume_command(args: &PrintArgs, stopped_at: usize) -> String {
    let mut command = format!(
        "sprint print {} --start-line {}",
        shell_word(&args.file.display().to_string()),
        stopped_at.saturating_add(1)
    );
    let target = match &args.connect {
        TargetArg::Target(target) => target.to_string(),
        TargetArg::Profile => "profile".to_string(),
    };
    command.push_str(&format!(" --connect {}", shell_word(&target)));
    if let Some(profile) = &args.profile {
        command.push_str(&format!(" --profile {}", shell_word(profile)));
    }
    command
}

/// `word` as one argument for a shell: quoted when it holds anything but
/// letters, digits and `_-./:\@=,+`.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:\\@=,+".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("\"{}\"", word.replace('"', "\\\""))
    }
}

/// Before the link closes: when the job failed (a line the firmware kept
/// rejecting, a resend it could not serve) or Sprint stopped waiting for
/// a cancel ([`Outcome::abandoned`]), no cancel script has turned the
/// heaters off, so they are switched off here (see
/// [`session::heaters_off`]; harmless when the engine did it already).
/// `say` gets what is going on. Returns whether the printer reported them
/// off, or `None` when nothing was needed (or the link is gone, or the
/// printer halted, which switches them off itself).
fn stop_safely(printer: &Printer, outcome: &Outcome, say: &mut dyn FnMut(&str)) -> Option<bool> {
    let failed = matches!(outcome.last.0, JobStatus::Failed(_));
    if !outcome.abandoned && !failed {
        return None;
    }
    let reachable = printer.with_state(|s| s.is_connected() && !s.halted);
    if !reachable {
        return None;
    }
    say("Turning the heaters off before disconnecting…");
    Some(session::heaters_off(printer, session::HEATERS_OFF_WAIT))
}

/// How `--start-line` resumes: `--board-reset` declares the nozzle's Z
/// (the firmware lost it), `--z-clearance` sets the lift and
/// `--no-wait-temps` skips the heat waits.
fn resume_options(args: &PrintArgs) -> ResumeOptions {
    let defaults = ResumeOptions::default();
    ResumeOptions {
        restore_z_position: args.board_reset,
        z_clearance: args.z_clearance.unwrap_or(defaults.z_clearance),
        wait_for_temps: !args.no_wait_temps,
        ..defaults
    }
}

/// Load and analyze the file (progress on stderr when `progress`). A
/// file that is not G-code (a 3D model, an archive, binary data) is
/// refused before anything connects: it must never be streamed.
fn load(
    path: &Path,
    profile: &PrinterProfile,
    progress: bool,
    interrupt: &Interrupt,
) -> Result<LoadedFile> {
    let file = analyze::load(path)?;
    sprint_tui::loader::check_content(path, &file).map_err(anyhow::Error::msg)?;
    let options = sprint_tui::loader::analyze_options(profile);
    let started = Instant::now();
    let stderr = io::stderr();
    let mut bar = analyze::Progress::new(progress.then_some(stderr), file.name());
    let toolpath = sprint_gcode::analyze(&file, &options, &mut |fraction| {
        bar.update(fraction);
        !interrupt.is_raised()
    });
    bar.finish();
    let toolpath = toolpath.context("the analysis was cancelled")?;
    Ok(LoadedFile::new(
        path.to_path_buf(),
        file,
        toolpath,
        started.elapsed(),
    ))
}

/// What the job looked like at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub status: JobStatus,
    /// 0..=1.
    pub progress: f32,
    /// [`report::monitor_text`] of the printer.
    pub text: String,
}

/// How following a job ended.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The last status, the elapsed time and the progress ("42.0%").
    pub last: (JobStatus, Duration, String),
    /// Ctrl+C asked the printer to cancel the job.
    pub cancel_sent: bool,
    /// Sprint stopped waiting before the job ended (a second Ctrl+C, or a
    /// cancel that took too long): the cancel may not have finished.
    pub abandoned: bool,
    /// A failed or cancelled host print: the 0-based first line the
    /// printer did not acknowledge, where a recovery starts
    /// ([`sprint_host::JobInfo::stopped_at`]).
    pub stopped_at: Option<usize>,
}

/// Where [`follow`] reports.
pub trait Report {
    /// The job now (called on every look at it).
    fn show(&mut self, view: &JobView);
    /// Something to tell the user: a question, a message from the printer.
    fn say(&mut self, text: &str);
}

/// Follow the job until it ends. The first Ctrl+C cancels it, however
/// quickly a second one follows; a second one (or a cancel that takes too
/// long) then stops waiting for the cancel to finish. The printer's
/// messages and questions are reported as they come, and the questions
/// are answered as `prompts` says; a pause at one of `pauses` (the
/// `--pause-at-layer` points) waits for Enter.
pub fn follow(
    printer: &Printer,
    interrupt: &Interrupt,
    prompts: &mut PromptWatch,
    report: &mut dyn Report,
    pauses: &[PausePoint],
) -> Outcome {
    let events = printer.subscribe();
    let mut last_message: Option<String> = None;
    let mut cancelled_at: Option<Instant> = None;
    // Since when the printer has paused the print without asking anything
    // and without working on something (parking, unloading filament).
    let mut quiet_pause: Option<Instant> = None;
    // The pause points still ahead at the last look, and the layer of the
    // last one the job passed: the pause that follows is the one asked for.
    let mut points_ahead: Vec<usize> = pauses.iter().map(|p| p.line).collect();
    let mut paused_at_layer: Option<u64> = None;
    loop {
        let (view, elapsed, prompt, waiting, busy, ahead, stopped_at) = printer.with_state(|s| {
            let job = s.job.as_ref();
            (
                JobView {
                    status: job.map_or(JobStatus::Failed("no job".into()), |j| j.status.clone()),
                    progress: job.map_or(0.0, |j| j.progress),
                    text: report::monitor_text(s),
                },
                job.map(|j| j.elapsed).unwrap_or_default(),
                s.prompt.clone(),
                sprint_tui::actions::waiting_for_user(s),
                s.busy.is_some(),
                job.map(|j| j.pause_at.clone()).unwrap_or_default(),
                job.and_then(|j| j.stopped_at),
            )
        });
        for event in events.try_iter() {
            // The same message again and again (a runout warning) once.
            if let Event::FirmwareMessage(text) = event
                && last_message.as_ref() != Some(&text)
            {
                report.say(&format!("Printer: {}", prompts::printable(&text)));
                last_message = Some(text);
            }
        }
        for passed in points_ahead.iter().filter(|line| !ahead.contains(line)) {
            paused_at_layer = pauses.iter().find(|p| p.line == *passed).map(|p| p.layer);
        }
        points_ahead = ahead;
        report.show(&view);
        let percent = format::percent(f64::from(view.progress));
        if view.status.is_finished() {
            return Outcome {
                last: (view.status, elapsed, percent),
                cancel_sent: cancelled_at.is_some(),
                abandoned: false,
                stopped_at,
            };
        }
        let presses = interrupt.count();
        // Any press cancels first: two presses between two polls must not
        // skip the cancel (and its script, which turns the heaters off).
        if presses >= 1 && cancelled_at.is_none() {
            printer.cancel();
            cancelled_at = Some(Instant::now());
            report.say("Cancelling: the cancel script runs (Ctrl+C again stops waiting for it)…");
        }
        let gave_up = cancelled_at.is_some_and(|t| t.elapsed() >= CANCEL_TIMEOUT);
        if presses >= 2 || gave_up {
            return Outcome {
                last: (view.status, elapsed, percent),
                cancel_sent: true,
                abandoned: true,
                stopped_at,
            };
        }
        let paused = view.status == JobStatus::Paused;
        let quiet = paused && prompt.is_none() && !busy;
        quiet_pause = quiet.then(|| quiet_pause.unwrap_or_else(Instant::now));
        if cancelled_at.is_none() {
            let question = match &prompt {
                Some(prompt) => Some(Question::Prompt(prompt)),
                None if waiting => Some(Question::WaitingForUser),
                // The pause asked for with --pause-at-layer.
                None if paused && paused_at_layer.is_some() => {
                    paused_at_layer.map(Question::PausedAtLayer)
                }
                // A filament change pauses first and asks once parked.
                None if quiet_pause.is_some_and(|t| t.elapsed() >= PAUSE_GRACE) => {
                    Some(Question::Paused)
                }
                None => None,
            };
            for effect in prompts.update(question) {
                match effect {
                    Effect::Say(text) => report.say(&text),
                    Effect::Answer(button) => printer.answer_prompt(button),
                    // Resuming a print the printer paused also releases a
                    // firmware waiting for the user.
                    Effect::Continue if paused => {
                        paused_at_layer = None;
                        printer.resume();
                    }
                    Effect::Continue => printer.send("M108"),
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

/// [`follow`], except that a panic (a bug) does not leave the printer
/// heating with nobody watching: the job is cancelled (its cancel script
/// turns the heaters off; they are switched off here should the cancel
/// not finish) and the link closed before the panic goes on.
pub fn follow_or_close(
    printer: &Printer,
    interrupt: &Interrupt,
    prompts: &mut PromptWatch,
    report: &mut dyn Report,
    pauses: &[PausePoint],
) -> Outcome {
    let followed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        follow(printer, interrupt, prompts, report, pauses)
    }));
    match followed {
        Ok(outcome) => outcome,
        Err(panic) => {
            let _ = session::cancel_before_closing(printer, session::CANCEL_WAIT, |name| {
                eprintln!("Cancelling {name} before closing the connection…");
            });
            if printer.with_state(sprint_host::PrinterState::job_active) {
                let _ = session::heaters_off(printer, session::HEATERS_OFF_WAIT);
            }
            printer.disconnect();
            std::panic::resume_unwind(panic)
        }
    }
}

/// The progress line on stderr.
struct Progress {
    enabled: bool,
    terminal: bool,
    last_draw: Option<Instant>,
    /// Width of the line on screen (terminal).
    drawn: usize,
    /// Last 10% step printed (not a terminal).
    last_step: Option<u32>,
    last_status: Option<JobStatus>,
}

impl Progress {
    fn new(enabled: bool, terminal: bool) -> Progress {
        Progress {
            enabled,
            terminal,
            last_draw: None,
            drawn: 0,
            last_step: None,
            last_status: None,
        }
    }

    /// Take the line drawn in place off the screen; it is drawn again
    /// at once on the next look at the job.
    fn clear(&mut self, err: &mut impl Write) {
        if self.terminal && self.drawn > 0 {
            let _ = write!(err, "\r{}\r", " ".repeat(self.drawn));
            self.drawn = 0;
            self.last_draw = None;
        }
    }

    fn finish(&mut self) {
        if self.enabled && self.terminal && self.drawn > 0 {
            let _ = writeln!(io::stderr());
            self.drawn = 0;
        }
    }
}

impl Report for Progress {
    fn show(&mut self, view: &JobView) {
        if !self.enabled {
            return;
        }
        let status_changed = self.last_status.as_ref() != Some(&view.status);
        self.last_status = Some(view.status.clone());
        let mut err = io::stderr().lock();
        if self.terminal {
            let due = self.last_draw.is_none_or(|t| t.elapsed() >= REDRAW);
            if !(due || status_changed || view.status.is_finished()) {
                return;
            }
            self.last_draw = Some(Instant::now());
            let text = fit(&view.text, terminal_width().saturating_sub(1));
            let width = unicode_width::UnicodeWidthStr::width(text.as_str());
            let pad = " ".repeat(self.drawn.saturating_sub(width));
            let _ = write!(err, "\r{text}{pad}").and_then(|()| err.flush());
            self.drawn = width;
        } else {
            // In 0..=10 for progress in 0..=1.
            let step = (view.progress.clamp(0.0, 1.0) * 10.0).floor() as u32;
            if self.last_step != Some(step) || status_changed || view.status.is_finished() {
                self.last_step = Some(step);
                let _ = writeln!(err, "{}", view.text);
            }
        }
    }

    /// Questions and messages are printed even with `--no-progress`: the
    /// user may have to answer them.
    fn say(&mut self, text: &str) {
        let mut err = io::stderr().lock();
        self.clear(&mut err);
        let _ = writeln!(err, "{text}");
    }
}

/// The terminal's width in columns (80 when unknown).
fn terminal_width() -> usize {
    crossterm::terminal::size().map_or(80, |(w, _)| usize::from(w).max(20))
}

/// `text` cut to `max` columns.
fn fit(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > max {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::AnswerPrompts;
    use sprint_host::sprint_sim::SimConfig;
    use sprint_host::{ConnectOptions, ConnectTarget};
    use sprint_tui::session;
    use std::sync::Arc;

    fn connect(sim: SimConfig) -> Printer {
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        session::connect_blocking(
            &printer,
            ConnectTarget::Simulator(sim),
            ConnectOptions::default(),
            &interrupt,
        )
        .expect("connect to the simulator");
        session::wait_setup(&printer, Duration::from_secs(30), &interrupt).expect("setup");
        printer
    }

    /// What [`follow`] reported.
    #[derive(Default)]
    struct Seen {
        views: Vec<JobView>,
        said: Vec<String>,
    }

    impl Report for Seen {
        fn show(&mut self, view: &JobView) {
            self.views.push(view.clone());
        }

        fn say(&mut self, text: &str) {
            self.said.push(text.to_string());
        }
    }

    /// Follow the job, answering the printer's questions as `policy`
    /// says (nothing is typed).
    fn watch(printer: &Printer, interrupt: &Interrupt, policy: AnswerPrompts) -> (Outcome, Seen) {
        let (_typing, typed) = crossbeam_channel::unbounded();
        let mut prompts = PromptWatch::with_answers(policy, typed, true);
        follow_at_most(printer, interrupt, &mut prompts, &[])
    }

    /// [`follow`], with a double Ctrl+C after a minute so that a job
    /// that never ends fails the test instead of hanging it.
    fn follow_at_most(
        printer: &Printer,
        interrupt: &Interrupt,
        prompts: &mut PromptWatch,
        pauses: &[PausePoint],
    ) -> (Outcome, Seen) {
        let (done, finished) = crossbeam_channel::bounded::<()>(0);
        let watchdog = interrupt.clone();
        let thread = std::thread::spawn(move || {
            if finished.recv_timeout(Duration::from_secs(60))
                == Err(crossbeam_channel::RecvTimeoutError::Timeout)
            {
                watchdog.raise();
                watchdog.raise();
            }
        });
        let mut seen = Seen::default();
        let outcome = follow(printer, interrupt, prompts, &mut seen, pauses);
        drop(done);
        let _ = thread.join();
        (outcome, seen)
    }

    /// Many short real-time moves: still printing while Ctrl+C arrives.
    fn slow_job() -> Arc<sprint_gcode::GcodeFile> {
        let mut text = String::from("G28\nG90\nM83\nG1 Z0.3 F600\n");
        for i in 0..400 {
            text.push_str(&format!("G1 X{} Y20 E0.2 F600\n", 20 + (i % 2) * 5));
        }
        Arc::new(sprint_gcode::GcodeFile::from_text("slow.gcode", &text))
    }

    #[test]
    fn a_print_is_followed_to_completion() {
        let printer = connect(SimConfig {
            boot_delay_ms: 20,
            time_scale: 100.0,
            instant_moves: true,
            ..SimConfig::default()
        });
        printer
            .start_print(slow_job(), Default::default())
            .expect("start");
        let (outcome, seen) = watch(&printer, &Interrupt::new(), AnswerPrompts::Ask);
        assert_eq!(outcome.last.0, JobStatus::Completed);
        assert_eq!(outcome.last.2, "100.0%");
        assert!(!outcome.cancel_sent && !outcome.abandoned);
        // Nothing to make safe after a completed print.
        assert_eq!(stop_safely(&printer, &outcome, &mut |_| {}), None);
        assert!(
            seen.views
                .last()
                .is_some_and(|v| v.text.starts_with("Completed 100.0%"))
        );
        printer.disconnect();
    }

    #[test]
    fn ctrl_c_cancels_then_stops_waiting() {
        let printer = connect(SimConfig {
            boot_delay_ms: 20,
            time_scale: 5.0,
            instant_moves: false,
            ..SimConfig::default()
        });
        printer
            .start_print(slow_job(), Default::default())
            .expect("start");
        let interrupt = Interrupt::new();
        let raiser = interrupt.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            raiser.raise();
        });
        let (outcome, _) = watch(&printer, &interrupt, AnswerPrompts::Ask);
        let _ = thread.join();
        assert!(outcome.cancel_sent);
        assert!(!outcome.abandoned);
        assert_eq!(outcome.last.0, JobStatus::Cancelled);
        assert!(outcome.stopped_at.is_some(), "{outcome:?}");
        printer.disconnect();
    }

    /// Heats, then many short real-time moves.
    fn hot_job() -> Arc<sprint_gcode::GcodeFile> {
        let mut text =
            String::from("M104 S200\nM140 S60\nM106 S255\nG28\nG90\nM83\nG1 Z0.3 F600\n");
        for i in 0..400 {
            text.push_str(&format!("G1 X{} Y20 E0.2 F600\n", 20 + (i % 2) * 5));
        }
        Arc::new(sprint_gcode::GcodeFile::from_text("hot.gcode", &text))
    }

    #[test]
    fn a_quick_double_ctrl_c_still_cancels_and_turns_the_heaters_off() {
        let printer = connect(SimConfig {
            boot_delay_ms: 20,
            time_scale: 5.0,
            instant_moves: false,
            ..SimConfig::default()
        });
        let control = printer.sim_control().expect("simulator");
        printer
            .start_print(hot_job(), Default::default())
            .expect("start");
        let patience = Duration::from_secs(30);
        session::wait_for(&printer, patience, &Interrupt::new(), true, |s| {
            s.job.as_ref().is_some_and(|j| j.current_line > 8)
        })
        .expect("printing");
        // Both presses land before the first look at the job.
        let interrupt = Interrupt::new();
        interrupt.raise();
        interrupt.raise();
        let (outcome, _) = watch(&printer, &interrupt, AnswerPrompts::Ask);
        assert!(outcome.cancel_sent && outcome.abandoned, "{outcome:?}");
        // The cancel reached the printer all the same.
        session::wait_for(&printer, patience, &Interrupt::new(), true, |s| {
            s.job
                .as_ref()
                .is_some_and(|j| matches!(j.status, JobStatus::Cancelling | JobStatus::Cancelled))
        })
        .expect("the job is cancelled");
        // Not waiting for the cancel script: the heaters go off anyway
        // before the link closes.
        let mut said = Vec::new();
        let off = stop_safely(&printer, &outcome, &mut |t| said.push(t.to_string()));
        assert_eq!(off, Some(true), "{said:?}");
        let sim = control.snapshot();
        assert_eq!(sim.hotend_targets, [0.0]);
        assert_eq!(sim.bed_target, 0.0);
        printer.disconnect();
    }

    #[test]
    fn a_failed_print_leaves_the_heaters_off() {
        let printer = connect(SimConfig {
            boot_delay_ms: 20,
            time_scale: 100.0,
            instant_moves: true,
            ..SimConfig::default()
        });
        let control = printer.sim_control().expect("simulator");
        // A line longer than the firmware's line buffer is rejected again
        // and again until the engine gives up on the job, while the link
        // stays usable.
        let mut text = String::from("M104 S200\nM140 S60\nG28\nG90\n");
        for i in 0..50 {
            text.push_str(&format!("G1 X{} Y20 F3000\n", 20 + i % 2));
        }
        text.push_str(&format!("M117 {}\n", "x".repeat(600)));
        for i in 0..50 {
            text.push_str(&format!("G1 X{} Y30 F3000\n", 20 + i % 2));
        }
        let file = Arc::new(sprint_gcode::GcodeFile::from_text("long.gcode", &text));
        printer
            .start_print(file, Default::default())
            .expect("start");
        let (outcome, _) = watch(&printer, &Interrupt::new(), AnswerPrompts::Ask);
        assert!(
            matches!(outcome.last.0, JobStatus::Failed(_)),
            "{outcome:?}"
        );
        assert!(!outcome.cancel_sent && !outcome.abandoned);
        // The line it stopped at: the long M117 (line 55, 0-based 54).
        assert_eq!(outcome.stopped_at, Some(54), "{outcome:?}");
        let mut said = Vec::new();
        let off = stop_safely(&printer, &outcome, &mut |t| said.push(t.to_string()));
        assert_eq!(off, Some(true), "{said:?}");
        assert!(said.iter().any(|t| t.contains("heaters off")), "{said:?}");
        let sim = control.snapshot();
        assert_eq!(sim.hotend_targets, [0.0]);
        assert_eq!(sim.bed_target, 0.0);
        printer.disconnect();
        // Once the link is gone there is nothing left to do.
        assert_eq!(stop_safely(&printer, &outcome, &mut |_| {}), None);
    }

    /// Heats, then changes the filament (M600) halfway.
    fn filament_change() -> Arc<sprint_gcode::GcodeFile> {
        let text = "G28\nM109 S200\nG90\nG1 X20 Y20 F3000\nG1 X30 Y20 F3000\nM600\n\
                    G1 X40 Y20 F3000\nM104 S0\n";
        Arc::new(sprint_gcode::GcodeFile::from_text("m600.gcode", text))
    }

    fn fast_sim() -> SimConfig {
        SimConfig {
            boot_delay_ms: 20,
            time_scale: 100.0,
            instant_moves: true,
            ..SimConfig::default()
        }
    }

    #[test]
    fn a_filament_change_is_answered_with_continue() {
        let printer = connect(fast_sim());
        printer
            .start_print(filament_change(), Default::default())
            .expect("start");
        let (outcome, seen) = watch(&printer, &Interrupt::new(), AnswerPrompts::Continue);
        assert_eq!(outcome.last.0, JobStatus::Completed, "{:?}", seen.said);
        let said = seen.said.join("\n");
        assert!(said.contains("The printer asks:"), "{said}");
        assert!(said.contains("Answered Continue"), "{said}");
        printer.disconnect();
    }

    #[test]
    fn a_filament_change_is_shown_and_answered_from_the_input() {
        let printer = connect(fast_sim());
        printer
            .start_print(filament_change(), Default::default())
            .expect("start");
        let (typing, typed) = crossbeam_channel::unbounded();
        let mut prompts = PromptWatch::with_answers(AnswerPrompts::Ask, typed, true);
        let asked = printer.clone();
        // Enter, once the question is up (and has been seen by follow).
        let user = std::thread::spawn(move || {
            let shown = session::wait_for(
                &asked,
                Duration::from_secs(30),
                &Interrupt::new(),
                true,
                |s| s.prompt.is_some(),
            );
            std::thread::sleep(POLL * 5);
            let _ = typing.send(String::new());
            shown.is_ok()
        });
        let (outcome, seen) = follow_at_most(&printer, &Interrupt::new(), &mut prompts, &[]);
        assert!(user.join().unwrap_or(false), "no prompt");
        assert_eq!(outcome.last.0, JobStatus::Completed, "{:?}", seen.said);
        let said = seen.said.join("\n");
        assert!(said.contains("The printer asks:"), "{said}");
        assert!(said.contains("[1] "), "{said}");
        assert!(said.contains("answer at the printer"), "{said}");
        assert!(said.contains("Answered "), "{said}");
        // The progress line said what the printer waited for.
        assert!(
            seen.views
                .iter()
                .any(|v| v.text.contains("waiting for the user")),
            "{:?}",
            seen.views.last()
        );
        printer.disconnect();
    }

    /// Breaks once the job is under way (a bug in the progress line).
    struct Breaks;

    impl Report for Breaks {
        fn show(&mut self, view: &JobView) {
            if view.progress > 0.02 {
                panic!("the progress line broke");
            }
        }

        fn say(&mut self, _text: &str) {}
    }

    #[test]
    fn a_panic_while_following_still_cancels_the_print_before_closing() {
        let printer = connect(SimConfig {
            boot_delay_ms: 20,
            time_scale: 5.0,
            instant_moves: false,
            ..SimConfig::default()
        });
        let control = printer.sim_control().expect("simulator");
        printer
            .start_print(hot_job(), Default::default())
            .expect("start");
        let (_typing, typed) = crossbeam_channel::unbounded();
        let mut prompts = PromptWatch::with_answers(AnswerPrompts::Ask, typed, true);
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            follow_or_close(&printer, &Interrupt::new(), &mut prompts, &mut Breaks, &[])
        }));
        assert!(ended.is_err(), "the panic goes on");
        assert_eq!(
            printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
            Some(JobStatus::Cancelled)
        );
        assert!(!printer.with_state(|s| s.status.is_open()));
        // The cancel script turned the heaters off before the link closed.
        let got = control.received_commands();
        assert!(got.iter().any(|c| c == "M104 S0"), "{got:?}");
        assert!(got.iter().any(|c| c == "M140 S0"), "{got:?}");
    }

    fn print_args(extra: &[&str]) -> PrintArgs {
        use clap::Parser;
        let mut argv = vec!["sprint", "print", "a.gcode", "-c", "sim"];
        argv.extend_from_slice(extra);
        match crate::cli::Cli::try_parse_from(argv).map(crate::cli::Cli::into_command) {
            Ok(crate::cli::Command::Print(args)) => args,
            other => panic!("not a print: {other:?}"),
        }
    }

    #[test]
    fn a_board_reset_resume_declares_the_z_position() {
        let file = sprint_gcode::GcodeFile::from_text(
            "part.gcode",
            "M104 S200\nM140 S60\nG28\nG90\nG1 Z0.2 F600\nG1 X10 Y10 E1\nG1 Z5.4\nG1 X20 Y20 E2\n",
        );
        // Resuming at the last line, Z 5.4.
        let plain = resume_preamble(
            &file,
            7,
            &resume_options(&print_args(&["--start-line", "8"])),
        );
        assert!(!plain.iter().any(|l| l.starts_with("G92 Z")), "{plain:?}");
        assert!(plain.iter().any(|l| l.starts_with("M109")), "{plain:?}");
        let args = print_args(&[
            "--start-line",
            "8",
            "--board-reset",
            "--z-clearance",
            "4",
            "--no-wait-temps",
        ]);
        let options = resume_options(&args);
        assert!(options.restore_z_position);
        assert_eq!(options.z_clearance, 4.0);
        assert!(!options.wait_for_temps);
        let reset = resume_preamble(&file, 7, &options);
        assert!(reset.iter().any(|l| l == "G92 Z5.4"), "{reset:?}");
        assert!(!reset.iter().any(|l| l.starts_with("M109")), "{reset:?}");
        assert!(reset.iter().any(|l| l.starts_with("M104")), "{reset:?}");
    }

    /// A file with `layers` layers of a few short real-time moves each.
    fn layered(layers: usize) -> sprint_gcode::GcodeFile {
        let mut text = String::from("G28\nG90\nM83\n");
        for layer in 0..layers {
            text.push_str(&format!("G1 Z{:.1} F600\n", 0.2 * (layer + 1) as f64));
            for i in 0..4 {
                text.push_str(&format!("G1 X{} Y20 E0.5 F1200\n", 20 + (i % 2) * 10));
            }
        }
        sprint_gcode::GcodeFile::from_text("layers.gcode", &text)
    }

    fn layered_loaded(layers: usize) -> LoadedFile {
        let file = layered(layers);
        let toolpath =
            sprint_gcode::analyze_simple(&file, &sprint_gcode::AnalyzeOptions::default());
        LoadedFile::new("layers.gcode".into(), file, toolpath, Duration::ZERO)
    }

    #[test]
    fn progress_on_the_printer_comes_from_the_flag_or_the_profile() {
        let loaded = layered_loaded(2);
        let mut profile = PrinterProfile::default();
        let plain = print_options(&print_args(&[]), &loaded, &profile).expect("options");
        assert!(!plain.emit_progress && plain.toolpath.is_some());
        let flag = print_args(&["--progress-on-printer"]);
        assert!(
            print_options(&flag, &loaded, &profile)
                .expect("options")
                .emit_progress
        );
        profile.emit_progress = true;
        assert!(
            print_options(&print_args(&[]), &loaded, &profile)
                .expect("options")
                .emit_progress
        );
        // --start-line: the line and the preamble.
        let resume =
            print_options(&print_args(&["--start-line", "5"]), &loaded, &profile).expect("options");
        assert_eq!(resume.start_line, 4);
        assert!(!resume.preamble.is_empty());
        let past = print_options(&print_args(&["--start-line", "999"]), &loaded, &profile)
            .expect_err("past the end");
        assert!(past.to_string().contains("is past the end"), "{past}");
    }

    #[test]
    fn pause_points_are_layer_starts() {
        let loaded = layered_loaded(4);
        assert_eq!(loaded.toolpath.layers.len(), 4);
        let start = |i: usize| loaded.toolpath.layers[i].lines.start as usize;
        let points = pause_points(&loaded, &[3, 2, 3], 0).expect("points");
        assert_eq!(
            points,
            [
                PausePoint {
                    line: start(1),
                    layer: 2
                },
                PausePoint {
                    line: start(2),
                    layer: 3
                },
            ]
        );
        assert!(pause_points(&loaded, &[], 0).expect("none").is_empty());
        let err = pause_points(&loaded, &[5], 0).expect_err("no layer 5");
        assert_eq!(err, "--pause-at-layer 5: layers.gcode has 4 layers");
        let err = pause_points(&loaded, &[2], start(2)).expect_err("before the start");
        assert!(err.contains("starts before"), "{err}");
    }

    #[test]
    fn the_resume_command_names_the_line_after_the_last_acknowledged() {
        let args = print_args(&["--profile", "Bench 2"]);
        assert_eq!(
            resume_command(&args, 41),
            r#"sprint print a.gcode --start-line 42 --connect sim --profile "Bench 2""#
        );
        let mut args = print_args(&[]);
        args.file = std::path::PathBuf::from("my part.gcode");
        args.connect = crate::cli::TargetArg::Profile;
        assert_eq!(
            resume_command(&args, 0),
            r#"sprint print "my part.gcode" --start-line 1 --connect profile"#
        );
        assert_eq!(shell_word("sim://?a=1&b=2"), r#""sim://?a=1&b=2""#);
        assert_eq!(shell_word(r"C:\x\cube.gcode"), r"C:\x\cube.gcode");
        assert_eq!(shell_word(""), r#""""#);
    }

    #[test]
    fn a_pause_at_a_layer_waits_for_enter() {
        let printer = connect(SimConfig {
            boot_delay_ms: 20,
            time_scale: 20.0,
            instant_moves: false,
            ..SimConfig::default()
        });
        let loaded = layered_loaded(4);
        let points = pause_points(&loaded, &[3], 0).expect("points");
        printer
            .start_print(loaded.file.clone(), Default::default())
            .expect("start");
        printer.pause_at_lines(&[points[0].line]);
        let (typing, typed) = crossbeam_channel::unbounded();
        // --answer-prompts continue does not resume a pause asked for.
        let mut prompts = PromptWatch::with_answers(AnswerPrompts::Continue, typed, false);
        let asked = printer.clone();
        let user = std::thread::spawn(move || {
            let paused = session::wait_for(
                &asked,
                Duration::from_secs(30),
                &Interrupt::new(),
                true,
                |s| {
                    s.job
                        .as_ref()
                        .is_some_and(|j| j.status == JobStatus::Paused)
                },
            );
            std::thread::sleep(POLL * 5);
            let _ = typing.send(String::new());
            paused.is_ok()
        });
        let (outcome, seen) = follow_at_most(&printer, &Interrupt::new(), &mut prompts, &points);
        assert!(user.join().unwrap_or(false), "never paused");
        assert_eq!(outcome.last.0, JobStatus::Completed, "{:?}", seen.said);
        let said = seen.said.join("\n");
        assert!(
            said.contains("Paused before layer 3 (--pause-at-layer)"),
            "{said}"
        );
        assert!(said.contains("Press Enter to resume the print"), "{said}");
        assert!(said.contains("Resuming the print"), "{said}");
        assert!(!said.contains("Paused at the printer"), "{said}");
        printer.disconnect();
    }

    #[test]
    fn progress_lines_without_a_terminal_come_per_ten_percent() {
        let mut progress = Progress::new(true, false);
        let view = |p: f32| JobView {
            status: JobStatus::Printing,
            progress: p,
            text: String::new(),
        };
        progress.show(&view(0.0));
        assert_eq!(progress.last_step, Some(0));
        progress.show(&view(0.05));
        assert_eq!(progress.last_step, Some(0));
        progress.show(&view(0.31));
        assert_eq!(progress.last_step, Some(3));
        assert_eq!(fit("Printing 42.0%", 8), "Printing");
        assert_eq!(fit("ab", 10), "ab");
    }
}
