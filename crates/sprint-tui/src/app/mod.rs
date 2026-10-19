//! The full-screen terminal UI: state, key handling and printer actions.
//! [`ui`] draws it and [`terminal`] runs it on a real terminal.
//!
//! The [`App`] never touches the terminal itself: it takes crossterm
//! events, talks to a [`Printer`] and draws into a ratatui frame, so tests
//! drive it with synthetic keys, a simulator and a `TestBackend`.

mod dialogs;
pub(crate) mod terminal;
#[cfg(test)]
mod tests;
mod ui;
mod view;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use sprint_host::sprint_proto::{gcode_doc, setting_def};
use sprint_host::{
    AppConfig, Axes, Axis, ConnectTarget, ConsoleKind as HostKind, Event, Heater, HostPrompt,
    JobKind, JobStatus, Printer, PrinterProfile, list_ports,
};

use crate::actions;
use crate::commands::{
    self, Command, Eeprom, Input, SdCommand, command_candidates, command_spec, looks_like_gcode,
};
use crate::format;
use crate::loader::{LoadJob, LoadedFile, analyze_options};
use crate::report::{self, ReportLine, Tone};
use crate::session;
use crate::theme::Theme;
use crate::widgets::{
    Candidate, CommandInput, ConsoleKind, ConsoleLine, ConsoleState, InputEvent, LayerFit,
    gcode_candidates,
};

use dialogs::{
    Action, Confirm, ConnectDialog, OpenDialog, Outcome, Pending, PromptDialog, ReportSource,
    ReportView, TempDialog, plain_input,
};
use view::{PrinterView, TempHistory};

/// Jog step sizes (mm); `+`/`-` move through them.
pub(crate) const JOG_STEPS: [f32; 5] = [0.1, 1.0, 10.0, 50.0, 100.0];
/// Default jog step index (10 mm).
const DEFAULT_JOG_STEP: usize = 2;
/// Largest Z jog step (mm).
const MAX_Z_STEP: f32 = 10.0;
/// How long a status message stays.
const TOAST_TIME: Duration = Duration::from_secs(5);
/// Second F8 press must come within this.
const ESTOP_WINDOW: Duration = Duration::from_secs(3);
/// How long a report waits for the printer.
const REPORT_TIMEOUT: Duration = Duration::from_secs(8);
/// How long after letting a firmware waiting for the user go on F6 does
/// not do it again: the wait shows until the printer's next busy message
/// (every two seconds on Marlin), and a second M108 could break the heat
/// wait that follows.
const CONTINUE_SETTLE: Duration = Duration::from_secs(3);
/// How long probing the bed may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(600);
/// A report whose answer may carry no news ends when the link has been
/// idle this long (and the request is at least this old).
const REPORT_IDLE_AFTER: Duration = Duration::from_millis(250);
/// Console entries fetched per call.
const CONSOLE_FETCH: usize = 4096;

/// A `source` file read off the UI thread: its name and command lines,
/// or why it could not be read.
type SourceRead = Result<(String, Vec<String>), String>;

/// Which panel has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    /// The layer view (or statistics).
    Layers,
    /// The console.
    Console,
    /// The input line.
    Input,
}

/// What the input line is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputMode {
    /// G-code sent as typed.
    Gcode,
    /// The command palette (`:`): shell commands and G-code.
    Command,
    /// Console search text.
    Search,
}

/// What the centre panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Center {
    Layers,
    Stats,
}

/// A dialog over the screen.
pub(crate) enum Modal {
    Help { scroll: usize },
    Connect(ConnectDialog),
    Open(OpenDialog),
    Temps(TempDialog),
    Confirm(Confirm),
    Report(ReportView),
    Prompt(PromptDialog),
}

/// A resume waiting for the link to settle (see [`App::resume`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PendingResume {
    /// Since when nothing has been queued or unanswered.
    idle_since: Option<Instant>,
}

/// A transient status message in the hint bar.
#[derive(Clone, Debug)]
pub(crate) struct Toast {
    pub text: String,
    pub tone: Tone,
    pub until: Instant,
}

/// The TUI's state.
pub(crate) struct App {
    pub printer: Printer,
    pub config: AppConfig,
    /// Where to save the configuration (recent files); `None` in tests.
    config_path: Option<PathBuf>,
    pub profile: PrinterProfile,
    pub theme: Theme,
    pub view: PrinterView,
    pub temps: TempHistory,
    events: Receiver<Event>,
    pub file: Option<LoadedFile>,
    pub loader: Option<LoadJob>,
    connecting: Option<Receiver<Result<(), String>>>,
    /// Work running off the UI thread; each ends with a status message
    /// (`Err` for a failure).
    background: Vec<Receiver<Result<String, String>>>,
    pub focus: Focus,
    /// The panel to return to when the input line is left.
    return_focus: Focus,
    pub input_mode: InputMode,
    pub gcode_input: CommandInput,
    pub command_input: CommandInput,
    pub search_input: CommandInput,
    pub search: Option<String>,
    pub center: Center,
    /// Layer chosen by the user; `None` follows the print.
    pub layer_view: Option<usize>,
    pub fit: LayerFit,
    pub show_travel: bool,
    pub stats_scroll: usize,
    pub console: Vec<ConsoleLine>,
    console_seq: u64,
    console_capacity: usize,
    pub console_state: ConsoleState,
    pub show_auto: bool,
    pub modal: Option<Modal>,
    /// Jog mode with this step index.
    pub jog: Option<usize>,
    pub estop_armed: Option<Instant>,
    /// The terminal reports key releases (the Windows console does).
    releases_seen: bool,
    /// F8 was released since it armed the emergency stop.
    estop_released: bool,
    /// A resume waiting for the commands typed while paused.
    pub resume_pending: Option<PendingResume>,
    pub toast: Option<Toast>,
    /// A host prompt the user closed without answering.
    hidden_prompt: Option<HostPrompt>,
    pub quit: bool,
    /// Something changed since the last draw.
    pub dirty: bool,
    /// Where the console and the layer view were last drawn (mouse).
    pub console_area: Rect,
    pub canvas_area: Rect,
    base_dir: PathBuf,
    last_load_progress: u32,
    /// The target of the last connect (for `reset`).
    last_target: Option<ConnectTarget>,
    /// An SD file being deleted: its name and when M30 was sent, until
    /// the printer answers (or [`REPORT_TIMEOUT`] passes).
    pending_sd_delete: Option<(String, Instant)>,
    /// When F6 (or `resume`) last let a firmware waiting for the user go
    /// on.
    continued_at: Option<Instant>,
    /// A `source` file being read: its name and lines.
    sourcing: Option<Receiver<SourceRead>>,
}

/// Completion for the command palette: command names, then G-codes.
fn command_completer(word: &str) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = command_candidates(word)
        .into_iter()
        .map(|c| Candidate {
            insert: c.name.to_string(),
            description: c.summary.to_string(),
            dangerous: false,
        })
        .collect();
    if out.is_empty() || looks_like_gcode(word) {
        out.extend(gcode_candidates(word));
    }
    out.truncate(crate::widgets::input::MAX_CANDIDATES);
    out
}

fn console_kind(kind: HostKind) -> ConsoleKind {
    match kind {
        HostKind::Sent => ConsoleKind::Sent,
        HostKind::Received => ConsoleKind::Received,
        HostKind::Info => ConsoleKind::Info,
        HostKind::Warning => ConsoleKind::Warning,
        HostKind::Error => ConsoleKind::Error,
    }
}

impl App {
    /// An app for `printer` with `config` (its active profile is used).
    pub(crate) fn new(
        printer: Printer,
        config: AppConfig,
        config_path: Option<PathBuf>,
        theme: Theme,
    ) -> App {
        let profile = config.active().clone();
        printer.set_profile(profile.clone());
        let events = printer.subscribe();
        let console_capacity = config.console_max_lines.max(100);
        let mut app = App {
            printer,
            config,
            config_path,
            profile,
            theme,
            view: PrinterView::default(),
            temps: TempHistory::default(),
            events,
            file: None,
            loader: None,
            connecting: None,
            background: Vec::new(),
            focus: Focus::Layers,
            return_focus: Focus::Layers,
            input_mode: InputMode::Gcode,
            gcode_input: CommandInput::new(),
            command_input: CommandInput::new().with_completer(command_completer),
            search_input: plain_input(),
            search: None,
            center: Center::Layers,
            layer_view: None,
            fit: LayerFit::Object,
            show_travel: false,
            stats_scroll: 0,
            console: Vec::new(),
            console_seq: 0,
            console_capacity,
            console_state: ConsoleState::new(),
            show_auto: false,
            modal: None,
            jog: None,
            estop_armed: None,
            releases_seen: false,
            estop_released: false,
            resume_pending: None,
            toast: None,
            hidden_prompt: None,
            quit: false,
            dirty: true,
            console_area: Rect::default(),
            canvas_area: Rect::default(),
            base_dir: std::env::current_dir().unwrap_or_default(),
            last_load_progress: 0,
            last_target: None,
            pending_sd_delete: None,
            continued_at: None,
            sourcing: None,
        };
        app.sync();
        app
    }

    // ----- status messages -----

    pub(crate) fn notify(&mut self, text: impl Into<String>, tone: Tone) {
        self.toast = Some(Toast {
            text: text.into(),
            tone,
            until: Instant::now() + TOAST_TIME,
        });
        self.dirty = true;
    }

    fn info(&mut self, text: impl Into<String>) {
        self.notify(text, Tone::Plain);
    }

    fn good(&mut self, text: impl Into<String>) {
        self.notify(text, Tone::Good);
    }

    fn warn(&mut self, text: impl Into<String>) {
        self.notify(text, Tone::Warn);
    }

    fn error(&mut self, text: impl Into<String>) {
        self.notify(text, Tone::Bad);
    }

    // ----- printer sync -----

    /// Copy the printer state, new console lines and events.
    pub(crate) fn sync(&mut self) {
        self.view = self.printer.with_state(PrinterView::from_state);
        let temps = &mut self.temps;
        self.printer.with_state(|s| temps.update(&s.temp_history));
        self.sync_console();
        while let Ok(event) = self.events.try_recv() {
            self.on_printer_event(event);
        }
        self.sync_prompt();
        self.dirty = true;
    }

    fn sync_console(&mut self) {
        loop {
            let batch = self.printer.console_since(self.console_seq, CONSOLE_FETCH);
            let count = batch.len();
            for entry in batch {
                self.console_seq = entry.seq;
                self.console.push(ConsoleLine {
                    kind: console_kind(entry.kind),
                    text: entry.text,
                    auto: entry.auto,
                });
            }
            if count < CONSOLE_FETCH {
                break;
            }
        }
        // Trim in batches so the log is not shifted on every line.
        let slack = self.console_capacity / 10;
        if self.console.len() > self.console_capacity + slack {
            let excess = self.console.len() - self.console_capacity;
            self.console.drain(..excess);
            self.console_state.lines_removed(excess);
        }
    }

    fn on_printer_event(&mut self, event: Event) {
        match event {
            Event::Status(status) => match status {
                sprint_host::ConnectionStatus::Connected => {
                    let target = self.view.target.clone().unwrap_or_default();
                    let firmware = self.view.firmware.clone();
                    match firmware {
                        Some(fw) => self.good(format!("Connected to {target}: {fw}")),
                        None => self.good(format!("Connected to {target}")),
                    }
                }
                sprint_host::ConnectionStatus::Failed(why) => self.error(why),
                sprint_host::ConnectionStatus::Disconnected => self.info("Disconnected"),
                _ => {}
            },
            Event::Job(status) => self.on_job_status(&status),
            Event::SettingsUpdated => {
                self.report_arrived(|w| matches!(w, ReportSource::Settings(_)));
            }
            Event::Firmware(_) => self.report_arrived(|w| *w == ReportSource::Firmware),
            Event::Endstops(_) => self.report_arrived(|w| *w == ReportSource::Endstops),
            Event::Mesh(_) => self.report_arrived(|w| *w == ReportSource::Mesh),
            Event::Stats(_) => self.report_arrived(|w| *w == ReportSource::Stats),
            Event::SdFiles(_) => {
                // The host takes a deleted file off the listing.
                if let Some((name, _)) = self.pending_sd_delete.take() {
                    self.good(format!("Deleted {name} from the SD card"));
                }
                self.report_arrived(|w| *w == ReportSource::SdFiles);
            }
            Event::Console(entry)
                if entry.kind == HostKind::Warning && entry.text.contains("Could not delete") =>
            {
                if self.pending_sd_delete.take().is_some() {
                    self.error(entry.text);
                }
            }
            Event::AutotuneFinished { heater, kp, ki, kd } => self.good(format!(
                "PID autotune of {}: Kp {} Ki {} Kd {} (eeprom save keeps them)",
                heater.name(),
                format::fixed(f64::from(kp), 2),
                format::fixed(f64::from(ki), 2),
                format::fixed(f64::from(kd), 2)
            )),
            Event::AutotuneFailed(why) => self.error(format!("PID autotune failed: {why}")),
            Event::Error(why) => self.error(why),
            _ => {}
        }
    }

    fn on_job_status(&mut self, status: &JobStatus) {
        let name = self
            .view
            .job
            .as_ref()
            .map(|j| j.name.clone())
            .unwrap_or_default();
        match status {
            JobStatus::Starting => {
                self.layer_view = None;
                // No jogging during a job: the overlay goes too.
                self.jog = None;
                self.info(format!("Starting {name}"));
            }
            JobStatus::Paused => self.warn(format!("Paused {name} (F6 resumes)")),
            JobStatus::Completed => {
                let elapsed = self
                    .view
                    .job
                    .as_ref()
                    .map(|j| format::duration_of(j.elapsed))
                    .unwrap_or_default();
                self.good(format!("Finished {name} in {elapsed}"));
            }
            JobStatus::Cancelled => self.warn(format!("Cancelled {name}")),
            JobStatus::Failed(why) => self.error(format!("{name} failed: {why}")),
            _ => {}
        }
    }

    /// Show or hide the host prompt dialog to match the printer.
    fn sync_prompt(&mut self) {
        match self.view.prompt.clone() {
            Some(prompt) => {
                let showing = matches!(&self.modal, Some(Modal::Prompt(p)) if p.prompt == prompt);
                let hidden = self.hidden_prompt.as_ref() == Some(&prompt);
                if !showing && !hidden && !matches!(self.modal, Some(Modal::Confirm(_))) {
                    self.modal = Some(Modal::Prompt(PromptDialog::new(prompt)));
                }
            }
            None => {
                self.hidden_prompt = None;
                if matches!(self.modal, Some(Modal::Prompt(_))) {
                    self.modal = None;
                }
            }
        }
    }

    /// `p`: show the host prompt the user hid again (the printer still
    /// waits for its answer).
    fn show_hidden_prompt(&mut self) {
        if self.hidden_prompt.take().is_some() {
            self.sync_prompt();
        } else {
            self.info("The printer is not asking anything");
        }
    }

    // ----- timers -----

    /// Advance timers: status messages, the E-stop arm, loads, connects
    /// and report timeouts.
    pub(crate) fn tick(&mut self, now: Instant) {
        if self.toast.as_ref().is_some_and(|t| now >= t.until) {
            self.toast = None;
            self.dirty = true;
        }
        if self
            .estop_armed
            .is_some_and(|armed| now.duration_since(armed) >= ESTOP_WINDOW)
        {
            self.estop_armed = None;
            self.info("Emergency stop disarmed");
        }
        self.poll_loader();
        if let Some(rx) = &self.connecting {
            let result = match rx.try_recv() {
                Ok(result) => Some(result),
                Err(crossbeam_channel::TryRecvError::Empty) => None,
                // The connect thread died without an answer: do not keep
                // refusing new connects as "already connecting".
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    Some(Err("Connecting stopped unexpectedly".to_string()))
                }
            };
            if let Some(result) = result {
                self.connecting = None;
                if let Err(why) = result {
                    self.error(why);
                }
                self.dirty = true;
            }
        }
        self.poll_background();
        self.poll_source();
        self.tick_resume(now);
        self.tick_report(now);
        self.tick_sd_delete(now);
    }

    /// Stop waiting for the answer to an SD delete after
    /// [`REPORT_TIMEOUT`]: a later listing must not be taken for it.
    fn tick_sd_delete(&mut self, now: Instant) {
        let Some((name, since)) = &self.pending_sd_delete else {
            return;
        };
        if now.saturating_duration_since(*since) < REPORT_TIMEOUT {
            return;
        }
        let name = name.clone();
        self.pending_sd_delete = None;
        self.warn(format!(
            "No answer about deleting {name} (sd ls shows the card)"
        ));
    }

    /// Whether the job is paused, and whether nothing is queued or
    /// unanswered.
    fn paused_and_idle(&self) -> (bool, bool) {
        self.printer.with_state(|s| {
            let paused = s
                .job
                .as_ref()
                .is_some_and(|j| j.status == JobStatus::Paused);
            (paused, crate::session::link_idle(s))
        })
    }

    /// Resume once the link has been idle for a moment.
    fn tick_resume(&mut self, now: Instant) {
        if self.resume_pending.is_none() {
            return;
        }
        let (paused, idle) = self.paused_and_idle();
        let Some(pending) = &mut self.resume_pending else {
            return;
        };
        if !paused {
            self.resume_pending = None;
            return;
        }
        if !idle {
            pending.idle_since = None;
            return;
        }
        let since = *pending.idle_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= crate::session::IDLE_SETTLE {
            self.resume_pending = None;
            self.printer.resume();
            self.info("Resuming");
        }
    }

    /// Resume the job after the commands typed while it was paused (a
    /// purge, a move): the printer would run the resume script (back to
    /// the print) before them otherwise, and they would land on the part.
    /// A command sent a moment ago may not show as queued yet, so the link
    /// must stay idle for a moment first.
    fn resume(&mut self) {
        let (paused, idle) = self.paused_and_idle();
        if paused {
            self.resume_pending = Some(PendingResume::default());
            self.info(if idle {
                "Resuming"
            } else {
                "Resuming once the queued commands are done"
            });
        } else {
            self.printer.resume();
            self.info("Resuming");
        }
    }

    /// Run `work` on a helper thread named `name`; its message shows when
    /// it ends.
    fn spawn_background(
        &mut self,
        name: &str,
        work: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let _ = tx.send(work());
            });
        match spawned {
            Ok(_) => self.background.push(rx),
            Err(e) => self.error(format!("Cannot start {name}: {e}")),
        }
    }

    /// `source <file>`: read the file off the UI thread; its lines are run
    /// once confirmed (see [`App::poll_source`]).
    fn read_source(&mut self, path: PathBuf) {
        if self.sourcing.is_some() {
            self.warn("Already reading a source file");
            return;
        }
        let path = crate::paths::resolve(&path.to_string_lossy(), &self.base_dir);
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("sprint-source".into())
            .spawn(move || {
                let read = std::fs::read(&path)
                    .map(|bytes| {
                        let name = path.file_name().map_or_else(
                            || path.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        let lines = pasted_commands(&String::from_utf8_lossy(&bytes))
                            .into_iter()
                            .map(str::to_string)
                            .collect();
                        (name, lines)
                    })
                    .map_err(|e| format!("Cannot read {}: {e}", path.display()));
                let _ = tx.send(read);
            });
        match spawned {
            Ok(_) => self.sourcing = Some(rx),
            Err(e) => self.error(format!("Cannot read the file: {e}")),
        }
    }

    /// A `source` file was read: ask before running its lines.
    fn poll_source(&mut self) {
        let Some(rx) = &self.sourcing else {
            return;
        };
        let read = match rx.try_recv() {
            Ok(read) => read,
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Err("Reading the source file stopped".to_string())
            }
        };
        self.sourcing = None;
        match read {
            Ok((name, lines)) if lines.is_empty() => self.warn(format!("{name} has no commands")),
            Ok((name, lines)) => {
                let job_active = self.view.job_active();
                let mut message = Vec::new();
                if job_active {
                    message.push(
                        "A job is running: G-code goes between the print's lines.".to_string(),
                    );
                }
                message.extend(macro_preview(&lines.join("\n")));
                self.modal = Some(Modal::Confirm(Confirm::new(
                    format!("Run {} lines of {name}?", lines.len()),
                    message,
                    "Run",
                    "Not now",
                    job_active,
                    Action::RunLines(lines),
                )));
            }
            Err(why) => self.error(why),
        }
    }

    /// Show the messages of finished background work.
    fn poll_background(&mut self) {
        let mut finished = Vec::new();
        self.background.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                finished.push(result);
                false
            }
            Err(crossbeam_channel::TryRecvError::Empty) => true,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                finished.push(Err("A background task stopped".to_string()));
                false
            }
        });
        for result in finished {
            match result {
                Ok(text) => self.info(text),
                Err(why) => self.error(why),
            }
        }
    }

    /// End a report's wait: on timeout, or once the link has been idle for
    /// a moment when the answer may bring no news.
    fn tick_report(&mut self, now: Instant) {
        let Some(Modal::Report(r)) = &mut self.modal else {
            return;
        };
        let (Some(pending), Some(source)) = (&mut r.pending, &r.source) else {
            return;
        };
        let elapsed = now.saturating_duration_since(pending.since);
        if elapsed >= pending.timeout {
            let mut lines = vec![
                ReportLine::toned(
                    "The printer did not answer; showing what is known.",
                    Tone::Warn,
                ),
                ReportLine::new(),
            ];
            lines.extend(report_lines(&self.printer, source));
            r.lines = lines;
            r.pending = None;
            self.dirty = true;
            return;
        }
        if !pending.idle_completes || elapsed < REPORT_IDLE_AFTER {
            return;
        }
        let idle = self
            .printer
            .with_state(|s| s.link.queue_len == 0 && s.link.in_flight == 0);
        if !idle {
            pending.idle_since = None;
            return;
        }
        let since = *pending.idle_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= REPORT_IDLE_AFTER {
            r.lines = report_lines(&self.printer, source);
            r.pending = None;
            self.dirty = true;
        }
    }

    fn poll_loader(&mut self) {
        let Some(job) = &self.loader else {
            return;
        };
        match job.poll() {
            None => {
                let permille = (job.progress() * 1000.0) as u32;
                if permille != self.last_load_progress {
                    self.last_load_progress = permille;
                    self.dirty = true;
                }
            }
            Some(Ok(loaded)) => {
                self.loader = None;
                let stats = &loaded.toolpath.stats;
                let text = format!(
                    "Loaded {}: {} layers, {} estimated, {} filament",
                    loaded.name(),
                    loaded.toolpath.layers.len(),
                    format::duration(f64::from(stats.estimated_time)),
                    format::length_mm(f64::from(stats.filament_length))
                );
                self.remember_recent(loaded.path.clone());
                self.file = Some(loaded);
                self.layer_view = None;
                self.stats_scroll = 0;
                self.good(text);
            }
            Some(Err(why)) => {
                self.loader = None;
                self.error(format!("Cannot load: {why}"));
            }
        }
    }

    /// Add a recent file to the configuration (and its file).
    fn remember_recent(&mut self, file: PathBuf) {
        let Some(path) = &self.config_path else {
            self.config.push_recent(file);
            return;
        };
        match crate::config::save_recent(path, file.clone(), &self.config) {
            Ok(saved) => self.keep_profile(saved),
            Err(e) => {
                log::warn!("could not save the configuration: {e}");
                self.config.push_recent(file);
            }
        }
    }

    // ----- the loaded file and the print -----

    /// Start loading `path` (relative to the starting directory) in the
    /// background.
    pub(crate) fn open_file(&mut self, path: PathBuf) {
        // The full path: it becomes a recent file, which must not depend on
        // where the next session starts.
        let path = if path.is_relative() {
            self.base_dir.join(path)
        } else {
            path
        };
        let options = analyze_options(&self.profile);
        let job = LoadJob::spawn(path, options);
        self.info(format!("Loading {}", job.name()));
        self.loader = Some(job);
        self.last_load_progress = 0;
    }

    /// The layer being printed, when the job prints the loaded file.
    pub(crate) fn printing_layer(&self) -> Option<usize> {
        let job = self.view.job.as_ref()?;
        let file = self.file.as_ref()?;
        if job.kind != JobKind::HostPrint || job.name != file.name() {
            return None;
        }
        job.layer.map(|(index, _)| index).or_else(|| {
            let line = u32::try_from(job.current_line.saturating_sub(1)).ok()?;
            file.toolpath.layer_at_line(line)
        })
    }

    /// The last printed source line (0-based), for dimming what is still
    /// to print; `None` draws everything.
    pub(crate) fn progress_line(&self) -> Option<u32> {
        let job = self.view.job.as_ref()?;
        let file = self.file.as_ref()?;
        if job.kind != JobKind::HostPrint
            || job.name != file.name()
            || job.status == JobStatus::Completed
        {
            return None;
        }
        u32::try_from(job.current_line.saturating_sub(1)).ok()
    }

    /// The layer shown: the chosen one, else the printing one, else the
    /// first.
    pub(crate) fn shown_layer(&self) -> usize {
        let count = self.file.as_ref().map_or(0, |f| f.toolpath.layers.len());
        let layer = self
            .layer_view
            .or_else(|| self.printing_layer())
            .unwrap_or(0);
        layer.min(count.saturating_sub(1))
    }

    fn step_layer(&mut self, delta: isize) {
        let count = self.file.as_ref().map_or(0, |f| f.toolpath.layers.len());
        if count == 0 {
            return;
        }
        let current = self.shown_layer() as isize;
        let next = (current + delta).clamp(0, count as isize - 1) as usize;
        self.layer_view = Some(next);
    }

    fn connected_or_warn(&mut self) -> bool {
        if self.view.connected() {
            true
        } else {
            self.warn("Not connected (F2 connects)");
            false
        }
    }

    /// F5: ask before printing the loaded file.
    fn request_print(&mut self) {
        let blocker = self
            .printer
            .with_state(|s| actions::print_blocker(s, self.file.as_ref()));
        if let Some(why) = blocker {
            let hint = match why.as_str() {
                "No file loaded" if self.loader.is_some() => " yet (still loading)",
                "No file loaded" => " (F3 opens one)",
                "Not connected to a printer" => " (F2 connects)",
                _ => "",
            };
            self.warn(format!("{why}{hint}"));
            return;
        }
        let Some(file) = &self.file else {
            return;
        };
        let stats = &file.toolpath.stats;
        let target = self.view.target.clone().unwrap_or_default();
        let message = vec![
            file.name().to_string(),
            format!(
                "{} layers · {} estimated · {} filament",
                file.toolpath.layers.len(),
                format::duration(f64::from(stats.estimated_time)),
                format::length_mm(f64::from(stats.filament_length))
            ),
            format!("on {target}"),
        ];
        self.modal = Some(Modal::Confirm(Confirm::new(
            "Start print?",
            message,
            "Print",
            "Not now",
            false,
            Action::StartPrint(file.path.clone()),
        )));
    }

    /// Print the loaded file when it is still the one at `path`: another
    /// file may have finished loading while the question was open.
    fn start_print(&mut self, path: &Path) {
        let Some(file) = &self.file else {
            self.warn("No file loaded (F3 opens one)");
            return;
        };
        if file.path != path {
            let name = file.name().to_string();
            self.warn(format!(
                "{name} was loaded meanwhile; press F5 again to print it"
            ));
            return;
        }
        let options = actions::print_options(file);
        let name = file.name().to_string();
        match self.printer.start_print(file.file.clone(), options) {
            Ok(()) => {
                self.layer_view = None;
                self.good(format!("Printing {name}"));
            }
            Err(e) => self.error(format!("Cannot print: {e}")),
        }
    }

    /// F6: pause or resume.
    fn toggle_pause(&mut self) {
        if self.continue_user_wait() {
            return;
        }
        match self.view.job.as_ref().map(|j| j.status.clone()) {
            Some(JobStatus::Paused | JobStatus::Pausing) => self.resume(),
            Some(status) if status.is_active() => {
                self.printer.pause();
                self.info("Pausing");
            }
            _ => self.warn("No job is running"),
        }
    }

    /// When the firmware waits for the user without a host prompt (M0/M1
    /// or a filament change on firmware without prompt support): let it go
    /// on and return true. Usually that is M108. A print the printer
    /// paused for the wait (`//action:paused`) is resumed instead, and the
    /// engine sends M108 then; it is resumed at once, since the link
    /// cannot become idle while the firmware holds back the `ok` of its
    /// M0.
    fn continue_user_wait(&mut self) -> bool {
        if !self.view.waiting_for_user {
            return false;
        }
        // Pressed again (or held) while the wait still shows.
        if self
            .continued_at
            .is_some_and(|t| t.elapsed() < CONTINUE_SETTLE)
        {
            self.info("Continuing (M108 sent a moment ago)");
            return true;
        }
        self.continued_at = Some(Instant::now());
        let paused = self
            .view
            .job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused);
        if paused {
            self.resume_pending = None;
            self.printer.resume();
        } else {
            self.printer.send("M108");
        }
        self.info("Continuing (M108)");
        true
    }

    /// F7: ask before cancelling, in words that fit the job (a print, an
    /// SD print, an upload).
    fn request_cancel(&mut self) {
        let Some(job) = self.view.job.as_ref().filter(|j| j.status.is_active()) else {
            self.warn("No job is running");
            return;
        };
        let question = actions::cancel_question(job, &self.profile.cancel_script);
        self.modal = Some(Modal::Confirm(Confirm::new(
            question.title,
            question.message,
            question.yes,
            question.no,
            true,
            Action::CancelJob,
        )));
    }

    /// F8: arm, then stop on the second press. A held key is not a second
    /// press: repeats are ignored, and where the terminal reports releases
    /// (the Windows console reports a held key as more presses) F8 must
    /// have been released in between.
    fn emergency_key(&mut self, kind: KeyEventKind) {
        if !self.view.status.is_open() {
            // Nothing would be sent: do not claim an emergency stop.
            self.estop_armed = None;
            self.warn("Not connected: there is no printer to stop");
            return;
        }
        if kind == KeyEventKind::Repeat {
            return;
        }
        if self.estop_armed.is_some() {
            if self.releases_seen && !self.estop_released {
                return;
            }
            self.estop_armed = None;
            self.printer.emergency_stop();
            self.error("EMERGENCY STOP sent (M112). Restart the firmware (M999) or reconnect.");
        } else {
            self.estop_armed = Some(Instant::now());
            self.estop_released = false;
            self.dirty = true;
        }
    }

    /// F10 / Ctrl+Q: quit, asking first while a job runs.
    fn request_quit(&mut self) {
        if !self.job_stops_ask(
            "Quit Sprint?",
            "Quitting",
            "Quit anyway",
            "Stay",
            Action::Quit,
        ) {
            self.quit = true;
        }
    }

    /// Connect on a background thread (opening a link can take seconds).
    pub(crate) fn connect(&mut self, target: ConnectTarget) {
        self.connect_with(target, false);
    }

    /// [`App::connect`]; `reset` pulses DTR after opening a serial port,
    /// which resets most boards.
    fn connect_with(&mut self, target: ConnectTarget, reset: bool) {
        if self.connecting.is_some() {
            self.warn("Already connecting");
            return;
        }
        self.last_target = Some(target.clone());
        let reconnect = self.view.status.is_open();
        let printer = self.printer.clone();
        let mut options = self.profile.options.clone();
        options.reset_on_connect |= reset;
        let label = target.label();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("sprint-connect".into())
            .spawn(move || {
                // Closing a link can take a moment too: off the UI thread.
                // A running print is cancelled first (heaters off).
                if reconnect {
                    let _ = session::cancel_before_closing(&printer, session::CANCEL_WAIT, |_| {});
                    printer.disconnect();
                }
                let result = printer
                    .connect(target, options)
                    .map_err(|e| format!("Cannot connect to {label}: {e}"));
                let _ = tx.send(result);
            });
        match spawned {
            Ok(_) => {
                self.connecting = Some(rx);
                self.info("Connecting…");
            }
            Err(e) => self.error(format!("Cannot connect: {e}")),
        }
    }

    // ----- dialogs -----

    fn open_connect_dialog(&mut self) {
        let dialog = ConnectDialog::new(list_ports(), self.view.status.is_open(), &self.profile);
        self.modal = Some(Modal::Connect(dialog));
    }

    fn open_file_dialog(&mut self) {
        let recent = self
            .config
            .recent_files
            .iter()
            .filter(|p| p.is_file())
            .cloned()
            .collect();
        self.modal = Some(Modal::Open(OpenDialog::new(self.base_dir.clone(), recent)));
    }

    fn open_temp_dialog(&mut self) {
        if !self.connected_or_warn() {
            return;
        }
        let heaters = self
            .printer
            .with_state(|s| actions::heaters(s, &self.profile));
        let readings = heaters
            .into_iter()
            .map(|h| {
                let reading = match h {
                    Heater::Hotend(t) => self.view.temps.hotends.get(usize::from(t)),
                    Heater::Bed => self.view.temps.bed.as_ref(),
                    Heater::Chamber => self.view.temps.chamber.as_ref(),
                };
                let current = reading.map_or(f32::NAN, |r| r.current);
                let target = reading.and_then(|r| r.target).unwrap_or(0.0);
                (h, current, target)
            })
            .collect();
        self.modal = Some(Modal::Temps(TempDialog::new(
            readings,
            self.profile.temp_presets.clone(),
        )));
    }

    /// Show a report of `source` that waits for the printer's answer
    /// (the request was just sent).
    fn open_report(&mut self, title: &str, source: ReportSource, pending: Pending) {
        let lines = report_lines(&self.printer, &source);
        let mut view = ReportView::new(title, lines);
        view.source = Some(source);
        view.pending = Some(pending);
        self.modal = Some(Modal::Report(view));
    }

    fn show_report(&mut self, title: &str, lines: Vec<ReportLine>) {
        self.modal = Some(Modal::Report(ReportView::new(title, lines)));
    }

    /// The printer sent news a shown report is made of: redraw it.
    fn report_arrived(&mut self, matches: impl Fn(&ReportSource) -> bool) {
        if let Some(Modal::Report(r)) = &mut self.modal
            && let Some(source) = r.source.as_ref().filter(|s| matches(s))
        {
            r.lines = report_lines(&self.printer, source);
            r.pending = None;
            self.dirty = true;
        }
    }

    /// Close the connection on a helper thread (the engine threads get a
    /// moment to stop, which must not freeze the screen).
    /// A running print is cancelled first, so its cancel script turns the
    /// heaters off (see [`session::cancel_before_closing`]).
    fn disconnect(&mut self) {
        let printer = self.printer.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-disconnect".into())
            .spawn(move || {
                let _ = session::cancel_before_closing(&printer, session::CANCEL_WAIT, |_| {});
                printer.disconnect();
            });
        if spawned.is_err() {
            self.printer.disconnect();
        }
    }

    /// While a job runs, ask before `action`, which closes the connection,
    /// and return true; otherwise return false. `doing` ("Quitting") names
    /// the action in the question, `yes` and `no` are the answers.
    fn job_stops_ask(
        &mut self,
        title: &str,
        doing: &str,
        yes: &str,
        no: &str,
        action: Action,
    ) -> bool {
        let Some(job) = self.view.job.as_ref().filter(|j| j.status.is_active()) else {
            return false;
        };
        let name = &job.name;
        // A print Sprint streams (or an upload) is cancelled before the
        // link closes; the printer goes on with a print from its card.
        let message = match job.kind {
            JobKind::HostPrint => vec![
                format!("{name} is printing and Sprint streams it."),
                format!("{doing} cancels it first: the cancel script turns the heaters off."),
            ],
            JobKind::SdPrint => vec![
                format!("{name} prints from the printer's SD card."),
                format!("It goes on after {}, unwatched;", doing.to_lowercase()),
                "cancel it first (F7) to stop it.".to_string(),
            ],
            JobKind::SdUpload => vec![
                format!("{name} is being written to the SD card."),
                format!("{doing} cancels the upload and deletes the partial file."),
            ],
        };
        self.modal = Some(Modal::Confirm(Confirm::new(
            title, message, yes, no, true, action,
        )));
        true
    }

    /// Send heater targets.
    fn set_temps(&mut self, targets: &[(Heater, f32)]) {
        for &(heater, celsius) in targets {
            self.printer.set_temperature(heater, celsius);
        }
        self.info(actions::targets_text(targets));
    }

    /// The heaters that `targets` turn off while they are heating.
    fn heaters_turned_off(&self, targets: &[(Heater, f32)]) -> Vec<Heater> {
        let temps = &self.view.temps;
        targets
            .iter()
            .filter(|&&(_, celsius)| celsius <= 0.0)
            .filter(|&&(heater, _)| {
                let reading = match heater {
                    Heater::Hotend(t) => temps.hotends.get(usize::from(t)),
                    Heater::Bed => temps.bed.as_ref(),
                    Heater::Chamber => temps.chamber.as_ref(),
                };
                reading
                    .and_then(|r| r.target)
                    .is_some_and(|t| t.is_finite() && t > 0.0)
            })
            .map(|&(heater, _)| heater)
            .collect()
    }

    /// Run what a dialog asked for.
    fn run_action(&mut self, action: Action) {
        match action {
            Action::Connect(target) => {
                if !self.job_stops_ask(
                    "Connect elsewhere?",
                    "Connecting elsewhere",
                    "Connect anyway",
                    "Stay connected",
                    Action::ConnectAnyway(target.clone()),
                ) {
                    self.connect(target);
                }
            }
            Action::ConnectAnyway(target) => self.connect(target),
            Action::Disconnect => {
                if !self.job_stops_ask(
                    "Disconnect?",
                    "Disconnecting",
                    "Disconnect anyway",
                    "Stay connected",
                    Action::DisconnectAnyway,
                ) {
                    self.disconnect();
                }
            }
            Action::DisconnectAnyway => self.disconnect(),
            Action::Open(path) => self.open_file(path),
            Action::SetTemps(targets) => {
                let targets = actions::safe_targets(&targets);
                let off = self.heaters_turned_off(&targets);
                if self.view.job_active() && !off.is_empty() {
                    let name = self
                        .view
                        .job
                        .as_ref()
                        .map(|j| j.name.clone())
                        .unwrap_or_default();
                    let names: Vec<String> = off.into_iter().map(actions::heater_label).collect();
                    self.modal = Some(Modal::Confirm(Confirm::new(
                        "Turn heaters off while printing?",
                        vec![
                            format!("{} off while {name} prints.", names.join(" and ")),
                            "The print will most likely fail.".to_string(),
                        ],
                        "Turn off",
                        "Keep heating",
                        true,
                        Action::SetTempsAnyway(targets),
                    )));
                    return;
                }
                self.set_temps(&targets);
            }
            Action::SetTempsAnyway(targets) => {
                self.set_temps(&actions::safe_targets(&targets));
            }
            Action::StartPrint(path) => self.start_print(&path),
            Action::CancelJob => {
                self.printer.cancel();
                self.warn("Cancelling");
            }
            Action::Quit => self.quit = true,
            Action::EepromReset => {
                self.printer.factory_reset_settings();
                self.warn("Settings reset to factory defaults (M502); eeprom save keeps them");
            }
            Action::Send(gcode) => self.printer.send(&gcode),
            Action::RunMacro(name, args) => {
                let found = actions::find_macro(&self.config.macros, &name).and_then(|m| {
                    let gcode = actions::macro_gcode(m, &args)?;
                    Ok(sprint_host::Macro { gcode, ..m.clone() })
                });
                match found {
                    Ok(m) => {
                        self.printer.run_macro(&m);
                        self.info(format!("Macro {}", m.name));
                    }
                    Err(why) => self.error(why),
                }
            }
            Action::DeleteMacro(name) => {
                let saved = crate::config::edit_config(
                    self.config_path.as_deref(),
                    &self.config,
                    |config| {
                        actions::remove_macro(&mut config.macros, &name);
                    },
                );
                match saved {
                    Ok(config) => {
                        self.keep_profile(config);
                        self.info(format!("Deleted the macro {name}"));
                    }
                    Err(why) => self.error(format!("{name} not deleted: {why}")),
                }
            }
            Action::SdPrint(name) => match self.printer.start_sd_print(&name) {
                Ok(()) => self.good(format!("Printing {name} from the SD card")),
                Err(e) => self.error(format!("Cannot print {name}: {e}")),
            },
            Action::SdDelete(name) => {
                // The card may have become busy while the question was open.
                if let Some(why) = self.printer.with_state(actions::sd_delete_blocker) {
                    self.warn(why);
                    return;
                }
                self.printer.delete_sd_file(&name);
                self.info(format!("Deleting {name} from the SD card (M30)…"));
                self.pending_sd_delete = Some((name, Instant::now()));
            }
            Action::AnswerPrompt(button) => self.printer.answer_prompt(button),
            Action::EverythingOff => {
                if self.connected_or_warn() {
                    let heaters = session::everything_off(&self.printer);
                    let names: Vec<String> =
                        heaters.into_iter().map(actions::heater_label).collect();
                    self.info(format!(
                        "{} off, fans off, motors off (M84)",
                        names.join(", ")
                    ));
                }
            }
            Action::RunLines(lines) => {
                let total = lines.len();
                for (i, line) in lines.iter().enumerate() {
                    self.run_line(line);
                    // A question stops the run: its answer must come first.
                    if self.modal.is_some() && i + 1 < total {
                        self.warn(format!(
                            "Stopped after line {} of {total}, which asks a question",
                            i + 1
                        ));
                        break;
                    }
                }
            }
        }
    }

    // ----- input -----

    /// Handle one terminal event.
    pub(crate) fn handle_event(&mut self, event: TermEvent) {
        match event {
            TermEvent::Key(key) if key.kind == KeyEventKind::Release => {
                self.releases_seen = true;
                if key.code == KeyCode::F(8) {
                    self.estop_released = true;
                }
                return;
            }
            TermEvent::Key(key) => self.on_key(key),
            TermEvent::Mouse(mouse) => self.on_mouse(mouse),
            TermEvent::Paste(text) => self.on_paste(&text),
            TermEvent::Resize(..) | TermEvent::FocusGained => {}
            _ => return,
        }
        self.dirty = true;
    }

    fn on_paste(&mut self, text: &str) {
        match &mut self.modal {
            Some(Modal::Open(d)) => d.paste(text),
            Some(Modal::Connect(d)) => d.paste(text),
            Some(_) => {}
            None => {
                let to_gcode = self.focus != Focus::Input || self.input_mode == InputMode::Gcode;
                let commands = pasted_commands(text);
                if to_gcode && commands.len() > 1 {
                    // On one line the commands would run as one garbled
                    // command ("G28 G1 X10" homes X).
                    self.confirm_pasted(&commands);
                    return;
                }
                if self.focus != Focus::Input {
                    self.focus_input(InputMode::Gcode);
                }
                match commands.as_slice() {
                    [one] if to_gcode => self.active_input().insert_str(one),
                    _ => self
                        .active_input()
                        .insert_str(text.trim_end_matches(['\r', '\n'])),
                }
            }
        }
    }

    /// Ask before sending a pasted block, one command per line.
    fn confirm_pasted(&mut self, commands: &[&str]) {
        if !self.connected_or_warn() {
            return;
        }
        let dangerous: Vec<String> = commands
            .iter()
            .filter_map(|c| actions::dangerous_gcode(c))
            .map(|doc| format!("{} ({}) is among them.", doc.code, doc.name))
            .collect();
        let job_active = self.view.job_active();
        let mut message = Vec::new();
        if job_active {
            message.push("A job is running: these commands go between the print's lines.".into());
        }
        message.extend(macro_preview(&commands.join("\n")));
        message.extend(dangerous.iter().cloned());
        self.modal = Some(Modal::Confirm(Confirm::new(
            format!("Send {} pasted lines?", commands.len()),
            message,
            "Send",
            "Don't send",
            job_active || !dangerous.is_empty(),
            Action::Send(commands.join("\n")),
        )));
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.modal.is_some() {
            return;
        }
        let at = Position::new(mouse.column, mouse.row);
        if self.console_area.contains(at) {
            self.console_state.handle_mouse(mouse);
        } else if self.canvas_area.contains(at) && self.center == Center::Layers {
            match mouse.kind {
                MouseEventKind::ScrollUp => self.step_layer(1),
                MouseEventKind::ScrollDown => self.step_layer(-1),
                _ => {}
            }
        }
    }

    fn active_input(&mut self) -> &mut CommandInput {
        match self.input_mode {
            InputMode::Gcode => &mut self.gcode_input,
            InputMode::Command => &mut self.command_input,
            InputMode::Search => &mut self.search_input,
        }
    }

    fn focus_input(&mut self, mode: InputMode) {
        if self.focus != Focus::Input {
            self.return_focus = self.focus;
        }
        self.input_mode = mode;
        self.focus = Focus::Input;
        if mode == InputMode::Search {
            let text = self.search.clone().unwrap_or_default();
            self.search_input.set_text(&text);
        }
    }

    /// Leave the input line for the panel it was entered from.
    fn leave_input(&mut self) {
        self.input_mode = InputMode::Gcode;
        self.focus = self.return_focus;
    }

    fn cycle_focus(&mut self, forward: bool) {
        let order = [Focus::Layers, Focus::Console, Focus::Input];
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if forward { (i + 1) % 3 } else { (i + 2) % 3 };
        self.focus = order[next];
        if self.focus == Focus::Input && self.input_mode == InputMode::Search {
            self.input_mode = InputMode::Gcode;
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // The emergency stop works everywhere; any other key disarms it.
        if key.code == KeyCode::F(8) {
            self.emergency_key(key.kind);
            return;
        }
        if self.estop_armed.take().is_some() {
            self.info("Emergency stop not sent");
        }
        if let Some(modal) = self.modal.take() {
            self.on_modal_key(modal, key);
            return;
        }
        match key.code {
            KeyCode::F(1) => {
                self.modal = Some(Modal::Help { scroll: 0 });
                return;
            }
            KeyCode::F(2) => {
                self.open_connect_dialog();
                return;
            }
            KeyCode::F(3) => {
                self.open_file_dialog();
                return;
            }
            KeyCode::F(5) => {
                self.request_print();
                return;
            }
            KeyCode::F(6) => {
                self.toggle_pause();
                return;
            }
            KeyCode::F(7) => {
                self.request_cancel();
                return;
            }
            KeyCode::F(10) => {
                self.request_quit();
                return;
            }
            KeyCode::Char('q' | 'Q') if ctrl => {
                self.request_quit();
                return;
            }
            KeyCode::Char('c' | 'C') if ctrl => {
                if self.focus == Focus::Input && !self.active_input().is_empty() {
                    self.active_input().clear();
                } else {
                    self.request_quit();
                }
                return;
            }
            _ => {}
        }
        if self.jog.is_some() {
            self.on_jog_key(key);
            return;
        }
        match self.focus {
            Focus::Input => self.on_input_key(key),
            Focus::Layers | Focus::Console => self.on_panel_key(key),
        }
    }

    fn on_modal_key(&mut self, mut modal: Modal, key: KeyEvent) {
        let outcome = match &mut modal {
            Modal::Help { scroll } => match key.code {
                KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?' | 'q') | KeyCode::Enter => {
                    Outcome::Close
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                    Outcome::Keep
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll += 1;
                    Outcome::Keep
                }
                KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(10);
                    Outcome::Keep
                }
                KeyCode::PageDown => {
                    *scroll += 10;
                    Outcome::Keep
                }
                KeyCode::Home => {
                    *scroll = 0;
                    Outcome::Keep
                }
                _ => Outcome::Keep,
            },
            Modal::Connect(d) => d.handle_key(key),
            Modal::Open(d) => d.handle_key(key),
            Modal::Temps(d) => d.handle_key(key),
            Modal::Confirm(d) => d.handle_key(key),
            Modal::Report(d) => d.handle_key(key),
            Modal::Prompt(d) => {
                let outcome = d.handle_key(key);
                if outcome == Outcome::Close {
                    self.hidden_prompt = Some(d.prompt.clone());
                    self.info("Prompt hidden; the printer still waits (p shows it again)");
                }
                outcome
            }
        };
        match outcome {
            Outcome::Keep => self.modal = Some(modal),
            Outcome::Close => {}
            Outcome::Error(why) => {
                self.modal = Some(modal);
                self.error(why);
            }
            Outcome::Run(action) => self.run_action(action),
        }
    }

    fn on_input_key(&mut self, key: KeyEvent) {
        let mode = self.input_mode;
        match self.active_input().handle_key(key) {
            InputEvent::Submitted(line) => match mode {
                InputMode::Gcode => self.send_gcode(&line),
                // The palette closes once its command runs.
                InputMode::Command => {
                    self.leave_input();
                    self.run_line(&line);
                }
                InputMode::Search => {
                    self.search = Some(line);
                    self.input_mode = InputMode::Gcode;
                    self.focus = Focus::Console;
                }
            },
            InputEvent::Handled => {
                // Enter on an empty search line clears the search; on an
                // empty palette it closes the palette.
                let enter = key.code == KeyCode::Enter && self.active_input().is_empty();
                if enter && mode == InputMode::Search {
                    self.search = None;
                    self.input_mode = InputMode::Gcode;
                    self.focus = Focus::Console;
                } else if enter && mode == InputMode::Command {
                    self.leave_input();
                }
            }
            InputEvent::Ignored => match key.code {
                KeyCode::Esc => self.leave_input(),
                KeyCode::Tab => self.cycle_focus(true),
                KeyCode::BackTab => self.cycle_focus(false),
                _ => {
                    self.console_state.handle_key(key);
                }
            },
        }
    }

    fn on_panel_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            return;
        }
        // Keys of the focused panel first.
        let used = match self.focus {
            Focus::Console => self.on_console_key(key),
            _ => self.on_layers_key(key),
        };
        if used {
            return;
        }
        match key.code {
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Char('?') => self.modal = Some(Modal::Help { scroll: 0 }),
            KeyCode::Char(':') => self.focus_input(InputMode::Command),
            KeyCode::Char('i') | KeyCode::Enter => self.focus_input(InputMode::Gcode),
            KeyCode::Char('/') => self.focus_input(InputMode::Search),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('j') => self.enter_jog(),
            KeyCode::Char('t') => self.open_temp_dialog(),
            KeyCode::Char('p') => self.show_hidden_prompt(),
            KeyCode::Char('a') => {
                self.show_auto = !self.show_auto;
                self.info(if self.show_auto {
                    "Showing automatic traffic (temperature polling)"
                } else {
                    "Hiding automatic traffic"
                });
            }
            KeyCode::Char('s') => {
                self.center = match self.center {
                    Center::Layers => Center::Stats,
                    Center::Stats => Center::Layers,
                };
            }
            // Typing a command in capitals starts the G-code line.
            KeyCode::Char(c) if c.is_ascii_uppercase() => {
                self.focus_input(InputMode::Gcode);
                self.gcode_input.insert_char(c);
            }
            _ => {}
        }
    }

    fn on_layers_key(&mut self, key: KeyEvent) -> bool {
        if self.center == Center::Stats {
            match key.code {
                KeyCode::Up => self.stats_scroll = self.stats_scroll.saturating_sub(1),
                KeyCode::Down => self.stats_scroll += 1,
                KeyCode::PageUp => self.stats_scroll = self.stats_scroll.saturating_sub(10),
                KeyCode::PageDown => self.stats_scroll += 10,
                KeyCode::Home => self.stats_scroll = 0,
                _ => return false,
            }
            return true;
        }
        let count = self.file.as_ref().map_or(0, |f| f.toolpath.layers.len());
        match key.code {
            KeyCode::Left | KeyCode::Char('[') | KeyCode::Down => self.step_layer(-1),
            KeyCode::Right | KeyCode::Char(']') | KeyCode::Up => self.step_layer(1),
            KeyCode::PageDown => self.step_layer(-10),
            KeyCode::PageUp => self.step_layer(10),
            KeyCode::Home if count > 0 => self.layer_view = Some(0),
            KeyCode::End if count > 0 => self.layer_view = Some(count - 1),
            KeyCode::Char('f') => {
                self.layer_view = None;
                if self.printing_layer().is_some() {
                    self.info("Following the print");
                }
            }
            KeyCode::Char('b') => {
                self.fit = match self.fit {
                    LayerFit::Bed => LayerFit::Object,
                    LayerFit::Object => LayerFit::Bed,
                };
            }
            KeyCode::Char('v') => self.show_travel = !self.show_travel,
            _ => return false,
        }
        true
    }

    fn on_console_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => self.console_state.scroll_up(1),
            KeyCode::Down => self.console_state.scroll_down(1),
            KeyCode::PageUp => self.console_state.page_up(),
            KeyCode::PageDown => self.console_state.page_down(),
            KeyCode::Home => self.console_state.scroll_to_top(),
            KeyCode::End => self.console_state.follow(),
            KeyCode::Char('c') => {
                self.printer.clear_console();
                let removed = self.console.len();
                self.console.clear();
                self.console_state.lines_removed(removed);
                self.console_state.follow();
                self.info("Console cleared");
            }
            KeyCode::Char('n') if self.search.is_some() => {
                self.search = None;
            }
            _ => return false,
        }
        true
    }

    // ----- jog mode -----

    fn enter_jog(&mut self) {
        if !self.connected_or_warn() {
            return;
        }
        if self.view.job_active() {
            self.warn("Jogging is disabled while a job runs");
            return;
        }
        self.jog = Some(DEFAULT_JOG_STEP);
    }

    fn on_jog_key(&mut self, key: KeyEvent) {
        let Some(step_index) = self.jog else {
            return;
        };
        if self.view.job_active() {
            self.jog = None;
            self.warn("Jogging is disabled while a job runs");
            return;
        }
        let step = JOG_STEPS[step_index.min(JOG_STEPS.len() - 1)];
        let xy = self.profile.jog_feedrate_xy;
        let z = self.profile.jog_feedrate_z;
        match key.code {
            KeyCode::Esc | KeyCode::Char('j' | 'q') => self.jog = None,
            KeyCode::Left => self.printer.jog(Axis::X, -step, xy),
            KeyCode::Right => self.printer.jog(Axis::X, step, xy),
            KeyCode::Up => self.printer.jog(Axis::Y, step, xy),
            KeyCode::Down => self.printer.jog(Axis::Y, -step, xy),
            KeyCode::PageUp => self.printer.jog(Axis::Z, step.min(MAX_Z_STEP), z),
            KeyCode::PageDown => self.printer.jog(Axis::Z, -step.min(MAX_Z_STEP), z),
            KeyCode::Char('+' | '=') => self.jog = Some((step_index + 1).min(JOG_STEPS.len() - 1)),
            KeyCode::Char('-' | '_') => self.jog = Some(step_index.saturating_sub(1)),
            KeyCode::Char('h') => self.printer.home(Axes::ALL),
            KeyCode::Char('x') => self.printer.home(Axes::X),
            KeyCode::Char('y') => self.printer.home(Axes::Y),
            KeyCode::Char('z') => self.printer.home(Axes::Z),
            KeyCode::Tab => {
                self.jog = None;
                self.cycle_focus(true);
            }
            _ => {}
        }
    }

    // ----- commands -----

    /// Send G-code typed into the input line. Dangerous codes ask first,
    /// and so does what would ruin a running job (homing, moves).
    pub(crate) fn send_gcode(&mut self, line: &str) {
        if !self.connected_or_warn() {
            return;
        }
        if let Some(doc) = actions::dangerous_gcode(line) {
            self.modal = Some(Modal::Confirm(Confirm::new(
                format!("Send {}?", doc.code),
                vec![
                    format!("{}: {}", doc.code, doc.name),
                    doc.description.to_string(),
                ],
                "Send",
                "Don't send",
                true,
                Action::Send(line.to_string()),
            )));
            return;
        }
        let conflict = self.view.job.as_ref().and_then(|job| {
            let paused = job.status == JobStatus::Paused;
            let (doc, why) =
                actions::job_conflict(line, paused).filter(|_| job.status.is_active())?;
            let state = if paused { "paused" } else { "printing" };
            Some((doc.code, format!("{} is {state}: {why}.", job.name)))
        });
        if let Some((code, why)) = conflict {
            self.modal = Some(Modal::Confirm(Confirm::new(
                format!("Send {code} during the job?"),
                vec![why, "The print will most likely fail.".to_string()],
                "Send anyway",
                "Don't send",
                true,
                Action::Send(line.to_string()),
            )));
            return;
        }
        self.printer.send(line);
    }

    /// Run one palette line: a command or G-code.
    pub(crate) fn run_line(&mut self, line: &str) {
        match commands::parse(line) {
            Ok(Input::Empty) => {}
            Ok(Input::Gcode(gcode)) => self.send_gcode(&gcode),
            Ok(Input::Command(cmd)) => self.execute(cmd),
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Run a shell command.
    pub(crate) fn execute(&mut self, cmd: Command) {
        match cmd {
            Command::Help(None) => self.modal = Some(Modal::Help { scroll: 0 }),
            Command::Help(Some(topic)) => {
                if let Some(spec) = command_spec(&topic) {
                    self.show_report(&format!("Help: {}", spec.name), report::command_help(spec));
                } else if let Some(doc) = gcode_doc(&topic) {
                    self.show_report(&format!("G-code {}", doc.code), report::gcode_help(doc));
                } else if let Some(def) = setting_def(&topic) {
                    self.show_report(
                        &format!("Setting {}", def.code),
                        vec![
                            ReportLine::heading(format!("{}  {}", def.code, def.name)),
                            ReportLine::toned(def.description, Tone::Dim),
                        ],
                    );
                } else {
                    self.error(format!("No help for '{topic}'"));
                }
            }
            Command::Connect { target, baud, save } => {
                match actions::resolve_target(target.as_deref(), baud, &self.profile) {
                    Ok(target) => {
                        if save {
                            self.save_connection(&target);
                        }
                        self.run_action(Action::Connect(target));
                    }
                    Err(why) => self.error(why),
                }
            }
            Command::Profile(None) => {
                let lines = report::profile_lines(&self.config);
                self.show_report("Printer profiles", lines);
            }
            Command::Eta => {
                let job = self.view.job.as_ref().map(|j| {
                    (
                        j.status.is_active(),
                        format!(
                            "{} {}  {}",
                            report::job_status_text(&j.status),
                            j.name,
                            report::job_progress_text(j)
                        ),
                    )
                });
                match job {
                    Some((true, text)) => self.info(text),
                    Some((false, text)) => self.info(format!("No job is running (last: {text})")),
                    None => self.warn("No job is running"),
                }
            }
            Command::Source(path) => self.read_source(path),
            Command::WaitOnline => self.info(if self.view.connected() {
                "The printer is online"
            } else {
                "block_until_online is for shell scripts; the status bar shows the connection"
            }),
            Command::Profile(Some(name)) => self.switch_profile(&name),
            Command::Disconnect => self.run_action(Action::Disconnect),
            Command::Ports => self.show_report("Serial ports", report::ports_lines(&list_ports())),
            Command::Load(path) => {
                let path = crate::paths::resolve(&path.to_string_lossy(), &self.base_dir);
                self.open_file(path);
            }
            Command::Info => match &self.file {
                Some(f) => {
                    let lines = report::file_lines(f);
                    let title = f.name().to_string();
                    self.show_report(&title, lines);
                }
                None => self.warn("No file loaded (F3 opens one)"),
            },
            Command::Print => self.request_print(),
            Command::Pause => {
                if self.view.job_active() {
                    self.printer.pause();
                } else {
                    self.warn("No job is running");
                }
            }
            Command::Resume => {
                if self.continue_user_wait() {
                } else if self.view.job_active() {
                    self.resume();
                } else {
                    self.warn("No job is running");
                }
            }
            Command::Cancel => self.request_cancel(),
            Command::Status => {
                let lines = self
                    .printer
                    .with_state(|s| report::status_lines(s, self.file.as_ref()));
                self.show_report("Status", lines);
            }
            Command::Monitor(_) => self.info("This screen is the live monitor"),
            Command::Log(on) => {
                if let Some(on) = on {
                    self.show_auto = on;
                }
                self.info(if self.show_auto {
                    "Console shows all traffic"
                } else {
                    "Console hides automatic traffic"
                });
            }
            Command::Exit => self.request_quit(),
            Command::Macro(None) => {
                let lines = report::macro_lines(&self.config.macros);
                self.show_report("Macros", lines);
            }
            Command::MacroDefine { name, gcode, edit } => self.define_macro(&name, gcode, edit),
            Command::MacroRemove(name) => {
                match self
                    .config
                    .macros
                    .iter()
                    .find(|m| m.name.eq_ignore_ascii_case(name.trim()))
                {
                    Some(m) => {
                        let name = m.name.clone();
                        self.modal = Some(Modal::Confirm(Confirm::new(
                            format!("Delete the macro {name}?"),
                            macro_preview(&m.gcode),
                            "Delete",
                            "Keep",
                            true,
                            Action::DeleteMacro(name),
                        )));
                    }
                    None => self.error(format!("no macro '{name}' (macro lists them)")),
                }
            }
            Command::Preheat(None) => {
                let lines = report::preset_lines(&self.profile.temp_presets);
                self.show_report("Presets", lines);
            }
            other => {
                if self.connected_or_warn() {
                    self.execute_on_printer(other);
                }
            }
        }
    }

    /// Commands that need a connection.
    fn execute_on_printer(&mut self, cmd: Command) {
        let printer = self.printer.clone();
        match cmd {
            Command::Temp { target, tool } => {
                match actions::resolve_temp(&target, &self.profile.temp_presets, false) {
                    Ok(celsius) => {
                        let tool = tool.unwrap_or(self.view.active_tool);
                        self.run_action(Action::SetTemps(vec![(Heater::Hotend(tool), celsius)]));
                    }
                    Err(why) => self.error(why),
                }
            }
            Command::BedTemp(target) => {
                match actions::resolve_temp(&target, &self.profile.temp_presets, true) {
                    Ok(celsius) => self.run_action(Action::SetTemps(vec![(Heater::Bed, celsius)])),
                    Err(why) => self.error(why),
                }
            }
            Command::GetTemp => {
                let lines = printer.with_state(report::temperature_lines);
                self.show_report("Temperatures", lines);
            }
            Command::Off => match self.view.job.as_ref().filter(|j| j.status.is_active()) {
                Some(job) => {
                    let name = job.name.clone();
                    self.modal = Some(Modal::Confirm(Confirm::new(
                        "Turn everything off?",
                        vec![
                            format!("{name} is printing."),
                            "Heaters, fans and motors off ruins it.".to_string(),
                        ],
                        "Turn off",
                        "Keep printing",
                        true,
                        Action::EverythingOff,
                    )));
                }
                None => self.run_action(Action::EverythingOff),
            },
            Command::Tool(t) => {
                if self.motion_allowed() {
                    printer.select_tool(t);
                    self.info(format!("Tool T{t}"));
                }
            }
            Command::Reset => {
                if self.view.job_active() {
                    self.warn("Not while a job runs: resetting the board stops it (F7 cancels)");
                } else if let Some(target) = self.last_target.clone() {
                    self.connect_with(target, true);
                }
            }
            Command::Preheat(Some(name)) => {
                match actions::find_preset(&self.profile.temp_presets, &name) {
                    Ok(preset) => {
                        let tool = self.view.active_tool;
                        let targets = vec![
                            (Heater::Hotend(tool), preset.hotend),
                            (Heater::Bed, preset.bed),
                        ];
                        let name = preset.name.clone();
                        self.run_action(Action::SetTemps(targets));
                        if self.modal.is_none() {
                            self.info(format!("Preheating for {name}"));
                        }
                    }
                    Err(why) => self.error(why),
                }
            }
            Command::Cooldown => {
                let heaters = printer.with_state(|s| actions::heaters(s, &self.profile));
                self.run_action(Action::SetTemps(
                    heaters.into_iter().map(|h| (h, 0.0)).collect(),
                ));
            }
            Command::Home { axes, zero_e } => {
                if self.motion_allowed() {
                    if axes != Axes::default() {
                        printer.home(axes);
                    }
                    if zero_e {
                        printer.send("G92 E0");
                        self.info("Extruder position set to 0 (G92 E0)");
                    }
                }
            }
            Command::Move { axis, mm, feedrate } => {
                if self.motion_allowed() {
                    let feed = feedrate.unwrap_or(match axis {
                        Axis::Z => self.profile.jog_feedrate_z,
                        Axis::E => self.profile.extrude_feedrate,
                        _ => self.profile.jog_feedrate_xy,
                    });
                    printer.jog(axis, mm, feed);
                }
            }
            Command::Extrude { mm, feedrate } => {
                if self.motion_allowed() {
                    let feed = feedrate.unwrap_or(self.profile.extrude_feedrate);
                    printer.extrude(self.view.active_tool, mm, feed);
                }
            }
            Command::Fan(pct) => {
                printer.set_fan(0, actions::fan_value(pct));
                self.info(format!("Fan {}%", format::trimmed(f64::from(pct), 0)));
            }
            Command::Speed(pct) => printer.set_feedrate_override(pct),
            Command::Flow(pct) => printer.set_flow(self.view.active_tool, pct),
            Command::Babystep(mm) => printer.babystep_z(mm),
            Command::Settings(filter) => {
                printer.request_settings();
                let pending = Pending::new(REPORT_TIMEOUT, false);
                self.open_report("Settings", ReportSource::Settings(filter), pending);
            }
            Command::Set(edit) => {
                let merged = printer.with_state(|s| actions::merged_setting(&s.settings, &edit));
                printer.apply_setting(&merged);
                self.info(format!("Sent {} (eeprom save keeps it)", merged.to_gcode()));
            }
            Command::Eeprom(Eeprom::Save) => {
                printer.save_settings();
                self.good("Settings saved to EEPROM (M500)");
            }
            Command::Eeprom(Eeprom::Load) => {
                printer.load_settings();
                self.info("Settings reloaded from EEPROM (M501)");
            }
            Command::Eeprom(Eeprom::Reset { .. }) => {
                self.modal = Some(Modal::Confirm(Confirm::new(
                    "Factory reset?",
                    vec![
                        "M502 replaces every setting with the firmware defaults.".to_string(),
                        "They stay in RAM until eeprom save.".to_string(),
                    ],
                    "Reset",
                    "Keep settings",
                    true,
                    Action::EepromReset,
                )));
            }
            Command::Firmware => {
                printer.request_firmware_info();
                let pending = Pending::new(REPORT_TIMEOUT, false);
                self.open_report("Firmware", ReportSource::Firmware, pending);
            }
            Command::Endstops => {
                printer.request_endstops();
                let pending = Pending::new(REPORT_TIMEOUT, false);
                self.open_report("Endstops", ReportSource::Endstops, pending);
            }
            Command::Mesh { probe } => {
                if probe {
                    if !self.motion_allowed() {
                        return;
                    }
                    printer.probe_bed_mesh();
                } else {
                    printer.request_bed_mesh();
                }
                // Probing takes minutes; without a mesh, M420 V reports
                // none.
                let pending = if probe {
                    Pending::new(PROBE_TIMEOUT, false)
                } else {
                    Pending::new(REPORT_TIMEOUT, true)
                };
                self.open_report("Bed mesh", ReportSource::Mesh, pending);
            }
            Command::Stats => {
                printer.request_stats();
                // Firmware without PRINTCOUNTER answers M78 with no data.
                let pending = Pending::new(REPORT_TIMEOUT, true);
                self.open_report("Statistics", ReportSource::Stats, pending);
            }
            Command::Sd(SdCommand::List) => {
                printer.request_sd_files();
                // Without a card the listing never comes.
                let pending = Pending::new(REPORT_TIMEOUT, true);
                self.open_report("SD card", ReportSource::SdFiles, pending);
            }
            Command::Sd(SdCommand::Print(name)) => {
                self.modal = Some(Modal::Confirm(Confirm::new(
                    "Print from the SD card?",
                    vec![format!("Start printing {name} from the printer's card.")],
                    "Print",
                    "Not now",
                    false,
                    Action::SdPrint(name),
                )));
            }
            Command::Sd(SdCommand::Remove(name)) => {
                if let Some(why) = printer.with_state(actions::sd_delete_blocker) {
                    self.warn(why);
                    return;
                }
                self.modal = Some(Modal::Confirm(Confirm::new(
                    "Delete from the SD card?",
                    vec![format!("{name} is deleted from the printer's card (M30).")],
                    "Delete",
                    "Keep",
                    true,
                    Action::SdDelete(name),
                )));
            }
            Command::Sd(SdCommand::Upload {
                file: Some(path),
                name,
            }) => {
                // Reading a large file takes a while: off the UI thread.
                let path = crate::paths::resolve(&path.to_string_lossy(), &self.base_dir);
                let shown = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                self.info(format!("Reading {shown}…"));
                self.spawn_background("sprint-upload", move || {
                    let file = crate::loader::read_file(&path)?;
                    let sd_name = name.unwrap_or_else(|| actions::sd_name(&path));
                    upload(&printer, std::sync::Arc::new(file), &sd_name)
                });
            }
            Command::Sd(SdCommand::Upload { file: None, name }) => {
                let Some(loaded) = &self.file else {
                    self.warn("No file loaded (F3 opens one)");
                    return;
                };
                let sd_name = name.unwrap_or_else(|| actions::sd_name(&loaded.path));
                match upload(&printer, loaded.file.clone(), &sd_name) {
                    Ok(text) => self.info(text),
                    Err(why) => self.error(why),
                }
            }
            Command::Autotune {
                heater,
                celsius,
                cycles,
            } => {
                if self.view.job_active() {
                    self.warn("Autotune is disabled while a job runs");
                    return;
                }
                printer.start_pid_autotune(heater, celsius, cycles);
                self.info(format!(
                    "PID autotune of {} at {} °C, {cycles} cycles",
                    heater.name(),
                    format::trimmed(f64::from(celsius), 1)
                ));
            }
            Command::Macro(Some(text)) => match actions::macro_call(&self.config.macros, &text)
                .and_then(|(m, args)| actions::macro_gcode(m, &args).map(|gcode| (m, args, gcode)))
            {
                // While a job runs the commands go between the print's
                // lines (a G28 there ruins it): always asked, as danger.
                Ok((m, args, gcode)) if m.confirm || self.view.job_active() => {
                    let name = m.name.clone();
                    let job_active = self.view.job_active();
                    let mut message = Vec::new();
                    if job_active {
                        message.push(
                            "A job is running: these commands go between the print's lines."
                                .to_string(),
                        );
                    }
                    message.extend(macro_preview(&gcode));
                    self.modal = Some(Modal::Confirm(Confirm::new(
                        format!("Run {name}?"),
                        message,
                        "Run",
                        "Not now",
                        job_active,
                        Action::RunMacro(name, args),
                    )));
                }
                Ok((m, args, _)) => {
                    let name = m.name.clone();
                    self.run_action(Action::RunMacro(name, args));
                }
                Err(why) => self.error(why),
            },
            // Handled before a connection is needed.
            Command::Help(_)
            | Command::Connect { .. }
            | Command::Profile(_)
            | Command::MacroDefine { .. }
            | Command::MacroRemove(_)
            | Command::Eta
            | Command::Source(_)
            | Command::WaitOnline
            | Command::Disconnect
            | Command::Ports
            | Command::Load(_)
            | Command::Info
            | Command::Print
            | Command::Pause
            | Command::Resume
            | Command::Cancel
            | Command::Status
            | Command::Monitor(_)
            | Command::Macro(None)
            | Command::Preheat(None)
            | Command::Log(_)
            | Command::Exit => {}
        }
    }

    /// `macro define|edit <name> <gcode>`: save a macro (`|` separates its
    /// lines; the palette has one line).
    fn define_macro(&mut self, name: &str, gcode: Option<String>, edit: bool) {
        let existing = self
            .config
            .macros
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .map(|m| m.name.clone());
        if edit && existing.is_none() {
            self.error(format!("no macro '{name}' (macro define makes one)"));
            return;
        }
        let Some(gcode) = gcode else {
            self.warn(format!(
                "Give the G-code on the line: macro define {name} G28 | G1 Z10 (| separates lines)"
            ));
            return;
        };
        let name = existing.unwrap_or_else(|| name.to_string());
        let mut replaced = false;
        let saved =
            crate::config::edit_config(self.config_path.as_deref(), &self.config, |config| {
                replaced = actions::define_macro(&mut config.macros, &name, &gcode);
            });
        match saved {
            Ok(config) => {
                self.keep_profile(config);
                let lines = gcode.lines().count();
                self.good(format!(
                    "{} macro {name} ({lines} line{})",
                    if replaced { "Changed" } else { "Saved" },
                    if lines == 1 { "" } else { "s" }
                ));
            }
            Err(why) => self.error(format!("{name} not saved: {why}")),
        }
    }

    /// Use `config` (just saved) and keep this session's profile active.
    fn keep_profile(&mut self, mut config: AppConfig) {
        if let Some(index) = config
            .profiles
            .iter()
            .position(|p| p.name == self.profile.name)
        {
            config.active_profile = index;
        }
        self.config = config;
    }

    /// `profile <name>`: use another printer profile from now on, saved
    /// as the active one.
    fn switch_profile(&mut self, name: &str) {
        if self.view.job_active() {
            self.warn("Not while a job runs (the job uses the profile's scripts)");
            return;
        }
        let index = match actions::find_profile(&self.config.profiles, name) {
            Ok(index) => index,
            Err(why) => {
                self.error(why);
                return;
            }
        };
        let wanted = self.config.profiles[index].name.clone();
        let saved = crate::config::edit_profile(
            self.config_path.as_deref(),
            &self.config,
            &wanted,
            |config, i| config.active_profile = i,
        );
        let session_only = match saved {
            Ok((config, i)) => {
                self.use_config(config, i);
                None
            }
            Err(why) => {
                let config = self.config.clone();
                self.use_config(config, index);
                Some(why)
            }
        };
        let target = self.profile.connection.to_string();
        let name = self.profile.name.clone();
        match session_only {
            Some(why) => self.warn(format!("Using {name} ({target}) for this session: {why}")),
            None if self.view.status.is_open() => self.good(format!(
                "Using the profile {name}; still connected (F2 connects to {target})"
            )),
            None => self.good(format!("Using the profile {name} ({target})")),
        }
    }

    /// `connect <target> --save`: make `target` the active profile's
    /// printer in the configuration file.
    fn save_connection(&mut self, target: &ConnectTarget) {
        let name = self.profile.name.clone();
        let saved = target.clone();
        match crate::config::edit_profile(
            self.config_path.as_deref(),
            &self.config,
            &name,
            |config, i| {
                if let Some(profile) = config.profiles.get_mut(i) {
                    profile.connection = saved;
                }
            },
        ) {
            Ok((config, i)) => {
                self.use_config(config, i);
                self.info(format!("Saved {target} as the printer of {name}"));
            }
            Err(why) => self.error(format!("Not saved: {why}")),
        }
    }

    /// Use `config` with its profile `index` active (in this session).
    fn use_config(&mut self, mut config: AppConfig, index: usize) {
        let Some(profile) = config.profiles.get(index).cloned() else {
            return;
        };
        config.active_profile = index;
        self.printer.set_profile(profile.clone());
        self.profile = profile;
        self.config = config;
    }

    /// Manual moves are refused while a job runs.
    fn motion_allowed(&mut self) -> bool {
        if self.view.job_active() {
            self.warn("Not while a job runs");
            false
        } else {
            true
        }
    }
}

/// The command lines of pasted text: trimmed, without blank and
/// comment-only lines.
fn pasted_commands(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.split(';').next().unwrap_or_default().trim().is_empty())
        .collect()
}

/// Lines of macro G-code shown when asking to run it.
const MACRO_PREVIEW_LINES: usize = 12;

/// The first lines of a macro's G-code, and how many more there are.
fn macro_preview(gcode: &str) -> Vec<String> {
    let lines: Vec<&str> = gcode
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut out: Vec<String> = lines
        .iter()
        .take(MACRO_PREVIEW_LINES)
        .map(|l| (*l).to_string())
        .collect();
    if lines.len() > MACRO_PREVIEW_LINES {
        out.push(format!(
            "… {} more lines",
            lines.len() - MACRO_PREVIEW_LINES
        ));
    }
    out
}

/// Start writing `file` to the SD card as `sd_name`: the status message.
fn upload(
    printer: &Printer,
    file: std::sync::Arc<sprint_host::sprint_gcode::GcodeFile>,
    sd_name: &str,
) -> Result<String, String> {
    printer
        .upload_to_sd(file, sd_name)
        .map(|()| format!("Uploading to the SD card as {sd_name}"))
        .map_err(|e| format!("Cannot upload: {e}"))
}

/// The lines of a report from the current printer state.
fn report_lines(printer: &Printer, source: &ReportSource) -> Vec<ReportLine> {
    printer.with_state(|s| match source {
        ReportSource::Settings(filter) => report::settings_lines(&s.settings, filter.as_deref()),
        ReportSource::Firmware => report::firmware_lines(s),
        ReportSource::Endstops => report::endstop_lines(&s.endstops),
        ReportSource::Mesh => s.mesh.as_ref().map_or_else(
            || vec![ReportLine::toned("No bed mesh reported.", Tone::Dim)],
            report::mesh_lines,
        ),
        ReportSource::Stats => report::stats_lines(s.stats.as_ref(), &s.link),
        ReportSource::SdFiles => report::sd_lines(&s.sd),
    })
}
