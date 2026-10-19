//! Shell tests against the simulator (never a serial port).

use std::time::{Duration, Instant};

use sprint_host::sprint_sim::SimConfig;
use sprint_host::{AppConfig, ConnectTarget, JobStatus, Printer};

use super::input::Script;
use super::output::{Buffer, Out, WriterSink};
use super::*;
use crate::theme::ColorSupport;

/// Generous bound for anything a test waits for.
const PATIENCE: Duration = Duration::from_secs(30);

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

/// A fast virtual printer.
fn fast_sim() -> ConnectTarget {
    ConnectTarget::Simulator(SimConfig {
        boot_delay_ms: 20,
        time_scale: 100.0,
        instant_moves: true,
        ..SimConfig::default()
    })
}

/// A disconnected shell writing plain text into a buffer.
fn shell() -> (Shell, Buffer) {
    let buffer = Buffer::default();
    let out = Out::new(
        Box::new(WriterSink(buffer.clone())),
        ColorSupport::None,
        false,
    );
    let shell = Shell::new(
        Printer::new(),
        AppConfig::default(),
        None,
        out,
        Interrupt::new(),
    );
    (shell, buffer)
}

/// A shell connected to a fast simulator, its output cleared.
fn connected() -> (Shell, Buffer) {
    connected_to(fast_sim())
}

/// A shell connected to a simulator whose moves take (fast) real time.
fn connected_timed() -> (Shell, Buffer) {
    connected_to(ConnectTarget::Simulator(SimConfig {
        boot_delay_ms: 20,
        time_scale: 5.0,
        instant_moves: false,
        ..SimConfig::default()
    }))
}

fn connected_to(target: ConnectTarget) -> (Shell, Buffer) {
    let (mut shell, buffer) = shell();
    shell.connect(target);
    assert!(shell.printer.is_connected(), "{}", buffer.text());
    shell.echo.settle();
    buffer.clear();
    received(&shell);
    (shell, buffer)
}

/// Run lines as a script (end of input included: queued commands drain).
fn script(shell: &mut Shell, lines: &[&str]) {
    let mut input = Script::new(lines.iter().copied(), false);
    shell.run_lines(&mut input);
}

/// Commands the simulator received since the last call.
fn received(shell: &Shell) -> Vec<String> {
    let control = shell.printer.sim_control().expect("simulator");
    let got = control.received_commands();
    control.clear_received();
    got
}

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("sprint-shell-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    /// A job with many real-time moves, still running while keys are
    /// pressed: short moves, so pausing (which waits for the planned
    /// moves) is quick.
    fn slow_file(&self) -> PathBuf {
        let mut gcode = String::from("G28\nG90\nM83\nG1 Z0.3 F600\n");
        for i in 0..400 {
            let x = 20 + (i % 2) * 5;
            gcode.push_str(&format!("G1 X{x} Y{} E0.2 F600\n", 20 + i % 5));
        }
        let path = self.0.join("slow.gcode");
        std::fs::write(&path, gcode).expect("write slow file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_for(shell: &Shell, what: &str, cond: impl Fn(&sprint_host::PrinterState) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !shell.printer.with_state(&cond) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl Shell {
    /// Run every line, ignoring the end of the input.
    fn run_lines_until_eof_without_leaving(&mut self, input: &mut dyn LineSource) {
        while let Read::Line(line) = input.read("") {
            self.execute_line(&line, input);
        }
    }

    /// Like [`Shell::run`] but without disconnecting at the end.
    fn run_lines(&mut self, input: &mut dyn LineSource) {
        loop {
            let prompt = self.prompt();
            match input.read(&prompt) {
                Read::Line(text) => {
                    if self.execute_input(&text, input) == Flow::Exit {
                        break;
                    }
                }
                Read::Interrupted => {}
                Read::Eof => {
                    if self.leave(input) {
                        break;
                    }
                }
            }
        }
        self.echo.settle();
    }
}

#[test]
fn connecting_reports_the_firmware_without_the_setup_noise() {
    let (mut shell, buffer) = shell();
    assert_eq!(shell.prompt(), "sprint (offline)> ");
    shell.connect(fast_sim());
    shell.echo.settle();
    let text = buffer.text();
    assert!(text.contains("Connected to Virtual printer"), "{text}");
    assert!(
        text.contains("Ready  Marlin 2.1.2.1 (Sprint Virtual) · Sprint Virtual Printer"),
        "{text}"
    );
    assert!(text.contains("Connecting to Virtual printer"), "{text}");
    // The replies to M115 and M503 are not echoed.
    assert!(!text.contains("Cap:"), "{text}");
    assert!(!text.contains("echo:  M92"), "{text}");
    assert_eq!(shell.prompt(), "sprint> ");
}

#[test]
fn raw_gcode_is_sent_and_replies_are_echoed() {
    let (mut shell, buffer) = connected();
    script(&mut shell, &["M115", "  ; a comment", "", "!M105", "g28 x"]);
    let text = buffer.text();
    assert!(text.contains("FIRMWARE_NAME:Marlin"), "{text}");
    assert!(text.contains("ok T:"), "{text}");
    // Bare oks are not echoed.
    assert!(!text.lines().any(|l| l.trim() == "ok"), "{text}");
    let got = received(&shell);
    for want in ["M115", "M105", "G28 X"] {
        assert!(
            got.iter().any(|c| c.eq_ignore_ascii_case(want)),
            "missing {want} in {got:?}"
        );
    }
}

#[test]
fn printer_commands_send_the_right_gcode() {
    let (mut shell, buffer) = connected();
    script(
        &mut shell,
        &[
            "temp 210",
            "bedtemp 60",
            "temp off 0",
            "fan 50",
            "home x y",
            "move x 10",
            "move z -0.5 300",
            "extrude 5",
            "reverse 2",
            "speed 120",
            "flow 95",
            "babystep 0.05",
            "set M92 E415",
            "preheat pla",
            "cooldown",
        ],
    );
    let got = received(&shell);
    for want in [
        "M104 T0 S210",
        "M140 S60",
        "M104 T0 S0",
        "M106 S128",
        "G28 X Y",
        "G1 X10 F3000",
        "G1 Z-0.5 F300",
        "G1 E5 F300",
        "G1 E-2 F300",
        "M220 S120",
        "M221 T0 S95",
        "M290 Z0.05",
        "M104 T0 S205",
        "M140 S0",
    ] {
        assert!(got.iter().any(|c| c == want), "missing {want} in {got:?}");
    }
    assert!(
        got.iter()
            .any(|c| c.starts_with("M92 ") && c.ends_with("E415")),
        "{got:?}"
    );
    let text = buffer.text();
    assert!(text.contains("Hotend T0 210 °C"), "{text}");
    assert!(text.contains("Preheating for PLA"), "{text}");
    assert!(text.contains("eeprom save keeps it"), "{text}");
}

#[test]
fn reports_print_tables_instead_of_raw_replies() {
    let (mut shell, buffer) = connected();
    script(&mut shell, &["settings"]);
    let text = buffer.text();
    assert!(text.contains("Steps per unit"), "{text}");
    assert!(!text.contains("echo:"), "{text}");
    buffer.clear();
    script(&mut shell, &["settings motion"]);
    let text = buffer.text();
    assert!(text.contains("Maximum feedrate"), "{text}");
    assert!(!text.contains("Hotend PID"), "{text}");
    buffer.clear();
    script(&mut shell, &["firmware", "endstops", "stats", "sd ls"]);
    let text = buffer.text();
    for fact in ["Marlin", "x_min", "Printer statistics", "Link"] {
        assert!(text.contains(fact), "missing {fact} in\n{text}");
    }
    assert!(!text.contains("Cap:"), "{text}");
    // No mesh yet: said quickly, without waiting for a timeout.
    buffer.clear();
    let asked = Instant::now();
    script(&mut shell, &["mesh"]);
    assert!(asked.elapsed() < REPORT_TIMEOUT, "{:?}", asked.elapsed());
    let text = buffer.text();
    assert!(text.contains("No bed mesh reported"), "{text}");
    assert!(!text.contains("did not answer"), "{text}");
    buffer.clear();
    script(&mut shell, &["mesh probe"]);
    let text = buffer.text();
    assert!(text.contains("Bed mesh: 5 x 5"), "{text}");
    assert!(text.contains("range"), "{text}");
}

#[test]
fn sd_upload_writes_a_file_to_the_card() {
    let (mut shell, buffer) = connected();
    let cube = sample("calibration-cube.gcode");
    // The end of the script waits for the upload job.
    script(
        &mut shell,
        &[&format!("sd upload \"{}\"", cube.display()), "sd ls"],
    );
    let text = buffer.text();
    assert!(
        text.contains("Uploading to the SD card as CALIBRAT.GCO"),
        "{text}"
    );
    wait_for(&shell, "the upload", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    let got = received(&shell);
    assert!(
        got.iter().any(|c| c == "M28 CALIBRAT.GCO"),
        "{:?}",
        &got[..got.len().min(5)]
    );
    assert!(got.iter().any(|c| c.starts_with("M29")), "no M29");
    buffer.clear();
    script(&mut shell, &["sd upload", "sd upload missing.gcode"]);
    let text = buffer.text();
    assert!(text.contains("No file loaded"), "{text}");
    assert!(text.contains("no such file"), "{text}");
}

#[test]
fn a_script_prints_a_file_and_waits_for_it() {
    let (mut shell, buffer) = connected();
    let cube = sample("calibration-cube.gcode");
    script(
        &mut shell,
        &[&format!("load {}", cube.display()), "info", "print"],
    );
    let text = buffer.text();
    assert!(text.contains("Loaded calibration-cube.gcode"), "{text}");
    assert!(text.contains("Slicer"), "{text}");
    assert!(text.contains("Printing calibration-cube.gcode"), "{text}");
    assert!(
        text.contains("Waiting for calibration-cube.gcode to finish"),
        "{text}"
    );
    assert!(
        text.contains("calibration-cube.gcode finished in"),
        "{text}"
    );
    assert!(text.contains("100 layers"), "{text}");
    let status = shell
        .printer
        .with_state(|s| s.job.as_ref().map(|j| j.status.clone()));
    assert_eq!(status, Some(JobStatus::Completed));
    // A second print needs no reload; status shows the finished job.
    buffer.clear();
    script(&mut shell, &["status"]);
    assert!(buffer.text().contains("Completed"), "{}", buffer.text());
}

#[test]
fn monitor_follows_a_print_until_it_ends() {
    let (mut shell, buffer) = connected();
    let cube = sample("calibration-cube.gcode");
    script(&mut shell, &[&format!("load {}", cube.display())]);
    let file = shell.file.clone().expect("loaded");
    shell
        .printer
        .start_print(file.file.clone(), actions::print_options(&file))
        .expect("start");
    buffer.clear();
    script(&mut shell, &["monitor 0.2"]);
    let text = buffer.text();
    assert!(text.contains("Printing"), "{text}");
    assert!(text.contains("T0 "), "{text}");
    wait_for(&shell, "the end of the print", |s| !s.job_active());
}

#[test]
fn ctrl_c_stops_an_interactive_monitor() {
    let (mut shell, buffer) = connected();
    let interrupt = shell.interrupt.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        interrupt.raise();
    });
    let started = Instant::now();
    let mut input = Script::new(["monitor 0.2"], true);
    if let Read::Line(line) = input.read("") {
        shell.execute_line(&line, &mut input);
    }
    let _ = stopper.join();
    assert!(started.elapsed() < Duration::from_secs(10));
    let text = buffer.text();
    assert!(text.contains("Ctrl+C stops"), "{text}");
    assert!(
        text.lines().filter(|l| l.contains("T0 ")).count() >= 2,
        "{text}"
    );
}

#[test]
fn dangerous_gcode_asks_on_a_terminal() {
    let (mut shell, buffer) = connected();
    let mut input = Script::new(["M502", "n", "M502", "y"], true);
    shell.run_lines(&mut input);
    // Typed input does not wait for the queue to drain (scripts do).
    crate::session::wait_idle(&shell.printer, PATIENCE, &Interrupt::new()).expect("idle");
    let text = buffer.text();
    assert!(text.contains("Not sent"), "{text}");
    let got = received(&shell);
    assert_eq!(got.iter().filter(|c| *c == "M502").count(), 1, "{got:?}");
}

#[test]
fn eeprom_reset_needs_yes_in_scripts() {
    let (mut shell, buffer) = connected();
    script(&mut shell, &["eeprom reset"]);
    assert!(
        buffer.text().contains("eeprom reset yes"),
        "{}",
        buffer.text()
    );
    assert!(!received(&shell).iter().any(|c| c == "M502"));
    script(
        &mut shell,
        &["eeprom reset yes", "eeprom save", "eeprom load"],
    );
    let got = received(&shell);
    for want in ["M502", "M500", "M501"] {
        assert!(got.iter().any(|c| c == want), "missing {want} in {got:?}");
    }
}

#[test]
fn log_on_echoes_sent_lines() {
    let (mut shell, buffer) = connected();
    script(&mut shell, &["log", "log on", "G91", "log off", "G90"]);
    let text = buffer.text();
    assert!(text.contains("log is off"), "{text}");
    assert!(text.contains("> G91"), "{text}");
    assert!(!text.contains("> G90"), "{text}");
}

#[test]
fn offline_commands_explain_instead_of_failing() {
    let (mut shell, buffer) = shell();
    script(
        &mut shell,
        &[
            "temp 200",
            "G28",
            "status",
            "info",
            "print",
            "pause",
            "help",
            "help temp",
            "help G28",
            "help M92",
            "help zzz",
            "hoem",
            "temp hot",
            "connect sim 115200",
            "ports",
            "macro",
            "preheat",
            "load no-such-file-5d1.gcode",
        ],
    );
    let text = buffer.text();
    for fact in [
        "Not connected",
        "Printer   ● Disconnected",
        "No file loaded",
        "Not connected to a printer",
        "No job is running",
        "Temperatures and fans",
        "usage: temp <celsius|off> [tool]",
        "G28  Home axes",
        "M92  Set steps per unit",
        "No help for 'zzz'",
        "did you mean 'home'",
        "'hot' is not a temperature",
        "only applies to serial ports",
        "PLA",
        "no-such-file-5d1.gcode: no such file",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
    assert!(!shell.printer.with_state(|s| s.status.is_open()));
}

#[test]
fn exit_asks_while_printing_on_a_terminal() {
    let scratch = Scratch::new("exit");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    shell.print();
    wait_for(&shell, "the print", |s| s.job_active());
    let mut input = Script::new(["exit", "n", "exit", "y"], true);
    let mut flows = Vec::new();
    while let Read::Line(line) = input.read("") {
        flows.push(shell.execute_line(&line, &mut input));
    }
    assert_eq!(flows, [Flow::Continue, Flow::Exit]);
    assert!(!shell.job_active());
    // The question is the prompt; the answer cancelled the print.
    assert!(
        buffer.text().contains("slow.gcode cancelled"),
        "{}",
        buffer.text()
    );
}

#[test]
fn turning_heaters_off_during_a_job_asks_on_a_terminal() {
    let scratch = Scratch::new("cool");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    let mut input = Script::new(["bedtemp 50", "print"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the bed target and the print", |s| {
        s.temps.bed.as_ref().and_then(|b| b.target) == Some(50.0)
            && s.job
                .as_ref()
                .is_some_and(|j| j.status == JobStatus::Printing)
    });
    received(&shell);
    let mut input = Script::new(["cooldown", "n", "bedtemp off", "no", "temp 0"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    assert_eq!(text.matches("Temperatures kept").count(), 2, "{text}");
    // The hotend was not heating: nothing to ask.
    assert!(text.contains("Hotend T0 off"), "{text}");
    // Wait until the M104 went out: the M140s would have gone before it.
    let deadline = Instant::now() + PATIENCE;
    let mut got = Vec::new();
    while !got.iter().any(|c: &String| c.starts_with("M104")) {
        assert!(Instant::now() < deadline, "no M104 in {got:?}");
        std::thread::sleep(Duration::from_millis(5));
        got.extend(received(&shell));
    }
    assert!(!got.iter().any(|c| c.starts_with("M140 S0")), "{got:?}");
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

#[test]
fn pause_resume_cancel_and_the_prompt() {
    let scratch = Scratch::new("pause");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    let mut input = Script::new(["print", "home"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the print", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
    assert!(
        shell.prompt().starts_with("sprint [printing "),
        "{}",
        shell.prompt()
    );
    assert!(
        buffer.text().contains("Not while a job runs"),
        "{}",
        buffer.text()
    );
    let mut input = Script::new(["pause"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the pause", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    assert!(
        shell.prompt().starts_with("sprint [paused "),
        "{}",
        shell.prompt()
    );
    // Moves are allowed while paused, but nothing that lowers the nozzle
    // to the bed (homing Z, probing): the part is there.
    received(&shell);
    let before = buffer.text().len();
    let mut input = Script::new(["home", "home z", "mesh probe", "home x y"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    let refused = text[before..].matches("would lower the nozzle").count();
    assert_eq!(refused, 3, "{text}");
    // A macro asks first on a terminal: here the answer is no.
    let mut input = Script::new(["macro home", "n"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    assert!(buffer.text().contains("Not run"), "{}", buffer.text());
    let mut input = Script::new(["move z 1", "resume"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the resume", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
    let got = received(&shell);
    // The move typed while paused ran before the resume script (whose
    // unretract is G1 E2) took the nozzle back to the print.
    let at = |want: &str| got.iter().position(|c| c == want);
    assert!(
        at("G1 Z1 F600").is_some_and(|m| at("G1 E2 F2700").is_some_and(|r| m < r)),
        "{got:?}"
    );
    assert!(got.iter().any(|c| c == "G28 X Y"), "{got:?}");
    assert!(
        !got.iter().any(|c| c == "G28" || c == "G28 Z" || c == "G29"),
        "{got:?}"
    );
    let mut input = Script::new(["cancel"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the cancel", |s| !s.job_active());
    shell.echo.settle();
    let text = buffer.text();
    assert!(text.contains("Paused slow.gcode"), "{text}");
    assert!(text.contains("slow.gcode cancelled"), "{text}");
}

#[test]
fn a_pasted_block_runs_line_by_line() {
    let (mut shell, buffer) = connected();
    // A block pasted into the line editor arrives as one line with line
    // breaks: each line runs as if typed, so the dangerous one still asks.
    let mut input = Script::new(["G28 X\nM502\r\nM105", "n"], true);
    shell.run_lines(&mut input);
    crate::session::wait_idle(&shell.printer, PATIENCE, &Interrupt::new()).expect("idle");
    let text = buffer.text();
    assert!(text.contains("Not sent"), "{text}");
    assert!(!text.contains("unknown command"), "{text}");
    let got = received(&shell);
    assert!(got.iter().any(|c| c == "G28 X"), "{got:?}");
    assert!(got.iter().any(|c| c == "M105"), "{got:?}");
    assert!(!got.iter().any(|c| c == "M502"), "{got:?}");
}

#[test]
fn disconnecting_says_what_happens_to_the_job() {
    use sprint_host::{JobInfo, JobKind, PrinterState};
    let state = |kind: JobKind, status: JobStatus| {
        let mut job = JobInfo::new(kind, "CUBE.GCO", 100, 1000);
        job.status = status;
        PrinterState {
            job: Some(job),
            ..PrinterState::default()
        }
    };
    assert_eq!(disconnect_question(&PrinterState::default()), None);
    assert_eq!(
        disconnect_question(&state(JobKind::HostPrint, JobStatus::Completed)),
        None
    );
    let host = disconnect_question(&state(JobKind::HostPrint, JobStatus::Paused));
    assert!(host.is_some_and(|q| q.contains("disconnecting cancels it")));
    // A print from the card goes on without Sprint.
    let sd = disconnect_question(&state(JobKind::SdPrint, JobStatus::Printing)).unwrap_or_default();
    assert!(sd.contains("goes on") && !sd.contains("cancels"), "{sd}");
    let upload =
        disconnect_question(&state(JobKind::SdUpload, JobStatus::Printing)).unwrap_or_default();
    assert!(upload.contains("cancels the upload"), "{upload}");
}

#[test]
fn typed_homing_during_a_job_asks_first() {
    let scratch = Scratch::new("typed-home");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    let mut input = Script::new(["print", "pause"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the pause", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    received(&shell);
    // Typed G28 homes Z onto the part like the refused `home`: asked on a
    // terminal (here: no), not sent by a script, sent with `!`.
    let mut input = Script::new(["G28", "n", "G28 X Y", "M84"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let mut input = Script::new(["G28"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    crate::session::wait_idle(&shell.printer, PATIENCE, &Interrupt::new()).expect("idle");
    let got = received(&shell);
    assert!(!got.iter().any(|c| c == "G28"), "{got:?}");
    assert!(!got.iter().any(|c| c == "M84"), "{got:?}");
    assert!(got.iter().any(|c| c == "G28 X Y"), "{got:?}");
    let text = buffer.text();
    assert_eq!(text.matches("Not sent").count(), 3, "{text}");
    let mut input = Script::new(["!M84"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    crate::session::wait_idle(&shell.printer, PATIENCE, &Interrupt::new()).expect("idle");
    assert!(received(&shell).iter().any(|c| c == "M84"));
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

#[test]
fn disconnecting_during_a_print_cancels_it_first() {
    let scratch = Scratch::new("disconnect-cancel");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    let mut input = Script::new(["print"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the print", |s| {
        s.job.as_ref().is_some_and(|j| j.current_line > 10)
    });
    let control = shell.printer.sim_control().expect("simulator");
    control.clear_received();
    let mut input = Script::new(["disconnect", "y"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    // The cancel script ran (heaters off) before the link closed.
    assert!(!shell.printer.with_state(|s| s.status.is_open()));
    assert_eq!(
        shell
            .printer
            .with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Cancelled)
    );
    let got = control.received_commands();
    assert!(got.iter().any(|c| c == "M104 S0"), "{got:?}");
    assert!(
        buffer.text().contains("Cancelling slow.gcode first"),
        "{}",
        buffer.text()
    );
}
