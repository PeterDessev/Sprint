//! The pronsole-style line shell: one command or G-code line at a time,
//! with what the printer says printed above the prompt.
//!
//! On a terminal the line editor keeps a history (next to the
//! configuration file) and completes commands, G-codes, file names, ports
//! and presets. Piped input runs as a script: no prompt, no questions
//! (`eeprom reset` then needs `yes`), and at the end of the input the
//! shell waits for the queue to drain and for a running print to finish.
//! A print the script paused (and did not resume) is cancelled instead:
//! nothing could resume it any more.

mod echo;
mod input;
mod output;
#[cfg(test)]
mod tests;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use sprint_host::sprint_proto::{JobContext, gcode_doc, setting_def};
use sprint_host::{
    AppConfig, Axes, Axis, ConnectTarget, ConnectionStatus, Heater, JobKind, Printer,
    PrinterProfile, list_ports,
};

use crate::TuiOptions;
use crate::actions::{self, Motion};
use crate::commands::{self, Command, Eeprom, Input, SdCommand, command_spec};
use crate::format;
use crate::interrupt::Interrupt;
use crate::loader::{self, LoadedFile};
use crate::report::{self, ReportLine, Tone};
use crate::session::{self, ConnectError, ResetError, WaitError};

use echo::{Awaited, Echo};
use input::{LineSource, Read, Script, ShellHelper, Stdin, Terminal};
use output::{Out, Sink, WriterSink};

/// How long a report waits for the printer.
const REPORT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long probing the bed may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(600);
/// How long the setup queries after a connect may take.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the end of a script waits for queued commands.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(600);
/// How long leaving waits for a cancelled print to stop.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the end of a script gives a `pause` asked just before it to
/// take effect.
const PAUSE_SETTLE: Duration = Duration::from_secs(2);
/// Default `monitor` interval.
const MONITOR_INTERVAL: f32 = 2.0;
/// How deep `source` files may run each other.
const MAX_SOURCE_DEPTH: usize = 8;

/// Run the line shell until `exit` or the end of the input.
///
/// `options.file` is loaded first; the shell connects to `options.connect`,
/// or to the active profile's printer only when the profile enables
/// `auto_connect`. With `options.execute` those commands run as a script
/// instead of the input.
pub fn run_shell(mut options: TuiOptions) -> anyhow::Result<()> {
    let (mut config, config_path, report) = crate::config::load_config();
    crate::config::select_profile(&mut config, options.profile.as_deref())
        .map_err(anyhow::Error::msg)?;
    let commands = std::mem::take(&mut options.execute);
    let interactive =
        commands.is_empty() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let support = report::stdout_colors();
    let base_dir = std::env::current_dir().unwrap_or_default();
    let (mut source, sink): (Box<dyn LineSource>, Box<dyn Sink>) = if !commands.is_empty() {
        (
            Box::new(Script::new(commands, false)),
            Box::new(WriterSink(std::io::stdout())),
        )
    } else if interactive {
        let helper = ShellHelper {
            presets: config
                .active()
                .temp_presets
                .iter()
                .map(|p| p.name.clone())
                .collect(),
            macros: config.macros.iter().map(|m| m.name.clone()).collect(),
            base: base_dir.clone(),
            support,
        };
        let history = Some(crate::config::history_path(&config_path));
        let mut terminal = Terminal::new(helper, history)?;
        let sink = terminal
            .sink()
            .unwrap_or_else(|_| Box::new(WriterSink(std::io::stdout())));
        (Box::new(terminal), sink)
    } else {
        (
            Box::new(Stdin::new()),
            Box::new(WriterSink(std::io::stdout())),
        )
    };
    let out = Out::new(sink, support, interactive);
    // What the configuration file lost is said first, scripts included.
    for message in crate::config::report_messages(&config_path, &report) {
        out.warn(message);
    }
    let printer = Printer::with_console_capacity(config.console_max_lines);
    let mut shell = Shell::new(
        printer,
        config,
        Some(config_path),
        out,
        crate::interrupt::global(),
    );
    shell.base_dir = base_dir;
    if interactive {
        shell.out.line(
            ReportLine::toned("Sprint shell ", Tone::Accent)
                .with(env!("CARGO_PKG_VERSION"), Tone::Accent)
                .with(
                    "  help lists the commands, G-code is sent as typed, exit leaves",
                    Tone::Dim,
                ),
        );
    }
    shell.start(options);
    shell.run(source.as_mut());
    source.save_history();
    Ok(())
}

/// How [`Shell::safe_to_send`] words its question and refusals.
struct SendCheck<'a> {
    /// The question after the reasons ("Send it?", "Run Home?").
    question: &'a str,
    /// Starts the refusal of a blocked line ("Not sent").
    refused: &'a str,
    /// Said when the answer is no.
    declined: &'a str,
    /// Asked even when the classifier lets everything through (a macro
    /// marked to ask first).
    always_ask: Option<&'a str>,
    /// Typed with `!`: sent without a question (a blocked line is still
    /// refused).
    raw: bool,
}

/// Whether the shell goes on after a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    Continue,
    Exit,
}

/// The shell's state.
pub(crate) struct Shell {
    printer: Printer,
    config: AppConfig,
    /// Where to save the configuration (recent files); `None` in tests.
    config_path: Option<PathBuf>,
    profile: PrinterProfile,
    file: Option<LoadedFile>,
    out: Out,
    echo: Echo,
    interrupt: Interrupt,
    base_dir: PathBuf,
    /// How deep `source` files are nested.
    sourcing: usize,
    /// When `pause` asked to pause the job (nothing in the shell resumed
    /// it since).
    pause_asked: Option<Instant>,
    /// Pause points (`print --pause-at-layer`) still ahead when the print
    /// started or was last resumed: a pause at one of them is the
    /// script's, like `pause`.
    layer_pauses_ahead: usize,
    /// Axes asked to be homed whose G28 may not be acknowledged yet.
    homing: std::cell::Cell<actions::HomingRequests>,
}

impl Shell {
    pub(crate) fn new(
        printer: Printer,
        config: AppConfig,
        config_path: Option<PathBuf>,
        out: Out,
        interrupt: Interrupt,
    ) -> Shell {
        let profile = config.active().clone();
        printer.set_profile(profile.clone());
        let echo = Echo::start(&printer, out.clone());
        Shell {
            printer,
            config,
            config_path,
            profile,
            file: None,
            out,
            echo,
            interrupt,
            base_dir: std::env::current_dir().unwrap_or_default(),
            sourcing: 0,
            pause_asked: None,
            layer_pauses_ahead: 0,
            homing: std::cell::Cell::default(),
        }
    }

    /// Load the startup file and connect as asked (or as the profile
    /// says).
    pub(crate) fn start(&mut self, options: TuiOptions) {
        if let Some(file) = options.file {
            self.load(&file);
        }
        if let Some(target) = actions::startup_target(options.connect, &self.profile) {
            self.connect(target);
        }
    }

    /// Read and run lines until `exit` or the end of the input, then
    /// disconnect. A panic (a bug) goes on only after a print the shell
    /// streams has been cancelled, so that its cancel script turns the
    /// heaters off before the link closes, as `exit` does.
    pub(crate) fn run(&mut self, input: &mut dyn LineSource) {
        let printer = self.printer.clone();
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.read_and_run(input);
        }));
        if let Err(panic) = ended {
            let _ = session::cancel_before_closing(&printer, session::CANCEL_WAIT, |name| {
                eprintln!("Cancelling {name} before closing the connection…");
            });
            printer.disconnect();
            std::panic::resume_unwind(panic);
        }
        self.printer.disconnect();
        self.echo.settle();
    }

    /// Read and run lines until `exit` or the end of the input.
    fn read_and_run(&mut self, input: &mut dyn LineSource) {
        loop {
            let prompt = self.prompt();
            match input.read(&prompt) {
                Read::Line(text) => {
                    if self.execute_input(&text, input) == Flow::Exit {
                        break;
                    }
                }
                Read::Interrupted => self
                    .out
                    .dim("(Ctrl+C) exit leaves the shell; cancel stops a print"),
                Read::Eof => {
                    if self.leave(input) {
                        break;
                    }
                }
            }
        }
    }

    /// The prompt: the connection and job state at a glance.
    pub(crate) fn prompt(&self) -> String {
        self.printer.with_state(|s| {
            if let Some(job) = s.job.as_ref().filter(|j| j.status.is_active()) {
                return format!(
                    "sprint [{} {}]> ",
                    job.status.label().to_lowercase(),
                    format::percent(f64::from(job.progress))
                );
            }
            match &s.status {
                ConnectionStatus::Connected if s.halted => "sprint (halted)> ".to_string(),
                ConnectionStatus::Connected => "sprint> ".to_string(),
                ConnectionStatus::Connecting | ConnectionStatus::Handshaking => {
                    "sprint (connecting)> ".to_string()
                }
                ConnectionStatus::Disconnected | ConnectionStatus::Failed(_) => {
                    "sprint (offline)> ".to_string()
                }
            }
        })
    }

    /// Run what the line editor returned: one line, or several when a
    /// block was pasted. Each line runs (and enters the history) as if it
    /// was typed, so a dangerous one still asks and a command is not
    /// handed the next lines as arguments.
    pub(crate) fn execute_input(&mut self, text: &str, input: &mut dyn LineSource) -> Flow {
        for line in text.split(['\n', '\r']) {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                input.add_history(trimmed);
            }
            if self.execute_line(line, input) == Flow::Exit {
                return Flow::Exit;
            }
        }
        Flow::Continue
    }

    /// Run one line: a command, G-code, or nothing.
    pub(crate) fn execute_line(&mut self, line: &str, input: &mut dyn LineSource) -> Flow {
        match commands::parse(line) {
            Ok(Input::Empty) => Flow::Continue,
            Ok(Input::Gcode(gcode)) => {
                let raw = line.trim_start().starts_with('!');
                self.send_gcode(&gcode, raw, input);
                Flow::Continue
            }
            Ok(Input::Command(cmd)) => self.execute(cmd, input),
            Err(e) => {
                self.out.error(e.to_string());
                Flow::Continue
            }
        }
    }

    // ----- helpers -----

    /// Ask a yes/no question (interactive only; scripts get `default`).
    fn confirm(&self, input: &mut dyn LineSource, question: &str, default: bool) -> bool {
        if !input.interactive() {
            return default;
        }
        match input.read(&format!("{question} [y/N] ")) {
            Read::Line(answer) => {
                matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
            }
            Read::Interrupted | Read::Eof => false,
        }
    }

    fn connected_or_warn(&self) -> bool {
        if self.printer.is_connected() {
            return true;
        }
        let connecting = self.printer.with_state(|s| s.status.is_open());
        self.out.warn(if connecting {
            "The printer is still connecting"
        } else {
            "Not connected (connect sim, connect COM3@115200 or connect tcp://host:port; ports lists serial ports)"
        });
        false
    }

    fn job_active(&self) -> bool {
        self.printer
            .with_state(sprint_host::PrinterState::job_active)
    }

    /// Whether a manual motion command may run now: refused while a job
    /// prints; while a host print is paused, as the classifier's rules say
    /// (moves and extrusion go, homing Z and probing ask first on a
    /// terminal and are not done by a script).
    fn motion_allowed(&self, motion: &Motion, input: &mut dyn LineSource) -> bool {
        if self.printer.with_state(actions::job_context) == JobContext::Printing {
            self.out.warn(actions::NOT_WHILE_PRINTING);
            return false;
        }
        let check = SendCheck {
            question: "Do it?",
            refused: "Not done",
            declined: "Not done",
            always_ask: None,
            raw: false,
        };
        self.safe_to_send(&motion.gcode(), &check, input)
    }

    /// Run a manual motion command if [`Shell::motion_allowed`].
    fn motion(&self, motion: Motion, input: &mut dyn LineSource) -> bool {
        let allowed = self.motion_allowed(&motion, input);
        if allowed {
            if let Some(warning) = self.homing_warning(motion.moves_axes(), actions::UNHOMED_MOVE) {
                self.out.warn(warning);
            }
            motion.run(&self.printer);
            self.sent_homing(&motion.gcode());
        }
        allowed
    }

    /// [`actions::homing_warning`] for `axes`, counting the homing just
    /// asked for.
    fn homing_warning(&self, axes: Axes, consequence: &str) -> Option<String> {
        let mut requests = self.homing.get();
        let warning = self.printer.with_state(|s| {
            let pending = requests.pending(s);
            actions::homing_warning(s, axes, pending, consequence)
        });
        self.homing.set(requests);
        warning
    }

    /// Remember what `gcode`, just sent, homes.
    fn sent_homing(&self, gcode: &str) {
        let mut requests = self.homing.get();
        requests.sent(gcode);
        self.homing.set(requests);
    }

    /// Wait for an expected answer: Ok when it arrived, or why not.
    fn wait_answer(&self, rx: &Receiver<()>, timeout: Duration) -> Result<(), WaitError> {
        let deadline = Instant::now().checked_add(timeout);
        let result = loop {
            if rx.recv_timeout(session::POLL * 5).is_ok() {
                break Ok(());
            }
            if self.interrupt.take() {
                break Err(WaitError::Interrupted);
            }
            if !self.printer.with_state(|s| s.status.is_open()) {
                break Err(WaitError::Disconnected(String::new()));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                break Err(WaitError::TimedOut);
            }
        };
        if result.is_err() {
            self.echo.cancel_expect();
        }
        result
    }

    /// Send a request and wait for its answer; false when there is nothing
    /// to show.
    fn request(
        &self,
        what: Awaited,
        idle_completes: bool,
        timeout: Duration,
        send: impl FnOnce(&Printer),
    ) -> bool {
        self.interrupt.reset();
        let rx = self.echo.expect(what, idle_completes);
        send(&self.printer);
        match self.wait_answer(&rx, timeout) {
            Ok(()) => true,
            Err(WaitError::TimedOut) => {
                self.out.warn(format!(
                    "The printer did not answer within {}; showing what is known",
                    format::duration(timeout.as_secs_f64())
                ));
                true
            }
            Err(WaitError::Interrupted) => {
                self.out.warn("Stopped waiting");
                false
            }
            Err(WaitError::Disconnected(_)) => {
                self.out.error("The connection closed");
                false
            }
        }
    }

    // ----- commands -----

    /// Connect to `target` (disconnecting first) and wait for the setup.
    pub(crate) fn connect(&mut self, target: ConnectTarget) {
        if self.job_active() {
            self.out
                .warn("A job is running; cancel it (or disconnect) before connecting elsewhere");
            return;
        }
        if self.printer.with_state(|s| s.status.is_open()) {
            self.printer.disconnect();
        }
        let label = target.label();
        // Quiet: the setup replies (M115, M503, ...) are not echoed.
        let _quiet = self.echo.expect(Awaited::Connect, false);
        self.interrupt.reset();
        let options = self.profile.options.clone();
        let result = session::connect_blocking(&self.printer, target, options, &self.interrupt);
        if result.is_ok() {
            let _ = session::wait_setup(&self.printer, SETUP_TIMEOUT, &self.interrupt);
        }
        self.echo.settle();
        self.echo.cancel_expect();
        match result {
            // The printer's console said "Connected to ..."; add what it
            // runs.
            Ok(()) => {
                let (firmware, machine) = self.printer.with_state(|s| {
                    let fw = s.firmware.as_ref();
                    (
                        fw.map(|f| f.name.clone()),
                        fw.and_then(|f| f.machine_type.clone()),
                    )
                });
                let mut line = ReportLine::toned("Ready", Tone::Good);
                match firmware {
                    Some(fw) => line.push(format!("  {fw}"), Tone::Plain),
                    None => line.push(format!("  {label} (no firmware report)"), Tone::Dim),
                }
                if let Some(machine) = machine {
                    line.push(format!(" · {machine}"), Tone::Dim);
                }
                self.out.line(line);
            }
            // The printer logged why the link did not open.
            Err(ConnectError::Open(_)) => {
                self.out
                    .dim("(ports lists the serial ports; connect sim uses the virtual printer)");
            }
            Err(e) => self.out.error(e.to_string()),
        }
    }

    /// Load and analyze a file in the foreground (Ctrl+C cancels).
    pub(crate) fn load(&mut self, path: &Path) {
        let path = crate::paths::resolve(&path.to_string_lossy(), &self.base_dir);
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let options = loader::analyze_options(&self.profile);
        self.interrupt.reset();
        let mut shown = u32::MAX;
        let out = self.out.clone();
        let interrupt = self.interrupt.clone();
        let result = loader::load_with_progress(&path, &options, &mut |p| {
            let percent = (p.clamp(0.0, 1.0) * 100.0) as u32;
            if percent != shown {
                shown = percent;
                out.progress(&format!("Analyzing {name} {percent}%"));
            }
            !interrupt.is_raised()
        });
        self.out.end_progress();
        match result {
            Ok(loaded) => {
                let stats = &loaded.toolpath.stats;
                self.out.line(
                    ReportLine::toned(format!("Loaded {}", loaded.name()), Tone::Good).with(
                        format!(
                            "  {} layers, {} estimated, {} filament",
                            loaded.toolpath.layers.len(),
                            format::duration(f64::from(stats.estimated_time)),
                            format::length_mm(f64::from(stats.filament_length))
                        ),
                        Tone::Dim,
                    ),
                );
                // Saved on top of the file as it is now (see save_recent).
                let saved = self.config_path.as_ref().map(|config_path| {
                    crate::config::save_recent(config_path, loaded.path.clone(), &self.config)
                });
                match saved {
                    Some(Ok(saved)) => self.keep_profile(saved),
                    Some(Err(e)) => {
                        log::warn!("could not save the configuration: {e}");
                        self.config.push_recent(loaded.path.clone());
                    }
                    None => self.config.push_recent(loaded.path.clone()),
                }
                self.file = Some(loaded);
            }
            Err(why) if why == "cancelled" => self.out.warn(format!("Stopped loading {name}")),
            Err(why) => self.out.error(format!("Cannot load: {why}")),
        }
    }

    /// Send G-code as typed, as the shared command-safety classifier
    /// allows ([`actions::check_gcode`]): what it blocks (an SD print or
    /// M28 during a print) is not sent; what needs a confirmation (a
    /// factory reset, homing onto a paused print, a move between the
    /// print's lines) asks first on a terminal, unless the line started
    /// with `!`. Scripts send what is dangerous in itself but nothing that
    /// would ruin a running job. M112 always goes out at once.
    pub(crate) fn send_gcode(&mut self, gcode: &str, raw: bool, input: &mut dyn LineSource) {
        if !self.connected_or_warn() {
            return;
        }
        let check = SendCheck {
            question: "Send it?",
            refused: "Not sent",
            declined: "Not sent (a line starting with ! is sent as typed)",
            always_ask: None,
            raw,
        };
        if self.safe_to_send(gcode, &check, input) {
            self.printer.send(gcode);
            self.sent_homing(gcode);
            // After an emergency stop, say how to get the printer back.
            if actions::check_gcode(gcode, JobContext::Idle).emergency_stop {
                self.out.warn(
                    "Emergency stop sent (M112): reset resets the board \
                     (M999 alone does not recover from M112)",
                );
            }
        }
    }

    /// Whether `gcode` (one or more lines: typed, a macro) may go to the
    /// printer now, as the shared classifier says: refused when it blocks
    /// a line; asked about when one needs a confirmation (or
    /// `check.always_ask` asks anyway): on a terminal, while scripts get
    /// no while a job runs and yes otherwise.
    fn safe_to_send(&self, gcode: &str, check: &SendCheck, input: &mut dyn LineSource) -> bool {
        let (ctx, job) = self
            .printer
            .with_state(|s| (actions::job_context(s), actions::active_job_name(s)));
        let safety = actions::check_gcode(gcode, ctx);
        let sentence = actions::job_sentence(ctx, job.as_deref());
        if safety.is_blocked() {
            let mut text = format!("{}: ", check.refused);
            if let Some(sentence) = &sentence {
                text.push_str(&format!("{sentence} "));
            }
            text.push_str(&safety.reasons().join("; "));
            self.out.error(text);
            return false;
        }
        if check.raw {
            return true;
        }
        let (question, why) = if safety.needs_confirmation() {
            let mut why = String::new();
            if safety.job_related()
                && let Some(sentence) = &sentence
            {
                why.push_str(&format!("{sentence} "));
            }
            why.push_str(safety.reasons().join("; ").trim_end_matches('.'));
            (format!("{why}. {}", check.question), Some(why))
        } else if let Some(always) = check.always_ask {
            (always.to_string(), None)
        } else {
            return true;
        };
        let default = ctx == JobContext::Idle || !safety.needs_confirmation();
        if self.confirm(input, &question, default) {
            return true;
        }
        // A script is not asked: it is told why.
        match why.filter(|_| !input.interactive()) {
            Some(why) => self.out.warn(format!("{}: {why}", check.declined)),
            None => self.out.dim(check.declined),
        }
        false
    }

    /// Run a command.
    pub(crate) fn execute(&mut self, cmd: Command, input: &mut dyn LineSource) -> Flow {
        match cmd {
            Command::Help(topic) => self.help(topic.as_deref()),
            Command::Connect { target, baud, save } => {
                match actions::resolve_target(target.as_deref(), baud, &self.profile) {
                    Ok(target) => {
                        if save {
                            self.save_connection(&target);
                        }
                        self.connect(target);
                    }
                    Err(why) => self.out.error(why),
                }
            }
            Command::Profile(None) => {
                let mut lines = report::profile_lines(&self.config);
                lines.extend(self.config_path.as_deref().map(report::config_file_line));
                self.out.lines(&lines);
            }
            Command::Eta => self.eta(),
            Command::Source(path) => return self.source(&path, input),
            Command::WaitOnline => self.wait_online(),
            Command::Profile(Some(name)) => self.switch_profile(&name),
            Command::Disconnect => {
                let (open, question) = self
                    .printer
                    .with_state(|s| (s.status.is_open(), disconnect_question(s)));
                if !open {
                    self.out.dim("Not connected");
                } else if question.is_none_or(|q| self.confirm(input, &q, true)) {
                    // A streamed print is cancelled first (heaters off).
                    let out = self.out.clone();
                    session::cancel_before_closing(&self.printer, CANCEL_TIMEOUT, |name| {
                        out.dim(format!("Cancelling {name} first…"));
                    });
                    self.printer.disconnect();
                }
            }
            Command::Ports => self.out.lines(&report::ports_lines(&list_ports())),
            Command::Load(path) => self.load(&path),
            Command::Info => match &self.file {
                Some(file) => self.out.lines(&report::file_lines(file)),
                None => self.out.warn("No file loaded (load <file>)"),
            },
            Command::Print { pause_at } => self.print(&pause_at),
            Command::Recover(line) => self.recover(line, input),
            Command::Objects => {
                let file_objects = file_object_names(self.file.as_ref());
                let lines = self
                    .printer
                    .with_state(|s| report::object_lines(s.job.as_ref(), &file_objects));
                self.out.lines(&lines);
            }
            Command::Exclude(name) => self.exclude_object(&name, true),
            Command::Include(name) => self.exclude_object(&name, false),
            Command::Pause => {
                if self.job_active() {
                    self.printer.pause();
                    self.pause_asked = Some(Instant::now());
                    self.out.dim("Pausing…");
                } else {
                    self.out.warn("No job is running");
                }
            }
            Command::Resume => {
                self.pause_asked = None;
                self.layer_pauses_ahead = self
                    .printer
                    .with_state(|s| s.job.as_ref().map_or(0, |j| j.pause_at.len()));
                let (waiting, paused) = self.printer.with_state(|s| {
                    (
                        actions::waiting_for_user(s),
                        s.job
                            .as_ref()
                            .is_some_and(|j| j.status == sprint_host::JobStatus::Paused),
                    )
                });
                if waiting {
                    // M0/M1 or a filament change without a host prompt. A
                    // print the printer paused for it (//action:paused) is
                    // resumed, and the engine sends M108; at once, since
                    // the link stays busy while the firmware holds back the
                    // ok of its M0.
                    if paused {
                        self.printer.resume();
                    } else {
                        self.printer.send("M108");
                    }
                    self.out.dim("Continuing (M108)");
                } else if !self.job_active() {
                    self.out.warn("No job is running");
                } else {
                    self.resume();
                }
            }
            Command::Cancel => {
                let script = actions::rendered_cancel_script(&self.profile);
                let question = self.printer.with_state(|s| {
                    s.job
                        .as_ref()
                        .filter(|j| j.status.is_active())
                        .map(|j| actions::cancel_question(j, &script))
                });
                match question {
                    None => self.out.warn("No job is running"),
                    Some(question) if self.confirm(input, &question.line(), true) => {
                        self.printer.cancel();
                        self.out.dim("Cancelling…");
                    }
                    Some(_) => self.out.dim("Not cancelled"),
                }
            }
            Command::Status => {
                let lines = self
                    .printer
                    .with_state(|s| report::status_lines(s, self.file.as_ref(), "reset"));
                self.out.lines(&lines);
            }
            Command::Monitor(secs) => self.monitor(secs, input.interactive()),
            Command::Log(None) => self.out.dim(if self.echo.log_all() {
                "log is on: all traffic is echoed"
            } else {
                "log is off: printer replies and messages are echoed"
            }),
            Command::Log(Some(on)) => {
                // What was sent before switching is echoed the old way.
                if self.printer.is_connected() {
                    self.interrupt.reset();
                    let _ =
                        session::wait_idle(&self.printer, Duration::from_secs(2), &self.interrupt);
                }
                self.echo.settle();
                self.echo.set_log_all(on);
                self.out.dim(if on {
                    "Echoing all traffic (sent lines, polling, oks)"
                } else {
                    "Echoing printer replies and messages only"
                });
            }
            Command::Macro(None) => {
                let mut lines = report::macro_lines(&self.config.macros);
                lines.extend(self.config_path.as_deref().map(report::config_file_line));
                self.out.lines(&lines);
            }
            Command::MacroDefine { name, gcode, edit } => {
                self.define_macro(&name, gcode, edit, input);
            }
            Command::MacroRemove(name) => self.remove_macro(&name, input),
            Command::Preheat(None) => self
                .out
                .lines(&report::preset_lines(&self.profile.temp_presets)),
            Command::Exit => {
                if self.leave(input) {
                    return Flow::Exit;
                }
            }
            other => {
                if self.connected_or_warn() {
                    self.execute_on_printer(other, input);
                }
            }
        }
        Flow::Continue
    }

    /// `macro define|edit <name> [gcode]`: save a macro; without G-code
    /// on the line, the lines that follow are its G-code, up to a blank
    /// line (pronsole's way).
    fn define_macro(
        &mut self,
        name: &str,
        gcode: Option<String>,
        edit: bool,
        input: &mut dyn LineSource,
    ) {
        let existing = self
            .config
            .macros
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .cloned();
        if edit && existing.is_none() {
            self.out.error(format!(
                "no macro '{name}' (macro lists them; macro define makes one)"
            ));
            return;
        }
        let gcode = match gcode {
            Some(gcode) => gcode,
            None => {
                if input.interactive() {
                    if let Some(m) = &existing {
                        self.out.dim(format!("{} now:", m.name));
                        for line in m.gcode.lines() {
                            self.out.dim(format!("  {line}"));
                        }
                    }
                    self.out.dim(format!(
                        "Type the G-code of {name}, a line at a time; a blank line ends it"
                    ));
                }
                let mut lines = Vec::new();
                loop {
                    match input.read("... ") {
                        Read::Line(line) if line.trim().is_empty() => break,
                        Read::Line(line) => lines.push(line.trim().to_string()),
                        Read::Eof => break,
                        Read::Interrupted => {
                            self.out.warn(format!("{name} not saved"));
                            return;
                        }
                    }
                }
                lines.join("\n")
            }
        };
        if gcode.trim().is_empty() {
            self.out.warn(format!("{name} not saved: it has no G-code"));
            return;
        }
        let name = existing.map_or_else(|| name.to_string(), |m| m.name);
        let lines = gcode.lines().count();
        let mut replaced = false;
        match crate::config::edit_config(self.config_path.as_deref(), &self.config, |config| {
            replaced = actions::define_macro(&mut config.macros, &name, &gcode);
        }) {
            Ok(saved) => {
                self.keep_profile(saved);
                self.out.good(format!(
                    "{} macro {name} ({lines} line{})",
                    if replaced { "Changed" } else { "Saved" },
                    if lines == 1 { "" } else { "s" }
                ));
            }
            Err(why) => self.out.error(format!("{name} not saved: {why}")),
        }
    }

    /// `macro rm <name>`: delete a macro (asks first on a terminal).
    fn remove_macro(&mut self, name: &str, input: &mut dyn LineSource) {
        let Some(m) = self
            .config
            .macros
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name.trim()))
            .cloned()
        else {
            self.out
                .error(format!("no macro '{name}' (macro lists them)"));
            return;
        };
        if !self.confirm(input, &format!("Delete the macro {}?", m.name), true) {
            self.out.dim("Kept");
            return;
        }
        match crate::config::edit_config(self.config_path.as_deref(), &self.config, |config| {
            actions::remove_macro(&mut config.macros, &m.name);
        }) {
            Ok(saved) => {
                self.keep_profile(saved);
                self.out.good(format!("Deleted the macro {}", m.name));
            }
            Err(why) => self.out.error(format!("{} not deleted: {why}", m.name)),
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

    /// Delete `name` from the SD card (M30) and say how it went once the
    /// printer answered: deleted, or (echoed from the printer's console)
    /// why not.
    fn delete_sd_file(&self, name: &str) {
        let events = self.printer.subscribe();
        self.printer.delete_sd_file(name);
        self.out.dim(format!("Deleting {name} (M30)…"));
        self.interrupt.reset();
        let deadline = Instant::now() + REPORT_TIMEOUT;
        let deleted = loop {
            match events.recv_timeout(session::POLL * 5) {
                // The host takes the file off the listing.
                Ok(sprint_host::Event::SdFiles(_)) => break Some(true),
                Ok(sprint_host::Event::Console(entry))
                    if entry.kind == sprint_host::ConsoleKind::Warning
                        && entry.text.contains("Could not delete") =>
                {
                    break Some(false);
                }
                Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break None,
            }
            let open = self.printer.with_state(|s| s.status.is_open());
            if !open || self.interrupt.take() || Instant::now() >= deadline {
                break None;
            }
        };
        // The printer's own lines (and its warning) come first.
        self.echo.settle();
        match deleted {
            Some(true) => self.out.good(format!("Deleted {name}")),
            Some(false) => {}
            None => self.out.warn(format!(
                "No answer about deleting {name} (sd ls shows the card)"
            )),
        }
    }

    /// `eta`: where the job is and how long it has left.
    fn eta(&self) {
        let job = self.printer.with_state(|s| {
            s.job.as_ref().map(|j| {
                (
                    j.status.is_active(),
                    j.name.clone(),
                    report::job_status_text(&j.status),
                    report::job_progress_text(j),
                )
            })
        });
        match job {
            Some((true, name, status, progress)) => {
                self.out.plain(format!("{status} {name}  {progress}"));
            }
            Some((false, name, status, progress)) => self.out.dim(format!(
                "No job is running (last: {status} {name}  {progress})"
            )),
            None => self.out.warn("No job is running"),
        }
    }

    /// `source <file>`: run its lines as if typed, until the end, `exit`
    /// or Ctrl+C.
    fn source(&mut self, path: &Path, input: &mut dyn LineSource) -> Flow {
        if self.sourcing >= MAX_SOURCE_DEPTH {
            self.out.error(format!(
                "source files nest more than {MAX_SOURCE_DEPTH} deep; not running {}",
                path.display()
            ));
            return Flow::Continue;
        }
        let path = crate::paths::resolve(&path.to_string_lossy(), &self.base_dir);
        let text = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                self.out
                    .error(format!("Cannot read {}: {e}", path.display()));
                return Flow::Continue;
            }
        };
        self.sourcing += 1;
        self.interrupt.reset();
        let mut flow = Flow::Continue;
        for (number, line) in text.lines().enumerate() {
            if self.interrupt.take() {
                self.out.warn(format!(
                    "Stopped {} before line {}",
                    path.display(),
                    number + 1
                ));
                break;
            }
            if self.execute_line(line, input) == Flow::Exit {
                flow = Flow::Exit;
                break;
            }
        }
        self.sourcing -= 1;
        flow
    }

    /// `block_until_online`: wait until the printer is connected.
    fn wait_online(&self) {
        let (connected, opening) = self
            .printer
            .with_state(|s| (s.is_connected(), s.status.is_open()));
        if connected {
            return;
        }
        if !opening {
            self.out
                .warn("Not connected, and not connecting (connect first)");
            return;
        }
        self.interrupt.reset();
        match session::wait_for(
            &self.printer,
            Duration::MAX,
            &self.interrupt,
            true,
            sprint_host::PrinterState::is_connected,
        ) {
            Ok(()) => self.out.good("The printer is online"),
            Err(WaitError::Interrupted) => self.out.warn("Stopped waiting"),
            Err(e) => self.out.error(format!("Not online: {e}")),
        }
    }

    /// `reset`: reset the board (pronsole's reset; [`Printer::reset_board`]:
    /// a DTR pulse on a serial port, a reboot of the virtual printer), the
    /// way to recover from an emergency stop, and wait until the printer
    /// is back. A network link cannot reset the board: the reason says
    /// what to do instead.
    fn reset(&mut self) {
        if self.job_active() {
            self.out
                .warn("Not while a job runs: resetting the board stops it (cancel it first)");
            return;
        }
        self.out.dim("Resetting the board…");
        // Quiet: the setup replies (M115, M503, ...) are not echoed.
        let _quiet = self.echo.expect(Awaited::Connect, false);
        self.interrupt.reset();
        let result =
            session::reset_board_blocking(&self.printer, session::RESET_WAIT, &self.interrupt);
        self.echo.settle();
        self.echo.cancel_expect();
        match result {
            Ok(()) => self.out.good("The board was reset; the printer is ready"),
            Err(ResetError::Refused(why)) => {
                self.out.error(format!("Cannot reset the board: {why}"));
            }
            Err(e) => self.out.error(format!("Reset: {e}")),
        }
    }

    /// `profile <name>`: use another printer profile from now on, saved
    /// as the active one.
    fn switch_profile(&mut self, name: &str) {
        if self.job_active() {
            self.out
                .warn("Not while a job runs (the job uses the profile's scripts)");
            return;
        }
        let index = match actions::find_profile(&self.config.profiles, name) {
            Ok(index) => index,
            Err(why) => {
                self.out.error(why);
                return;
            }
        };
        let wanted = self.config.profiles[index].name.clone();
        match crate::config::edit_profile(
            self.config_path.as_deref(),
            &self.config,
            &wanted,
            |config, i| config.active_profile = i,
        ) {
            Ok((config, i)) => self.use_config(config, i),
            Err(why) => {
                self.out
                    .warn(format!("{why}; using {wanted} for this session only"));
                let config = self.config.clone();
                self.use_config(config, index);
            }
        }
        let target = self.profile.connection.to_string();
        self.out.good(format!(
            "Using the profile {} ({target})",
            self.profile.name
        ));
        if let Some(label) = self.printer.with_state(|s| {
            s.status
                .is_open()
                .then(|| s.target.clone().unwrap_or_default())
        }) {
            self.out.dim(format!(
                "Still connected to {label}; connect without a target uses {target}"
            ));
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
                self.out
                    .good(format!("Saved {target} as the printer of {name}"));
            }
            Err(why) => self.out.error(format!("Not saved: {why}")),
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

    /// Resume the job. Commands typed while it was paused (a purge, a
    /// move) are still sent first: the engine puts what was queued before
    /// the resume script ahead of it.
    fn resume(&self) {
        self.printer.resume();
        self.out.dim("Resuming…");
    }

    fn help(&self, topic: Option<&str>) {
        let Some(topic) = topic else {
            self.out.lines(&report::help_lines());
            return;
        };
        if let Some(spec) = command_spec(topic) {
            self.out.lines(&report::command_help(spec));
        } else if let Some(doc) = gcode_doc(topic) {
            self.out.lines(&report::gcode_help(doc));
        } else if let Some(def) = setting_def(topic) {
            self.out.lines(&[
                ReportLine::new()
                    .with(def.code, Tone::Accent)
                    .with(format!("  {}", def.name), Tone::Plain),
                ReportLine::toned(format!("  {}", def.description), Tone::Dim),
            ]);
        } else {
            self.out
                .error(format!("No help for '{topic}' (help lists the commands)"));
        }
    }

    /// `print [--pause-at-layer L]`: print the loaded file, pausing (with
    /// the pause script) before the layers given; `resume` goes on.
    fn print(&mut self, pause_at: &[u64]) {
        let blocker = self
            .printer
            .with_state(|s| actions::print_blocker(s, self.file.as_ref(), "reset"));
        if let Some(why) = blocker {
            let hint = match why.as_str() {
                "No file loaded" => " (load <file>)",
                "Not connected to a printer" => " (connect sim, connect COM3, ...)",
                _ => "",
            };
            self.out.warn(format!("{why}{hint}"));
            return;
        }
        let Some(file) = &self.file else {
            return;
        };
        let points = match actions::pause_points(file, pause_at, 0) {
            Ok(points) => points,
            Err(why) => {
                self.out.error(why);
                return;
            }
        };
        self.pause_asked = None;
        let options = actions::print_options(file, &self.profile);
        let note = actions::progress_note(&options);
        match self.printer.start_print(file.file.clone(), options) {
            // The printer's console says "Printing ..."; add the plan.
            Ok(()) => {
                if !points.is_empty() {
                    let lines: Vec<usize> = points.iter().map(|p| p.line).collect();
                    self.printer.pause_at_lines(&lines);
                }
                self.layer_pauses_ahead = points.len();
                let stats = &file.toolpath.stats;
                self.echo.settle();
                self.out.dim(format!(
                    "{} layers, {} estimated (status or monitor shows the progress)",
                    file.toolpath.layers.len(),
                    format::duration(f64::from(stats.estimated_time))
                ));
                if let Some(note) = note {
                    self.out.dim(note);
                }
                if !points.is_empty() {
                    self.out.dim(format!(
                        "Pausing before layer {} (resume goes on)",
                        actions::layers_text(&points)
                    ));
                }
            }
            Err(e) => self.out.error(format!("Cannot print: {e}")),
        }
    }

    /// `exclude <object>` (or `include`): stop printing an object of the
    /// running print, or print it again; the printer's console says it
    /// took effect.
    fn exclude_object(&self, name: &str, exclude: bool) {
        let found = self.printer.with_state(|s| {
            let job = actions::excludable_job(s).ok_or_else(|| {
                "No print is running (objects are excluded while one runs)".to_string()
            })?;
            let label = actions::find_object(job, name)?;
            let already = job.excluded_objects.iter().any(|e| e == label);
            let left = job.objects.len() - job.excluded_objects.len();
            Ok::<_, String>((label.to_string(), already, left))
        });
        match found {
            Err(why) => self.out.error(why),
            Ok((label, true, _)) if exclude => self.out.dim(format!("{label} is excluded already")),
            Ok((label, false, _)) if !exclude => self.out.dim(format!("{label} is not excluded")),
            Ok((label, _, left)) => {
                if exclude {
                    self.printer.exclude_object(&label);
                    if left == 1 {
                        self.out.warn(
                            "Every object is excluded now: the print goes on without them \
                             (cancel ends it)",
                        );
                    }
                } else {
                    self.printer.include_object(&label);
                }
                self.echo.settle();
            }
        }
    }

    /// `recover [line]`: print the loaded file again from the line its
    /// failed or cancelled print stopped at (or from `line`), after the
    /// resume preamble, which is shown first and asked about on a
    /// terminal.
    fn recover(&mut self, line: Option<u64>, input: &mut dyn LineSource) {
        let blocker = self
            .printer
            .with_state(|s| actions::print_blocker(s, self.file.as_ref(), "reset"));
        if let Some(why) = blocker {
            self.out.warn(why);
            return;
        }
        let Some(file) = self.file.clone() else {
            return;
        };
        let recovery = match self
            .printer
            .with_state(|s| actions::recovery(s, Some(&file), line))
        {
            Ok(recovery) => recovery,
            Err(why) => {
                self.out.error(why);
                return;
            }
        };
        let mut lines = actions::recovery_lines(&recovery, file.name()).into_iter();
        if let Some(first) = lines.next() {
            self.out.plain(first);
        }
        for line in lines {
            self.out.dim(line);
        }
        let from = recovery.line + 1;
        if !self.confirm(
            input,
            &format!("Recover {} from line {from}?", file.name()),
            true,
        ) {
            self.out.dim("Not started");
            return;
        }
        self.pause_asked = None;
        self.layer_pauses_ahead = 0;
        let options = actions::recovery_options(&file, &self.profile, &recovery);
        match self.printer.start_print(file.file.clone(), options) {
            Ok(()) => {
                self.echo.settle();
                self.out
                    .dim(format!("Recovering {} from line {from}", file.name()));
            }
            Err(e) => self.out.error(format!("Cannot print: {e}")),
        }
    }

    /// Print a status line every `secs` seconds until Ctrl+C (and, in a
    /// script, until no job runs).
    fn monitor(&self, secs: Option<f32>, interactive: bool) {
        if !self.connected_or_warn() {
            return;
        }
        let secs = secs.filter(|s| s.is_finite()).unwrap_or(MONITOR_INTERVAL);
        let interval = Duration::from_secs_f32(secs.clamp(0.2, 3600.0));
        self.interrupt.reset();
        if interactive {
            self.out.dim(format!(
                "Status every {}; Ctrl+C stops",
                format::duration(interval.as_secs_f64())
            ));
        }
        loop {
            let (text, active, open) = self
                .printer
                .with_state(|s| (report::monitor_text(s), s.job_active(), s.status.is_open()));
            self.out.plain(text);
            if !open || (!interactive && !active) {
                return;
            }
            // A script waits for the job; nothing in it can resume a job
            // it paused itself (the end of the script cancels that).
            if !interactive && self.paused_by_script() {
                self.out
                    .warn("The script paused the job; monitor stops (a script cannot resume it)");
                return;
            }
            let until = Instant::now() + interval;
            while Instant::now() < until {
                if self.interrupt.take() {
                    return;
                }
                std::thread::sleep(session::POLL * 5);
            }
        }
    }

    /// Commands that need a connection.
    fn execute_on_printer(&mut self, cmd: Command, input: &mut dyn LineSource) {
        let printer = self.printer.clone();
        let tool = printer.with_state(|s| s.active_tool);
        match cmd {
            Command::Temp { target, tool: t } => {
                match actions::resolve_temp(&target, &self.profile.temp_presets, false) {
                    Ok(celsius) => {
                        self.set_temps(&[(Heater::Hotend(t.unwrap_or(tool)), celsius)], input);
                    }
                    Err(why) => self.out.error(why),
                }
            }
            Command::BedTemp(target) => {
                match actions::resolve_temp(&target, &self.profile.temp_presets, true) {
                    Ok(celsius) => self.set_temps(&[(Heater::Bed, celsius)], input),
                    Err(why) => self.out.error(why),
                }
            }
            // Asked for, as pronsole does: a target just set shows.
            Command::GetTemp => {
                if self.request(
                    Awaited::Temperatures,
                    false,
                    REPORT_TIMEOUT,
                    Printer::request_temperatures,
                ) {
                    self.out
                        .lines(&printer.with_state(report::temperature_lines));
                }
            }
            Command::Off => {
                let gcode = printer.with_state(|s| {
                    actions::everything_off_gcode(&actions::heaters(s, &self.profile))
                });
                // A script does not ruin a print by itself.
                let check = SendCheck {
                    question: "Turn everything off?",
                    refused: "Left on",
                    declined: "Left on",
                    always_ask: None,
                    raw: false,
                };
                if self.safe_to_send(&gcode, &check, input) {
                    let heaters = session::everything_off(&printer);
                    let names: Vec<String> =
                        heaters.into_iter().map(actions::heater_label).collect();
                    self.out.dim(format!(
                        "{} off, fans off, motors off (M84)",
                        names.join(", ")
                    ));
                }
            }
            Command::Tool(t) => {
                if self.motion(Motion::Tool(t), input) {
                    self.out.dim(format!("Tool T{t}"));
                }
            }
            Command::Reset => self.reset(),
            Command::Preheat(Some(name)) => {
                match actions::find_preset(&self.profile.temp_presets, &name) {
                    Ok(preset) => {
                        let targets = [
                            (Heater::Hotend(tool), preset.hotend),
                            (Heater::Bed, preset.bed),
                        ];
                        self.out.dim(format!("Preheating for {}", preset.name));
                        self.set_temps(&targets, input);
                    }
                    Err(why) => self.out.error(why),
                }
            }
            Command::Cooldown => {
                let heaters = printer.with_state(|s| actions::heaters(s, &self.profile));
                let targets: Vec<(Heater, f32)> = heaters.into_iter().map(|h| (h, 0.0)).collect();
                self.set_temps(&targets, input);
            }
            Command::Home { axes, zero_e } => {
                if self.motion(Motion::Home { axes, zero_e }, input) && zero_e {
                    self.out.dim("Extruder position set to 0 (G92 E0)");
                }
            }
            Command::Move { axis, mm, feedrate } => {
                let feedrate = feedrate.unwrap_or(match axis {
                    Axis::Z => self.profile.jog_feedrate_z,
                    Axis::E => self.profile.extrude_feedrate,
                    Axis::X | Axis::Y => self.profile.jog_feedrate_xy,
                });
                self.motion(Motion::Jog { axis, mm, feedrate }, input);
            }
            Command::Extrude { mm, feedrate } => {
                let feedrate = feedrate.unwrap_or(self.profile.extrude_feedrate);
                self.motion(Motion::Extrude { tool, mm, feedrate }, input);
            }
            Command::Fan(pct) => {
                printer.set_fan(0, actions::fan_value(pct));
                self.out
                    .dim(format!("Fan {}%", format::trimmed(f64::from(pct), 0)));
            }
            Command::Speed(pct) => {
                printer.set_feedrate_override(pct);
                self.out
                    .dim(format!("Speed {}%", format::trimmed(f64::from(pct), 1)));
            }
            Command::Flow(pct) => {
                printer.set_flow(tool, pct);
                self.out.dim(format!(
                    "Flow of T{tool} {}%",
                    format::trimmed(f64::from(pct), 1)
                ));
            }
            Command::Babystep(mm) => {
                if let Some(warning) = self.homing_warning(Axes::Z, actions::UNHOMED_BABYSTEP) {
                    self.out.warn(warning);
                }
                printer.babystep_z(mm);
                self.out.dim(format!(
                    "Z nudged by {} mm",
                    format::trimmed(f64::from(mm), 3)
                ));
            }
            Command::Settings(filter) => {
                if self.request(
                    Awaited::Settings,
                    false,
                    REPORT_TIMEOUT,
                    Printer::request_settings,
                ) {
                    let lines = printer
                        .with_state(|s| report::settings_lines(&s.settings, filter.as_deref()));
                    self.out.lines(&lines);
                }
            }
            Command::Set(edit) => {
                let merged = printer.with_state(|s| actions::merged_setting(&s.settings, &edit));
                let gcode = merged.to_gcode();
                let check = SendCheck {
                    question: "Change it?",
                    refused: "Not changed",
                    declined: "Not changed",
                    always_ask: None,
                    raw: false,
                };
                if self.safe_to_send(&gcode, &check, input) {
                    printer.apply_setting(&merged);
                    self.out.dim(format!("Sent {gcode} (eeprom save keeps it)"));
                }
            }
            Command::Eeprom(Eeprom::Save) => {
                printer.save_settings();
                self.out.good("Settings saved to EEPROM (M500)");
            }
            Command::Eeprom(Eeprom::Load) => {
                if self.request(
                    Awaited::Settings,
                    false,
                    REPORT_TIMEOUT,
                    Printer::load_settings,
                ) {
                    self.out.good("Settings reloaded from EEPROM (M501)");
                }
            }
            Command::Eeprom(Eeprom::Reset { confirmed }) => {
                let go = if confirmed {
                    true
                } else if input.interactive() {
                    self.confirm(
                        input,
                        "M502 replaces every setting with the firmware defaults. Reset?",
                        false,
                    )
                } else {
                    self.out
                        .error("eeprom reset needs confirmation in scripts: eeprom reset yes");
                    return;
                };
                if !go {
                    self.out.dim("Settings kept");
                } else if self.request(
                    Awaited::Settings,
                    false,
                    REPORT_TIMEOUT,
                    Printer::factory_reset_settings,
                ) {
                    self.out.warn(
                        "Settings reset to the firmware defaults (M502); eeprom save keeps them",
                    );
                }
            }
            Command::Firmware => {
                if self.request(
                    Awaited::Firmware,
                    false,
                    REPORT_TIMEOUT,
                    Printer::request_firmware_info,
                ) {
                    self.out.lines(&printer.with_state(report::firmware_lines));
                }
            }
            Command::Endstops => {
                if self.request(
                    Awaited::Endstops,
                    false,
                    REPORT_TIMEOUT,
                    Printer::request_endstops,
                ) {
                    self.out
                        .lines(&printer.with_state(|s| report::endstop_lines(&s.endstops)));
                }
            }
            Command::Mesh { probe } => {
                let shown = if probe {
                    if !self.motion_allowed(&Motion::ProbeMesh, input) {
                        return;
                    }
                    self.out.dim("Probing the bed (G29); Ctrl+C stops waiting");
                    self.request(Awaited::Mesh, false, PROBE_TIMEOUT, Printer::probe_bed_mesh)
                } else {
                    // Without a mesh the answer brings no news.
                    self.request(
                        Awaited::Mesh,
                        true,
                        REPORT_TIMEOUT,
                        Printer::request_bed_mesh,
                    )
                };
                if shown {
                    let lines = printer.with_state(|s| {
                        s.mesh.as_ref().map_or_else(
                            || {
                                vec![ReportLine::toned(
                                    "No bed mesh reported (mesh probe measures one)",
                                    Tone::Dim,
                                )]
                            },
                            report::mesh_lines,
                        )
                    });
                    self.out.lines(&lines);
                }
            }
            Command::Stats => {
                if self.request(Awaited::Stats, true, REPORT_TIMEOUT, Printer::request_stats) {
                    self.out.lines(
                        &printer.with_state(|s| report::stats_lines(s.stats.as_ref(), &s.link)),
                    );
                }
            }
            Command::Sd(SdCommand::List) => {
                if self.request(
                    Awaited::SdFiles,
                    true,
                    REPORT_TIMEOUT,
                    Printer::request_sd_files,
                ) {
                    self.out
                        .lines(&printer.with_state(|s| report::sd_lines(&s.sd)));
                }
            }
            Command::Sd(SdCommand::Print(name)) => match printer
                .start_sd_print(&name)
                .inspect(|()| self.pause_asked = None)
            {
                Ok(()) => self.out.good(format!("Printing {name} from the SD card")),
                Err(e) => self.out.error(format!("Cannot print {name}: {e}")),
            },
            Command::Sd(SdCommand::Remove(name)) => {
                if let Some(why) = printer.with_state(actions::sd_delete_blocker) {
                    self.out.warn(why);
                } else if self.confirm(input, &format!("Delete {name} from the SD card?"), true) {
                    // The card may have become busy while the question was
                    // open (a print started at the printer).
                    match printer.with_state(actions::sd_delete_blocker) {
                        Some(why) => self.out.warn(why),
                        None => self.delete_sd_file(&name),
                    }
                }
            }
            Command::Sd(SdCommand::Upload { file, name }) => self.upload(file.as_deref(), name),
            Command::Autotune {
                heater,
                celsius,
                cycles,
            } => {
                let gcode = actions::autotune_gcode(heater, celsius, cycles);
                let check = SendCheck {
                    question: "Start the autotune?",
                    refused: "Not started",
                    declined: "Not started",
                    always_ask: None,
                    raw: false,
                };
                if !self.safe_to_send(&gcode, &check, input) {
                    return;
                }
                printer.start_pid_autotune(heater, celsius, cycles);
                self.out.dim(format!(
                    "PID autotune of {} at {} °C, {cycles} cycles; the result is printed when it arrives",
                    heater.name(),
                    format::trimmed(f64::from(celsius), 1)
                ));
            }
            Command::Macro(Some(text)) => match actions::macro_call(&self.config.macros, &text)
                .and_then(|(m, args)| {
                    let gcode = actions::macro_gcode(m, &args)?;
                    Ok(sprint_host::Macro { gcode, ..m.clone() })
                }) {
                Ok(m) => {
                    // Its lines as they will be sent go through the
                    // classifier, like typed ones; a macro marked to ask
                    // first asks anyway.
                    let gcode = actions::rendered_gcode(&m.gcode, &self.profile);
                    // Asked on a terminal: what it sends, placeholders
                    // filled in, comes first.
                    let ctx = printer.with_state(actions::job_context);
                    let asks = m.confirm || actions::check_gcode(&gcode, ctx).needs_confirmation();
                    if asks && input.interactive() {
                        for line in gcode.lines() {
                            self.out.dim(format!("  {line}"));
                        }
                    }
                    let question = format!("Run {}?", m.name);
                    let refused = format!("Macro {} not run", m.name);
                    let check = SendCheck {
                        question: &question,
                        refused: &refused,
                        declined: "Not run",
                        always_ask: m.confirm.then_some(question.as_str()),
                        raw: false,
                    };
                    if self.safe_to_send(&gcode, &check, input) {
                        printer.run_macro(&m);
                        self.out.dim(format!("Macro {}", m.name));
                    }
                }
                Err(why) => self.out.error(why),
            },
            // Handled without a connection.
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
            | Command::Print { .. }
            | Command::Recover(_)
            | Command::Objects
            | Command::Exclude(_)
            | Command::Include(_)
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

    /// Send heater targets, as the classifier allows them for the running
    /// job (turning a heater off while printing asks first on a terminal;
    /// one that is off already is not asked about).
    fn set_temps(&self, targets: &[(Heater, f32)], input: &mut dyn LineSource) {
        let targets = actions::safe_targets(targets);
        let gcode = self
            .printer
            .with_state(|s| actions::targets_gcode(&s.temps, &targets));
        let question = format!("Set {}?", actions::targets_text(&targets));
        let check = SendCheck {
            question: &question,
            refused: "Not set",
            declined: "Temperatures kept",
            always_ask: None,
            raw: false,
        };
        if !self.safe_to_send(&gcode, &check, input) {
            return;
        }
        for &(heater, celsius) in &targets {
            self.printer.set_temperature(heater, celsius);
        }
        if !targets.is_empty() {
            self.out.dim(actions::targets_text(&targets));
        }
    }

    fn upload(&self, file: Option<&Path>, name: Option<String>) {
        let (gcode, path) = match file {
            Some(path) => {
                let path = crate::paths::resolve(&path.to_string_lossy(), &self.base_dir);
                match loader::read_file(&path) {
                    Ok(f) => (std::sync::Arc::new(f), path),
                    Err(why) => {
                        self.out.error(why);
                        return;
                    }
                }
            }
            None => match &self.file {
                Some(f) => (f.file.clone(), f.path.clone()),
                None => {
                    self.out
                        .warn("No file loaded (load <file>, or sd upload <file>)");
                    return;
                }
            },
        };
        let sd_name = name.unwrap_or_else(|| actions::sd_name(&path));
        match self.printer.upload_to_sd(gcode, &sd_name) {
            Ok(()) => self.out.dim(format!(
                "Uploading to the SD card as {sd_name} (status shows progress)"
            )),
            Err(e) => self.out.error(format!("Cannot upload: {e}")),
        }
    }

    /// `exit` or the end of the input: true when the shell may leave.
    /// A running print is cancelled after asking (on a terminal) or
    /// waited for (in a script); a script's queued commands are sent first.
    fn leave(&mut self, input: &mut dyn LineSource) -> bool {
        if self.job_active() {
            let name = self
                .printer
                .with_state(|s| s.job.as_ref().map(|j| j.name.clone()))
                .unwrap_or_default();
            if input.interactive() {
                if !self.confirm(
                    input,
                    &format!("{name} is printing; leaving cancels it. Leave?"),
                    false,
                ) {
                    return false;
                }
                self.cancel_and_wait();
                return true;
            }
            // Paused by the script: nothing that follows can resume it, and
            // waiting would keep the heaters on for ever. (A pause by the
            // printer, a filament change, is answered at the printer.) A
            // job that runs again (resumed at the printer) or was never
            // paused (an upload refuses) is waited for.
            if self.paused_by_script() {
                self.out.warn(format!(
                    "{name} is paused and a script cannot resume it: cancelling it \
                     (the cancel script turns the heaters off)"
                ));
                self.cancel_and_wait();
                return true;
            }
            self.out
                .dim(format!("Waiting for {name} to finish (Ctrl+C cancels it)"));
            self.interrupt.reset();
            let out = self.out.clone();
            let mut told = false;
            let ahead = self.layer_pauses_ahead;
            let finished =
                session::wait_for(&self.printer, Duration::MAX, &self.interrupt, true, |s| {
                    // A pause before a layer the script asked for: nothing
                    // will resume it either.
                    if paused_at_layer(s, ahead) {
                        return true;
                    }
                    let paused = s.job.as_ref().is_some_and(|j| {
                        matches!(
                            j.status,
                            sprint_host::JobStatus::Paused | sprint_host::JobStatus::Pausing
                        )
                    });
                    if paused && !told {
                        told = true;
                        out.warn(
                            "The printer paused the print: the script waits for it to go on \
                             (resume or answer at the printer; Ctrl+C cancels it)",
                        );
                    }
                    told &= paused;
                    !s.job_active()
                });
            if finished == Err(WaitError::Interrupted) {
                self.cancel_and_wait();
            } else if self.paused_by_script() {
                self.out.warn(format!(
                    "{name} paused before a layer and a script cannot resume it: cancelling \
                     it (the cancel script turns the heaters off)"
                ));
                self.cancel_and_wait();
            }
            self.echo.settle();
            return true;
        }
        if !input.interactive() && self.printer.is_connected() {
            self.interrupt.reset();
            let _ = session::wait_idle(&self.printer, DRAIN_TIMEOUT, &self.interrupt);
            self.echo.settle();
        }
        true
    }

    /// Whether `pause` paused the job and it is still paused (or
    /// pausing). A pause asked a moment ago is given [`PAUSE_SETTLE`] to
    /// show: the engine starts pausing a few milliseconds after `pause`.
    fn paused_by_script(&self) -> bool {
        let ahead = self.layer_pauses_ahead;
        if self.printer.with_state(|s| paused_at_layer(s, ahead)) {
            return true;
        }
        let Some(asked) = self.pause_asked else {
            return false;
        };
        let paused = |s: &sprint_host::PrinterState| {
            s.job.as_ref().is_some_and(|j| {
                matches!(
                    j.status,
                    sprint_host::JobStatus::Paused | sprint_host::JobStatus::Pausing
                )
            })
        };
        let settle = PAUSE_SETTLE.saturating_sub(asked.elapsed());
        let _ = session::wait_for(&self.printer, settle, &Interrupt::new(), true, |s| {
            paused(s) || !s.job_active()
        });
        self.printer.with_state(|s| paused(s) && s.job_active())
    }

    fn cancel_and_wait(&self) {
        self.printer.cancel();
        self.interrupt.reset();
        let _ = session::wait_for(&self.printer, CANCEL_TIMEOUT, &self.interrupt, true, |s| {
            !s.job_active()
        });
        self.echo.settle();
    }
}

/// The object labels of the loaded file, for `objects` before a print.
fn file_object_names(file: Option<&LoadedFile>) -> Vec<String> {
    file.map(|f| f.toolpath.objects.iter().map(|o| o.name.clone()).collect())
        .unwrap_or_default()
}

/// Whether the job is paused at a `print --pause-at-layer` point: fewer
/// points are ahead than the `ahead` there were when it last ran.
fn paused_at_layer(state: &sprint_host::PrinterState, ahead: usize) -> bool {
    state
        .job
        .as_ref()
        .is_some_and(|j| j.status.is_active() && j.status.is_paused() && j.pause_at.len() < ahead)
}

/// What to ask before disconnecting while a job runs: a print Sprint
/// streams (or an upload) is cancelled first; one from the printer's SD
/// card goes on unwatched.
fn disconnect_question(state: &sprint_host::PrinterState) -> Option<String> {
    let job = state.job.as_ref().filter(|j| j.status.is_active())?;
    let name = &job.name;
    Some(match job.kind {
        JobKind::HostPrint => format!(
            "{name} is printing; disconnecting cancels it (the cancel script turns the heaters off). Disconnect?"
        ),
        JobKind::SdPrint => format!(
            "{name} prints from the SD card and goes on unwatched (cancel ends it). Disconnect?"
        ),
        JobKind::SdUpload => format!(
            "{name} is being written to the SD card; disconnecting cancels the upload. Disconnect?"
        ),
    })
}
