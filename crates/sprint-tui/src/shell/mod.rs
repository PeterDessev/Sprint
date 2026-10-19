//! The pronsole-style line shell: one command or G-code line at a time,
//! with what the printer says printed above the prompt.
//!
//! On a terminal the line editor keeps a history (next to the
//! configuration file) and completes commands, G-codes, file names, ports
//! and presets. Piped input runs as a script: no prompt, no questions
//! (`eeprom reset` then needs `yes`), and at the end of the input the
//! shell waits for the queue to drain and for a running print to finish.

mod echo;
mod input;
mod output;
#[cfg(test)]
mod tests;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use sprint_host::sprint_proto::{gcode_doc, setting_def};
use sprint_host::{
    AppConfig, Axis, ConnectTarget, ConnectionStatus, Heater, Printer, PrinterProfile, list_ports,
};

use crate::TuiOptions;
use crate::actions;
use crate::commands::{self, Command, Eeprom, Input, SdCommand, command_spec};
use crate::format;
use crate::interrupt::Interrupt;
use crate::loader::{self, LoadedFile};
use crate::report::{self, ReportLine, Tone};
use crate::session::{self, ConnectError, WaitError};

use echo::{Awaited, Echo};
use input::{LineSource, Read, ShellHelper, Stdin, Terminal};
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
/// Default `monitor` interval.
const MONITOR_INTERVAL: f32 = 2.0;

/// Run the line shell until `exit` or the end of the input.
///
/// `options.file` is loaded first; the shell connects to `options.connect`,
/// or to the active profile's printer only when the profile enables
/// `auto_connect`.
pub fn run_shell(options: TuiOptions) -> anyhow::Result<()> {
    let (config, config_path) = crate::config::load_config();
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let support = report::stdout_colors();
    let base_dir = std::env::current_dir().unwrap_or_default();
    let (mut source, sink): (Box<dyn LineSource>, Box<dyn Sink>) = if interactive {
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
    /// disconnect.
    pub(crate) fn run(&mut self, input: &mut dyn LineSource) {
        loop {
            let prompt = self.prompt();
            match input.read(&prompt) {
                Read::Line(line) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        input.add_history(trimmed);
                    }
                    if self.execute_line(&line, input) == Flow::Exit {
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
        self.printer.disconnect();
        self.echo.settle();
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

    /// Manual moves: refused while a print runs, allowed while it is
    /// paused.
    fn motion_allowed(&self) -> bool {
        let printing = self.printer.with_state(|s| {
            s.job
                .as_ref()
                .is_some_and(|j| j.status.is_active() && j.status != sprint_host::JobStatus::Paused)
        });
        if printing {
            self.out.warn("Not while a job runs (pause it first)");
        }
        !printing
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
        let result = session::connect_blocking(
            &self.printer,
            target,
            self.profile.options.clone(),
            &self.interrupt,
        );
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
                self.config.push_recent(loaded.path.clone());
                if let Some(config_path) = &self.config_path
                    && let Err(e) = self.config.save_to(config_path)
                {
                    log::warn!("could not save the configuration: {e}");
                }
                self.file = Some(loaded);
            }
            Err(why) if why == "cancelled" => self.out.warn(format!("Stopped loading {name}")),
            Err(why) => self.out.error(format!("Cannot load: {why}")),
        }
    }

    /// Send G-code as typed (dangerous codes ask first on a terminal,
    /// unless the line started with `!`).
    pub(crate) fn send_gcode(&mut self, gcode: &str, raw: bool, input: &mut dyn LineSource) {
        if !self.connected_or_warn() {
            return;
        }
        if !raw && let Some(doc) = actions::dangerous_gcode(gcode) {
            let question = format!("{} ({}): {} Send it?", doc.code, doc.name, doc.description);
            if !self.confirm(input, &question, true) {
                self.out.dim("Not sent");
                return;
            }
        }
        self.printer.send(gcode);
    }

    /// Run a command.
    pub(crate) fn execute(&mut self, cmd: Command, input: &mut dyn LineSource) -> Flow {
        match cmd {
            Command::Help(topic) => self.help(topic.as_deref()),
            Command::Connect { target, baud } => {
                match actions::resolve_target(target.as_deref(), baud, &self.profile) {
                    Ok(target) => self.connect(target),
                    Err(why) => self.out.error(why),
                }
            }
            Command::Disconnect => {
                if !self.printer.with_state(|s| s.status.is_open()) {
                    self.out.dim("Not connected");
                } else if !self.job_active()
                    || self.confirm(
                        input,
                        "A job is running; disconnecting stops it. Disconnect?",
                        true,
                    )
                {
                    self.printer.disconnect();
                }
            }
            Command::Ports => self.out.lines(&report::ports_lines(&list_ports())),
            Command::Load(path) => self.load(&path),
            Command::Info => match &self.file {
                Some(file) => self.out.lines(&report::file_lines(file)),
                None => self.out.warn("No file loaded (load <file>)"),
            },
            Command::Print => self.print(),
            Command::Pause => {
                if self.job_active() {
                    self.printer.pause();
                    self.out.dim("Pausing…");
                } else {
                    self.out.warn("No job is running");
                }
            }
            Command::Resume => {
                if self.job_active() {
                    self.printer.resume();
                    self.out.dim("Resuming…");
                } else {
                    self.out.warn("No job is running");
                }
            }
            Command::Cancel => {
                if !self.job_active() {
                    self.out.warn("No job is running");
                } else if self.confirm(input, "Cancel the job? It cannot be resumed.", true) {
                    self.printer.cancel();
                    self.out.dim("Cancelling…");
                }
            }
            Command::Status => {
                let lines = self
                    .printer
                    .with_state(|s| report::status_lines(s, self.file.as_ref()));
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
            Command::Macro(None) => self.out.lines(&report::macro_lines(&self.config.macros)),
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

    fn print(&mut self) {
        let blocker = self
            .printer
            .with_state(|s| actions::print_blocker(s, self.file.as_ref()));
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
        match self
            .printer
            .start_print(file.file.clone(), actions::print_options(file))
        {
            // The printer's console says "Printing ..."; add the plan.
            Ok(()) => {
                let stats = &file.toolpath.stats;
                self.echo.settle();
                self.out.dim(format!(
                    "{} layers, {} estimated (status or monitor shows the progress)",
                    file.toolpath.layers.len(),
                    format::duration(f64::from(stats.estimated_time))
                ));
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
            Command::Temp { celsius, tool: t } => {
                self.set_temps(&[(Heater::Hotend(t.unwrap_or(tool)), celsius)]);
            }
            Command::BedTemp(celsius) => self.set_temps(&[(Heater::Bed, celsius)]),
            Command::Preheat(Some(name)) => {
                match actions::find_preset(&self.profile.temp_presets, &name) {
                    Ok(preset) => {
                        let targets = [
                            (Heater::Hotend(tool), preset.hotend),
                            (Heater::Bed, preset.bed),
                        ];
                        self.out.dim(format!("Preheating for {}", preset.name));
                        self.set_temps(&targets);
                    }
                    Err(why) => self.out.error(why),
                }
            }
            Command::Cooldown => {
                let heaters = printer.with_state(|s| actions::heaters(s, &self.profile));
                let targets: Vec<(Heater, f32)> = heaters.into_iter().map(|h| (h, 0.0)).collect();
                self.set_temps(&targets);
            }
            Command::Home(axes) => {
                if self.motion_allowed() {
                    printer.home(axes);
                }
            }
            Command::Move { axis, mm, feedrate } => {
                if self.motion_allowed() {
                    let feed = feedrate.unwrap_or(match axis {
                        Axis::Z => self.profile.jog_feedrate_z,
                        Axis::E => self.profile.extrude_feedrate,
                        Axis::X | Axis::Y => self.profile.jog_feedrate_xy,
                    });
                    printer.jog(axis, mm, feed);
                }
            }
            Command::Extrude { mm, feedrate } => {
                if self.motion_allowed() {
                    printer.extrude(tool, mm, feedrate.unwrap_or(self.profile.extrude_feedrate));
                }
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
                printer.apply_setting(&merged);
                self.out
                    .dim(format!("Sent {} (eeprom save keeps it)", merged.to_gcode()));
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
                    if !self.motion_allowed() {
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
            Command::Sd(SdCommand::Print(name)) => match printer.start_sd_print(&name) {
                Ok(()) => self.out.good(format!("Printing {name} from the SD card")),
                Err(e) => self.out.error(format!("Cannot print {name}: {e}")),
            },
            Command::Sd(SdCommand::Remove(name)) => {
                if self.confirm(input, &format!("Delete {name} from the SD card?"), true) {
                    printer.delete_sd_file(&name);
                    self.out.dim(format!("Deleted {name} (M30)"));
                }
            }
            Command::Sd(SdCommand::Upload { file, name }) => self.upload(file.as_deref(), name),
            Command::Autotune {
                heater,
                celsius,
                cycles,
            } => {
                if self.job_active() {
                    self.out.warn("Not while a job runs");
                    return;
                }
                printer.start_pid_autotune(heater, celsius, cycles);
                self.out.dim(format!(
                    "PID autotune of {} at {} °C, {cycles} cycles; the result is printed when it arrives",
                    heater.name(),
                    format::trimmed(f64::from(celsius), 1)
                ));
            }
            Command::Macro(Some(name)) => match actions::find_macro(&self.config.macros, &name) {
                Ok(m) => {
                    let m = m.clone();
                    if !m.confirm || self.confirm(input, &format!("Run {}?", m.name), true) {
                        printer.run_macro(&m);
                        self.out.dim(format!("Macro {}", m.name));
                    }
                }
                Err(why) => self.out.error(why),
            },
            // Handled without a connection.
            Command::Help(_)
            | Command::Connect { .. }
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

    fn set_temps(&self, targets: &[(Heater, f32)]) {
        let targets = actions::safe_targets(targets);
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
            self.out
                .dim(format!("Waiting for {name} to finish (Ctrl+C cancels it)"));
            self.interrupt.reset();
            let finished =
                session::wait_for(&self.printer, Duration::MAX, &self.interrupt, true, |s| {
                    !s.job_active()
                });
            if finished == Err(WaitError::Interrupted) {
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

    fn cancel_and_wait(&self) {
        self.printer.cancel();
        self.interrupt.reset();
        let _ = session::wait_for(&self.printer, CANCEL_TIMEOUT, &self.interrupt, true, |s| {
            !s.job_active()
        });
        self.echo.settle();
    }
}
