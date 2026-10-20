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
    // The default profile does not show the progress on the printer.
    assert!(!text.contains("(M73)"), "{text}");
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
fn the_profile_can_show_the_progress_on_the_printer() {
    let (mut shell, buffer) = connected();
    shell.profile.emit_progress = true;
    let arcs = sample("arcs-test.gcode");
    script(&mut shell, &[&format!("load {}", arcs.display()), "print"]);
    let text = buffer.text();
    assert!(
        text.contains("Progress shows on the printer's display (M73)."),
        "{text}"
    );
    let got = received(&shell);
    assert!(got.iter().any(|c| c.starts_with("M73 P")), "{got:?}");
}

#[test]
fn monitor_follows_a_print_until_it_ends() {
    let (mut shell, buffer) = connected();
    let cube = sample("calibration-cube.gcode");
    script(&mut shell, &[&format!("load {}", cube.display())]);
    let file = shell.file.clone().expect("loaded");
    shell
        .printer
        .start_print(
            file.file.clone(),
            actions::print_options(&file, &shell.profile),
        )
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
            "temp 2hot",
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
        "Printer    ● Disconnected",
        "No file loaded",
        "Not connected to a printer",
        "No job is running",
        "Temperatures and fans",
        "usage: temp <celsius|preset|off> [tool]",
        "G28  Home axes",
        "M92  Set steps per unit",
        "No help for 'zzz'",
        "did you mean 'home'",
        "'2hot' is not a temperature",
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
    shell.print(&[]);
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
    // Moves are allowed while paused, but what lowers the nozzle to the
    // bed (homing Z, probing) asks first, and a script is told why instead.
    received(&shell);
    let before = buffer.text().len();
    let mut input = Script::new(["home", "home z", "mesh probe", "home x y"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    let refused = text[before..]
        .matches("Not done: slow.gcode is paused. ")
        .count();
    assert_eq!(refused, 3, "{text}");
    assert!(
        text[before..].contains("lowers the nozzle onto the bed"),
        "{text}"
    );
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

/// A terminal whose answer to the question comes only after the print
/// was resumed elsewhere (at the printer).
struct AnswersAfterResume {
    printer: Printer,
    answer: Option<String>,
}

impl LineSource for AnswersAfterResume {
    fn read(&mut self, _prompt: &str) -> Read {
        let Some(answer) = self.answer.take() else {
            return Read::Eof;
        };
        self.printer.resume();
        let printing = |s: &sprint_host::PrinterState| {
            s.job
                .as_ref()
                .is_some_and(|j| j.status == JobStatus::Printing)
        };
        let deadline = Instant::now() + PATIENCE;
        while !self.printer.with_state(printing) {
            assert!(Instant::now() < deadline, "the print never went on");
            std::thread::sleep(Duration::from_millis(5));
        }
        Read::Line(answer)
    }

    fn interactive(&self) -> bool {
        true
    }
}

#[test]
fn a_move_answered_after_the_print_went_on_is_not_made() {
    let scratch = Scratch::new("paused-answer");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    let mut input = Script::new(["print", "pause"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the pause", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    shell.echo.settle();
    received(&shell);
    buffer.clear();
    // Homing Z while paused asks; the answer comes once the print runs
    // again: nothing homes between its lines.
    let mut input = AnswersAfterResume {
        printer: shell.printer.clone(),
        answer: Some("y".into()),
    };
    shell.execute_line("home", &mut input);
    assert!(input.answer.is_none(), "not asked: {}", buffer.text());
    let text = buffer.text();
    assert!(
        text.contains(
            "Not done: slow.gcode is printing. The answer came after the job changed; ask again."
        ),
        "{text}"
    );
    let line = shell
        .printer
        .with_state(|s| s.job.as_ref().map_or(0, |j| j.current_line));
    wait_for(&shell, "more of the print", |s| {
        s.job.as_ref().is_some_and(|j| j.current_line > line + 30)
    });
    let got = received(&shell);
    assert!(!got.iter().any(|c| c.starts_with("G28")), "{got:?}");
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

#[test]
fn gcode_answered_after_the_print_went_on_is_not_sent() {
    let scratch = Scratch::new("paused-gcode-answer");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    let mut input = Script::new(["print"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    // A typed G28, then the Home macro: each asks while the print is
    // paused, and the answer comes once it runs again. Neither homes
    // between its lines.
    for typed in ["G28", "macro home"] {
        shell.printer.pause();
        wait_for(&shell, "the pause", |s| {
            s.job
                .as_ref()
                .is_some_and(|j| j.status == JobStatus::Paused)
        });
        shell.echo.settle();
        received(&shell);
        buffer.clear();
        let mut input = AnswersAfterResume {
            printer: shell.printer.clone(),
            answer: Some("y".into()),
        };
        shell.execute_line(typed, &mut input);
        assert!(
            input.answer.is_none(),
            "{typed}: not asked: {}",
            buffer.text()
        );
        let text = buffer.text();
        assert!(text.contains("slow.gcode is printing"), "{typed}: {text}");
        let line = shell
            .printer
            .with_state(|s| s.job.as_ref().map_or(0, |j| j.current_line));
        wait_for(&shell, "more of the print", |s| {
            s.job.as_ref().is_some_and(|j| j.current_line > line + 30)
        });
        let got = received(&shell);
        assert!(
            !got.iter().any(|c| c.starts_with("G28")),
            "{typed}: {got:?}"
        );
    }
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
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

/// A disconnected shell with `config`, saving to `path`.
fn shell_with(config: AppConfig, path: Option<PathBuf>) -> (Shell, Buffer) {
    let buffer = Buffer::default();
    let out = Out::new(
        Box::new(WriterSink(buffer.clone())),
        ColorSupport::None,
        false,
    );
    let shell = Shell::new(Printer::new(), config, path, out, Interrupt::new());
    (shell, buffer)
}

#[test]
fn profiles_are_listed_switched_and_saved() {
    let scratch = Scratch::new("profiles");
    let path = scratch.0.join("config.toml");
    let mut config = AppConfig::default();
    // Loopback with nothing listening: never a serial port.
    config.profiles.push(sprint_host::PrinterProfile {
        name: "Bench".into(),
        connection: ConnectTarget::parse("tcp://127.0.0.1:1").expect("target"),
        ..sprint_host::PrinterProfile::default()
    });
    config.save_to(&path).expect("save");
    let (mut shell, buffer) = shell_with(config, Some(path.clone()));
    script(&mut shell, &["profile", "macro"]);
    let text = buffer.text();
    assert!(text.contains("*  1  Virtual printer"), "{text}");
    assert!(text.contains("   2  Bench"), "{text}");
    assert!(text.contains("tcp://127.0.0.1:1"), "{text}");
    // Both lists name the file their changes are saved to.
    let file_line = format!("Configuration file: {}", path.display());
    assert_eq!(text.matches(&file_line).count(), 2, "{text}");
    buffer.clear();
    script(&mut shell, &["profile ben"]);
    assert!(
        buffer.text().contains("Using the profile Bench"),
        "{}",
        buffer.text()
    );
    assert_eq!(shell.profile.name, "Bench");
    assert_eq!(shell.printer.profile().name, "Bench");
    let on_disk = AppConfig::load_from(&path).expect("load");
    assert_eq!(on_disk.active_profile, 1);
    // connect --save stores the target in the active profile.
    buffer.clear();
    script(
        &mut shell,
        &["connect sim://?boot_delay_ms=20&time_scale=100&instant_moves=true --save"],
    );
    assert!(shell.printer.is_connected(), "{}", buffer.text());
    assert!(
        buffer.text().contains("as the printer of Bench"),
        "{}",
        buffer.text()
    );
    let on_disk = AppConfig::load_from(&path).expect("load");
    assert!(on_disk.profiles[1].connection.is_simulator());
    assert_eq!(on_disk.profiles[0], AppConfig::default().profiles[0]);
    assert!(shell.profile.connection.is_simulator());
    // Still connected: switching says so; an unknown name is an error.
    buffer.clear();
    script(&mut shell, &["profile 1", "profile nothing"]);
    let text = buffer.text();
    assert!(
        text.contains("Still connected to Virtual printer"),
        "{text}"
    );
    assert!(text.contains("no profile 'nothing'"), "{text}");
    assert_eq!(AppConfig::load_from(&path).expect("load").active_profile, 0);
    shell.printer.disconnect();
}

#[test]
fn pronsole_commands_do_what_they_did_in_pronsole() {
    let scratch = Scratch::new("pronsole");
    let script_file = scratch.0.join("warmup.txt");
    std::fs::write(&script_file, "; a comment\nsettemp 200\nM105\n").expect("write script");
    let (mut shell, buffer) = connected();
    let source = format!("source {}", script_file.display());
    script(
        &mut shell,
        &[
            "settemp pla",
            "bedtemp petg",
            "gettemp",
            "eta",
            "tool 0",
            "home e",
            "ls",
            &source,
            "run_script make.sh",
            "block_until_online",
            "off",
        ],
    );
    let got = received(&shell);
    for want in [
        "M104 T0 S205",
        "M140 S80",
        "T0",
        "G92 E0",
        "M104 T0 S200",
        "M104 T0 S0",
        "M140 S0",
        "M84",
    ] {
        assert!(got.iter().any(|c| c == want), "missing {want} in {got:?}");
    }
    assert!(got.iter().any(|c| c.starts_with("M20")), "{got:?}");
    assert!(
        got.iter().any(|c| c == "M107" || c.starts_with("M106 S0")),
        "{got:?}"
    );
    let text = buffer.text();
    for fact in [
        "Hotend     ",
        "No job is running",
        "Extruder position set to 0 (G92 E0)",
        "run_script is a pronsole command",
        "source <file>",
        "fans off, motors off (M84)",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
    // An unknown preset says which ones there are.
    buffer.clear();
    script(&mut shell, &["settemp nylon"]);
    assert!(
        buffer
            .text()
            .contains("no preset 'nylon' (presets: PLA, PETG"),
        "{}",
        buffer.text()
    );
    // reset resets the board (the virtual printer boots again).
    buffer.clear();
    script(&mut shell, &["reset", "eta"]);
    assert!(shell.printer.is_connected(), "{}", buffer.text());
    assert!(
        buffer
            .text()
            .contains("The board was reset; the printer is ready"),
        "{}",
        buffer.text()
    );
}

#[test]
fn off_during_a_print_asks_and_a_script_leaves_it_on() {
    let scratch = Scratch::new("off-print");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    let mut input = Script::new(["print"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the print", |s| {
        s.job.as_ref().is_some_and(|j| j.current_line > 5)
    });
    received(&shell);
    let mut input = Script::new(["off", "eta"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    assert!(text.contains("Left on"), "{text}");
    assert!(text.contains("Printing slow.gcode"), "{text}");
    assert!(!received(&shell).iter().any(|c| c == "M84"));
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

#[test]
fn a_wait_for_the_user_without_a_prompt_is_told_and_released() {
    // Stock Marlin without host prompts: M0 only says busy: paused for
    // user, and M108 lets it go on.
    let fake = crate::fake_printer::FakePrinter::start();
    let (mut shell, buffer) = shell();
    shell.connect(fake.target());
    assert!(shell.printer.is_connected(), "{}", buffer.text());
    let mut input = Script::new(["M0"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(
        &shell,
        "the wait for the user",
        crate::actions::waiting_for_user,
    );
    let deadline = Instant::now() + PATIENCE;
    while !buffer.text().contains("The printer waits for the user") {
        assert!(Instant::now() < deadline, "not told:\n{}", buffer.text());
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut input = Script::new(["resume"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the release", |s| s.busy.is_none());
    assert!(
        fake.received().iter().any(|c| c == "M108"),
        "{:?}",
        fake.received()
    );
    assert!(
        buffer.text().contains("Continuing (M108)"),
        "{}",
        buffer.text()
    );
    // Told once per wait.
    assert_eq!(
        buffer
            .text()
            .matches("The printer waits for the user")
            .count(),
        1
    );
    shell.printer.disconnect();
}

#[test]
fn macros_are_defined_run_with_arguments_and_deleted() {
    let scratch = Scratch::new("macros");
    let path = scratch.0.join("config.toml");
    let (mut shell, buffer) = shell_with(AppConfig::default(), Some(path.clone()));
    shell.connect(fast_sim());
    assert!(shell.printer.is_connected(), "{}", buffer.text());
    received(&shell);
    // pronsole's way: the lines after `macro define` up to a blank one.
    script(
        &mut shell,
        &[
            "macro define warm",
            "M104 S{0}",
            "M140 S{1=55}",
            "",
            "macro warm 190",
            "macro warm",
            "macro define park G91 | G1 Z{0=7} | G90",
            "macro park",
        ],
    );
    let got = received(&shell);
    for want in ["M104 S190", "M140 S55", "G1 Z7"] {
        assert!(got.iter().any(|c| c == want), "missing {want} in {got:?}");
    }
    let text = buffer.text();
    assert!(text.contains("Saved macro warm (2 lines)"), "{text}");
    assert!(text.contains("macro warm needs 1 argument"), "{text}");
    let on_disk = AppConfig::load_from(&path).expect("saved");
    let warm = on_disk
        .macros
        .iter()
        .find(|m| m.name == "warm")
        .expect("warm saved");
    assert_eq!(warm.gcode, "M104 S{0}\nM140 S{1=55}");
    // Edit and delete.
    buffer.clear();
    script(
        &mut shell,
        &[
            "macro edit warm M104 S{0}",
            "macro rm park",
            "macro rm park",
        ],
    );
    let text = buffer.text();
    assert!(text.contains("Changed macro warm (1 line)"), "{text}");
    assert!(text.contains("Deleted the macro park"), "{text}");
    assert!(text.contains("no macro 'park'"), "{text}");
    let on_disk = AppConfig::load_from(&path).expect("saved");
    assert!(!on_disk.macros.iter().any(|m| m.name == "park"));
    shell.printer.disconnect();
}

/// A simulator with a large file on its card, printed in (fast) real
/// time.
fn sd_sim() -> ConnectTarget {
    ConnectTarget::Simulator(SimConfig {
        boot_delay_ms: 20,
        time_scale: 20.0,
        instant_moves: false,
        sd_files: vec![
            ("big.gcode".into(), 2_000_000),
            ("small.gcode".into(), 1_000),
        ],
        ..SimConfig::default()
    })
}

#[test]
fn sd_files_are_not_deleted_while_the_card_prints() {
    let (mut shell, buffer) = connected_to(sd_sim());
    let mut input = Script::new(["sd ls", "sd print big.gcode"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the SD print", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.bytes_done > 0)
    });
    received(&shell);
    buffer.clear();
    let mut input = Script::new(["sd rm big.gcode", "sd rm small.gcode"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    assert_eq!(
        text.matches("big.gcode is printing from the SD card")
            .count(),
        2,
        "{text}"
    );
    assert!(
        !received(&shell).iter().any(|c| c.starts_with("M30")),
        "M30 sent while printing from the card"
    );
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

#[test]
fn sd_deletes_are_reported_when_the_printer_answers() {
    let (mut shell, buffer) = connected_to(sd_sim());
    let mut input = Script::new(["sd rm nothere.gco", "sd rm small.gcode"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    assert!(text.contains("Deleting nothere.gco (M30)"), "{text}");
    assert!(text.contains("Could not delete nothere.gco"), "{text}");
    assert!(!text.contains("Deleted nothere.gco"), "{text}");
    assert!(text.contains("Deleted small.gcode"), "{text}");
    // Success is said after the printer answered, not before.
    let failed = text.find("Could not delete nothere.gco").unwrap_or(0);
    let next = text.find("Deleting small.gcode").unwrap_or(0);
    assert!(failed < next, "{text}");
}

#[test]
fn a_script_that_ends_paused_cancels_instead_of_waiting_for_ever() {
    let scratch = Scratch::new("paused-end");
    let (mut shell, buffer) = connected_timed();
    // If the end of the script waited for ever, this stops the wait (and
    // the assertions below fail) instead of hanging the test.
    let watchdog = shell.interrupt.clone();
    let (done, finished) = crossbeam_channel::bounded::<()>(0);
    let thread = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(60))
            == Err(crossbeam_channel::RecvTimeoutError::Timeout)
        {
            watchdog.raise();
        }
    });
    let slow = scratch.slow_file();
    script(
        &mut shell,
        &[&format!("load {}", slow.display()), "print", "pause"],
    );
    drop(done);
    let _ = thread.join();
    let text = buffer.text();
    assert!(
        text.contains("slow.gcode is paused and a script cannot resume it"),
        "{text}"
    );
    assert_eq!(
        shell
            .printer
            .with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Cancelled)
    );
    assert!(received(&shell).iter().any(|c| c == "M104 S0"));
}

#[test]
fn a_script_leaves_a_print_that_was_resumed_elsewhere_running() {
    let scratch = Scratch::new("resumed-elsewhere");
    let (mut shell, buffer) = connected_timed();
    // Short enough to finish within the test: a few seconds of moves.
    let mut gcode = String::from("G28\nG90\nM83\nG1 Z0.3 F600\n");
    for i in 0..30 {
        gcode.push_str(&format!("G1 X{} Y20 E0.2 F600\n", 20 + (i % 2) * 5));
    }
    let path = scratch.0.join("short.gcode");
    std::fs::write(&path, gcode).expect("write");
    shell.load(&path);
    let mut input = Script::new(["print", "pause"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the pause", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    // Resumed at the printer (or from another front end), not by the
    // script: at its end the print is waited for, not cancelled.
    shell.printer.resume();
    wait_for(&shell, "the resume", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
    // If the end of the script waited for ever, this stops the wait.
    let watchdog = shell.interrupt.clone();
    let (done, finished) = crossbeam_channel::bounded::<()>(0);
    let thread = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(60))
            == Err(crossbeam_channel::RecvTimeoutError::Timeout)
        {
            watchdog.raise();
        }
    });
    script(&mut shell, &[]);
    drop(done);
    let _ = thread.join();
    let text = buffer.text();
    assert!(!text.contains("a script cannot resume it"), "{text}");
    assert_eq!(
        shell
            .printer
            .with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Completed),
        "{text}"
    );
}

#[test]
fn a_script_does_not_cancel_an_upload_after_a_refused_pause() {
    let (mut shell, buffer) = connected_to(sd_sim());
    let cube = sample("calibration-cube.gcode");
    script(
        &mut shell,
        &[&format!("sd upload {}", cube.display()), "pause"],
    );
    let text = buffer.text();
    assert!(!text.contains("a script cannot resume it"), "{text}");
    let job = shell
        .printer
        .with_state(|s| s.job.as_ref().map(|j| (j.kind, j.status.clone())));
    assert_eq!(
        job,
        Some((JobKind::SdUpload, JobStatus::Completed)),
        "{text}"
    );
}

#[test]
fn a_session_profile_stays_active_after_loading_a_file() {
    let scratch = Scratch::new("session-profile");
    let path = scratch.0.join("config.toml");
    let mut config = AppConfig::default();
    config.profiles.push(sprint_host::PrinterProfile {
        name: "Bench".into(),
        connection: ConnectTarget::parse("tcp://127.0.0.1:1").expect("target"),
        ..sprint_host::PrinterProfile::default()
    });
    config.save_to(&path).expect("save");
    // `sprint shell --profile bench`: this session only.
    crate::config::select_profile(&mut config, Some("bench")).expect("bench");
    let (mut shell, buffer) = shell_with(config, Some(path.clone()));
    let cube = sample("calibration-cube.gcode");
    script(
        &mut shell,
        &[&format!("load {}", cube.display()), "profile"],
    );
    let text = buffer.text();
    assert!(text.contains("*  2  Bench"), "{text}");
    assert!(text.contains("   1  Virtual printer"), "{text}");
    assert_eq!(shell.profile.name, "Bench");
    // The file keeps its own active profile.
    assert_eq!(AppConfig::load_from(&path).expect("load").active_profile, 0);
}

/// Answers yes to the first question, but only after starting a print
/// from the SD card (at the printer, while the question was open).
struct PrintWhileAsked {
    printer: Printer,
    lines: std::collections::VecDeque<String>,
}

impl LineSource for PrintWhileAsked {
    fn read(&mut self, prompt: &str) -> Read {
        if prompt.contains("[y/N]") {
            self.printer
                .start_sd_print("big.gcode")
                .expect("start the SD print");
            let deadline = Instant::now() + PATIENCE;
            while !self.printer.with_state(|s| {
                s.job
                    .as_ref()
                    .is_some_and(|j| j.status == JobStatus::Printing)
            }) {
                assert!(Instant::now() < deadline, "the SD print did not start");
                std::thread::sleep(Duration::from_millis(5));
            }
            return Read::Line("y".into());
        }
        self.lines.pop_front().map_or(Read::Eof, Read::Line)
    }

    fn interactive(&self) -> bool {
        true
    }
}

#[test]
fn an_sd_print_started_while_asked_still_blocks_the_delete() {
    let (mut shell, buffer) = connected_to(sd_sim());
    script(&mut shell, &["sd ls"]);
    received(&shell);
    buffer.clear();
    let mut input = PrintWhileAsked {
        printer: shell.printer.clone(),
        lines: ["sd rm big.gcode".to_string()].into(),
    };
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    assert!(
        text.contains("big.gcode is printing from the SD card"),
        "{text}"
    );
    assert!(
        !received(&shell).iter().any(|c| c.starts_with("M30")),
        "M30 sent while printing from the card"
    );
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

/// Starts the print, then breaks (a bug somewhere in the shell).
struct BreaksWhilePrinting {
    lines: std::collections::VecDeque<String>,
    printer: Printer,
}

impl LineSource for BreaksWhilePrinting {
    fn read(&mut self, _prompt: &str) -> Read {
        if let Some(line) = self.lines.pop_front() {
            return Read::Line(line);
        }
        let deadline = Instant::now() + PATIENCE;
        while !self
            .printer
            .with_state(|s| s.job.as_ref().is_some_and(|j| j.current_line > 5))
        {
            assert!(Instant::now() < deadline, "the print did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the shell broke");
    }

    fn interactive(&self) -> bool {
        false
    }
}

#[test]
fn a_panic_in_the_shell_still_cancels_the_print_before_closing() {
    let scratch = Scratch::new("panic");
    let (mut shell, _buffer) = connected_timed();
    let control = shell.printer.sim_control().expect("simulator");
    let mut gcode = String::from("M104 S200\nM140 S60\n");
    gcode.push_str(&std::fs::read_to_string(scratch.slow_file()).expect("read"));
    let path = scratch.0.join("hot.gcode");
    std::fs::write(&path, gcode).expect("write");
    shell.load(&path);
    let printer = shell.printer.clone();
    let mut input = BreaksWhilePrinting {
        lines: ["print".to_string()].into(),
        printer: printer.clone(),
    };
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shell.run(&mut input);
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

#[test]
fn resume_goes_on_after_a_wait_the_printer_paused_the_print_for() {
    // Marlin with host action commands but no host prompts: M0 pauses the
    // host's print (//action:paused) and waits for the user.
    let fake = crate::fake_printer::FakePrinter::start_announcing_pauses();
    let scratch = Scratch::new("printer-paused");
    let path = scratch.0.join("stop.gcode");
    std::fs::write(&path, "G28\nG1 X10 F3000\nM0\nG1 X20 F3000\nG1 X30 F3000\n").expect("write");
    let (mut shell, buffer) = shell();
    shell.connect(fake.target());
    assert!(shell.printer.is_connected(), "{}", buffer.text());
    shell.load(&path);
    let mut input = Script::new(["print"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the printer's pause", |s| {
        crate::actions::waiting_for_user(s)
            && s.job
                .as_ref()
                .is_some_and(|j| j.status == JobStatus::Paused)
    });
    // If resume waited for an idle link (never: M0 is unanswered until
    // the user goes on), this stops the wait.
    let watchdog = shell.interrupt.clone();
    let (done, finished) = crossbeam_channel::bounded::<()>(0);
    let thread = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(10))
            == Err(crossbeam_channel::RecvTimeoutError::Timeout)
        {
            watchdog.raise();
        }
    });
    let mut input = Script::new(["resume"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    drop(done);
    let _ = thread.join();
    wait_for(&shell, "the end of the print", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    assert!(
        fake.received().iter().any(|c| c == "M108"),
        "{:?}\n{}",
        fake.received(),
        buffer.text()
    );
    shell.printer.disconnect();
}

#[test]
fn gettemp_asks_the_printer_for_its_temperatures() {
    let (mut shell, buffer) = connected();
    // As pronsole does: the report shows the target just set.
    script(&mut shell, &["settemp 205", "gettemp"]);
    let text = buffer.text();
    let report = text
        .lines()
        .find(|l| l.starts_with("Hotend ") && l.contains("power"))
        .unwrap_or_else(|| panic!("no hotend report in\n{text}"));
    assert!(report.contains("/ 205 °C"), "{text}");
    assert!(received(&shell).iter().any(|c| c == "M105"));
}

#[test]
fn monitor_in_a_script_stops_at_a_pause_the_script_made() {
    let scratch = Scratch::new("monitor-paused");
    let (mut shell, buffer) = connected_timed();
    // If monitor waited for ever, this stops it (and the timing below
    // fails) instead of hanging the test.
    let watchdog = shell.interrupt.clone();
    let (done, finished) = crossbeam_channel::bounded::<()>(0);
    let thread = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(20))
            == Err(crossbeam_channel::RecvTimeoutError::Timeout)
        {
            watchdog.raise();
        }
    });
    let started = Instant::now();
    let slow = scratch.slow_file();
    script(
        &mut shell,
        &[
            &format!("load {}", slow.display()),
            "print",
            "pause",
            "monitor 0.2",
        ],
    );
    let took = started.elapsed();
    drop(done);
    let _ = thread.join();
    let text = buffer.text();
    assert!(took < Duration::from_secs(15), "{took:?}\n{text}");
    assert!(
        text.contains("slow.gcode is paused and a script cannot resume it"),
        "{text}"
    );
    assert_eq!(
        shell
            .printer
            .with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Cancelled)
    );
}

#[test]
fn the_shared_classifier_refuses_and_asks_during_a_print() {
    let scratch = Scratch::new("classifier");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    let mut input = Script::new(["print"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the print", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
    received(&shell);
    // Starting an SD print or writing to the card is refused, `!` or not;
    // a move asks (here: no); tuning goes straight out; a macro is checked
    // line by line.
    let mut input = Script::new(
        [
            "M24",
            "!M28 a.gco",
            "G1 X10",
            "n",
            "M220 S110",
            "macro home",
            "no",
        ],
        true,
    );
    shell.run_lines_until_eof_without_leaving(&mut input);
    crate::session::wait_idle(&shell.printer, PATIENCE, &Interrupt::new()).ok();
    let text = buffer.text();
    assert!(
        text.contains("Not sent: slow.gcode is printing. M24 (Start or resume SD print): "),
        "{text}"
    );
    assert!(
        text.contains("Not sent: slow.gcode is printing. M28 "),
        "{text}"
    );
    assert!(text.contains("Not run"), "{text}");
    let got = received(&shell);
    assert!(got.iter().any(|c| c == "M220 S110"), "{got:?}");
    for refused in ["M24", "M28 a.gco", "G1 X10", "G28"] {
        assert!(!got.iter().any(|c| c == refused), "{refused} in {got:?}");
    }
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

#[test]
fn an_emergency_stop_is_never_asked_about() {
    let (mut shell, buffer) = connected();
    let mut input = Script::new(["M112"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the halt", |s| s.halted);
    let text = buffer.text();
    assert!(!text.contains("Send it?"), "{text}");
    // What to do next is said, and status says it too.
    assert!(
        text.contains(
            "Emergency stop sent (M112): reset resets the board (M999 alone does not \
             recover from M112)"
        ),
        "{text}"
    );
    assert_eq!(shell.prompt(), "sprint (halted)> ");
    buffer.clear();
    script(&mut shell, &["status", "print"]);
    let text = buffer.text();
    assert!(
        text.contains("HALTED: reset resets the board (M999 alone does not recover"),
        "{text}"
    );
    assert!(
        text.contains("The printer is halted: reset resets the board"),
        "{text}"
    );
    // `reset` resets the board and waits for the printer.
    buffer.clear();
    script(&mut shell, &["reset"]);
    let text = buffer.text();
    assert!(text.contains("Resetting the board"), "{text}");
    assert!(
        text.contains("The board was reset; the printer is ready"),
        "{text}"
    );
    assert!(shell.printer.with_state(|s| s.is_connected() && !s.halted));
    assert_eq!(shell.prompt(), "sprint> ");
}

#[test]
fn reset_says_why_a_network_printer_cannot_be_reset() {
    // A virtual printer served on loopback: a TCP link, like a network
    // bridge, whose board Sprint cannot reset.
    let server = sprint_host::sprint_sim::SimServer::bind(
        "127.0.0.1:0",
        sprint_host::sprint_sim::SimConfig {
            boot_delay_ms: 20,
            time_scale: 100.0,
            instant_moves: true,
            ..sprint_host::sprint_sim::SimConfig::default()
        },
    )
    .expect("serve the simulator");
    let target = ConnectTarget::parse(&format!("tcp://{}", server.local_addr())).expect("target");
    let (mut shell, buffer) = shell();
    shell.connect(target);
    assert!(shell.printer.is_connected(), "{}", buffer.text());
    script(&mut shell, &["reset"]);
    let text = buffer.text();
    assert!(text.contains("Cannot reset the board: "), "{text}");
    assert!(text.contains("reset button"), "{text}");
    shell.printer.disconnect();
}

#[test]
fn files_that_are_not_gcode_are_not_loaded() {
    let scratch = Scratch::new("not-gcode");
    let stl = scratch.0.join("cube.stl");
    std::fs::write(
        &stl,
        "solid cube\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   \
         vertex 0 1 0\n  endloop\n endfacet\nendsolid cube\n",
    )
    .expect("write");
    let (mut shell, buffer) = connected();
    script(
        &mut shell,
        &[&format!("load {}", stl.display()), "print", "sd upload"],
    );
    let text = buffer.text();
    assert!(
        text.contains("cube.stl: This is a 3D model, not G-code: slice it first."),
        "{text}"
    );
    assert!(text.contains("No file loaded"), "{text}");
    // Nor uploaded to the card.
    buffer.clear();
    script(&mut shell, &[&format!("sd upload {}", stl.display())]);
    assert!(
        buffer.text().contains("This is a 3D model"),
        "{}",
        buffer.text()
    );
    assert!(shell.printer.with_state(|s| s.job.is_none()));
}

/// A file of `layers` layers, a few extrusions each.
fn layered_file(dir: &std::path::Path, layers: usize) -> PathBuf {
    let mut gcode = String::from("G28\nG90\nM83\n");
    for layer in 0..layers {
        gcode.push_str(&format!("G1 Z{:.1} F600\n", 0.2 * (layer + 1) as f64));
        for i in 0..4 {
            gcode.push_str(&format!("G1 X{} Y20 E0.5 F1200\n", 20 + (i % 2) * 10));
        }
    }
    let path = dir.join("layers.gcode");
    std::fs::write(&path, gcode).expect("write layered file");
    path
}

#[test]
fn print_pauses_before_the_layers_asked_for() {
    let scratch = Scratch::new("pause-layer");
    let file = layered_file(&scratch.0, 4);
    // Moves in (fast) real time: the pause points are set while the first
    // layers still stream.
    let (mut shell, buffer) = connected_timed();
    // A layer the file does not have: nothing starts.
    script(
        &mut shell,
        &[
            &format!("load {}", file.display()),
            "print --pause-at-layer 9",
        ],
    );
    let text = buffer.text();
    assert!(
        text.contains("--pause-at-layer 9: layers.gcode has 4 layers"),
        "{text}"
    );
    assert!(shell.printer.with_state(|s| s.job.is_none()));
    // The first layer starts with the file, which streams at once: no
    // pause could be set in time, so it is refused rather than ignored.
    buffer.clear();
    script(&mut shell, &["print --pause-at-layer 1"]);
    let text = buffer.text();
    assert!(
        text.contains("--pause-at-layer 1: the print starts with layer 1"),
        "{text}"
    );
    assert!(!text.contains("Pausing before layer 1"), "{text}");
    assert!(shell.printer.with_state(|s| s.job.is_none()));
    // At a terminal: paused before layer 3, resume goes on.
    buffer.clear();
    let mut input = Script::new(["print --pause-at-layer 3"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    assert!(
        buffer
            .text()
            .contains("Pausing before layer 3 (resume goes on)"),
        "{}",
        buffer.text()
    );
    wait_for(&shell, "the pause", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused && j.pause_at.is_empty())
    });
    let line = shell
        .printer
        .with_state(|s| s.job.as_ref().map(|j| j.current_line));
    let layer_start = shell
        .file
        .as_ref()
        .map(|f| f.toolpath.layers[2].lines.start as usize);
    assert_eq!(line, layer_start, "paused right before layer 3");
    let mut input = Script::new(["resume"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the end", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    // In a script nothing could resume it: the end of the script cancels.
    buffer.clear();
    script(&mut shell, &["print --pause-at-layer 2"]);
    let text = buffer.text();
    assert!(
        text.contains("layers.gcode paused before a layer and a script cannot resume it"),
        "{text}"
    );
    assert_eq!(
        shell
            .printer
            .with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Cancelled)
    );
}

#[test]
fn recover_prints_a_cancelled_file_again_from_where_it_stopped() {
    let scratch = Scratch::new("recover");
    let (mut shell, buffer) = connected_timed();
    shell.load(&scratch.slow_file());
    // Nothing stopped yet.
    script(&mut shell, &["recover"]);
    assert!(
        buffer.text().contains("No failed or cancelled print"),
        "{}",
        buffer.text()
    );
    let mut input = Script::new(["print"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the print", |s| {
        s.job.as_ref().is_some_and(|j| j.current_line > 20)
    });
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Cancelled)
    });
    let stopped = shell
        .printer
        .with_state(|s| s.job.as_ref().and_then(|j| j.stopped_at))
        .expect("stopped at a line");
    received(&shell);
    buffer.clear();
    // The preamble is shown, then asked about (here: no).
    let mut input = Script::new(["recover", "n"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    assert!(
        text.contains(&format!(
            "slow.gcode prints again from line {}, after this preamble:",
            stopped + 1
        )),
        "{text}"
    );
    assert!(text.contains("  G28 X Y"), "{text}");
    assert!(text.contains("Not started"), "{text}");
    // The cancel script lifted the nozzle 10 mm and switched the motors
    // off (Z is no longer known to be homed): the firmware followed the
    // lift, so its Z is trusted. Declaring the resume height up there
    // would print the rest in the air.
    assert!(
        shell
            .printer
            .with_state(|s| s.homed.is_some_and(|axes| !axes.z)),
        "the cancel script switched the motors off"
    );
    assert!(!text.contains("\n  G92 Z"), "{text}");
    assert!(
        text.contains("the cancel script moved the nozzle: the firmware's Z is trusted"),
        "{text}"
    );
    // Yes: it prints from there.
    let mut input = Script::new(["recover", "y"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the recovery", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status.is_active() && j.current_line > stopped)
    });
    assert!(
        buffer
            .text()
            .contains(&format!("Recovering slow.gcode from line {}", stopped + 1)),
        "{}",
        buffer.text()
    );
    let got = received(&shell);
    assert!(got.iter().any(|c| c == "G28 X Y"), "{got:?}");
    assert!(!got.iter().any(|c| c.starts_with("G92 Z")), "{got:?}");
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

/// A plate of two objects labelled with M486, `layers` layers of a few
/// short moves each.
fn two_objects(dir: &std::path::Path, layers: usize) -> PathBuf {
    let mut gcode = String::from("G28\nG90\nM83\nM486 T2\n");
    for layer in 0..layers {
        gcode.push_str(&format!("G1 Z{:.1} F600\n", 0.2 * (layer + 1) as f64));
        for (id, name, x) in [(0, "Cube", 20), (1, "Cylinder", 60)] {
            gcode.push_str(&format!("M486 S{id} A\"{name}\"\n"));
            for i in 0..4 {
                gcode.push_str(&format!("G1 X{} Y20 E0.5 F1200\n", x + (i % 2) * 10));
            }
            gcode.push_str("M486 S-1\n");
        }
    }
    let path = dir.join("plate.gcode");
    std::fs::write(&path, gcode).expect("write plate");
    path
}

#[test]
fn objects_of_a_print_are_listed_excluded_and_included() {
    let scratch = Scratch::new("objects");
    let plate = two_objects(&scratch.0, 30);
    let (mut shell, buffer) = connected_timed();
    shell.load(&plate);
    buffer.clear();
    // Before printing: the file's objects; nothing to exclude yet.
    script(&mut shell, &["objects", "exclude cube"]);
    let text = buffer.text();
    assert!(text.contains("   1  Cube"), "{text}");
    assert!(text.contains("   2  Cylinder"), "{text}");
    assert!(text.contains("No print is running"), "{text}");
    let mut input = Script::new(["print"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the print", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.current_object.is_some() && j.objects.len() == 2)
    });
    buffer.clear();
    let mut input = Script::new(
        ["exclude 2", "exclude cylinder", "objects", "exclude sphere"],
        true,
    );
    shell.run_lines_until_eof_without_leaving(&mut input);
    shell.echo.settle();
    let text = buffer.text();
    assert!(text.contains("Excluded \"Cylinder\""), "{text}");
    assert!(text.contains("Cylinder is excluded already"), "{text}");
    assert!(text.contains("   2  Cylinder  excluded"), "{text}");
    assert!(text.contains("no object 'sphere'"), "{text}");
    assert_eq!(
        shell
            .printer
            .with_state(|s| s.job.as_ref().map(|j| j.excluded_objects.clone())),
        Some(vec!["Cylinder".to_string()])
    );
    // Excluding the last one says so; include prints one again.
    buffer.clear();
    let mut input = Script::new(["exclude cube", "include cube"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    shell.echo.settle();
    let text = buffer.text();
    assert!(text.contains("Every object is excluded now"), "{text}");
    assert!(text.contains("Printing \"Cube\" again"), "{text}");
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

#[test]
fn moves_and_babysteps_warn_while_axes_are_not_homed() {
    let (mut shell, buffer) = connected();
    // Nothing homed since connecting: warned, and still done.
    script(&mut shell, &["move x 5", "babystep 0.05", "extrude 1"]);
    let text = buffer.text();
    assert!(
        text.contains(
            "X not homed: the move starts from where the printer believes it is (home x first)"
        ),
        "{text}"
    );
    assert!(
        text.contains("Z not homed: babysteps shift a Z the printer does not know"),
        "{text}"
    );
    // Extrusion needs no homing.
    assert_eq!(text.matches("not homed").count(), 2, "{text}");
    let got = received(&shell);
    assert!(got.iter().any(|c| c == "G1 X5 F3000"), "{got:?}");
    assert!(got.iter().any(|c| c.starts_with("M290 Z")), "{got:?}");
    buffer.clear();
    script(
        &mut shell,
        &["home", "move x 5", "move z 1", "babystep 0.05"],
    );
    let text = buffer.text();
    assert!(!text.contains("not homed"), "{text}");
}

#[test]
fn a_babystep_during_a_print_does_not_ask_for_homing() {
    // The printer homed for its own SD print, which the host never sees:
    // telling the user to home Z now would wreck the print.
    let (mut shell, buffer) = connected_to(sd_sim());
    let mut input = Script::new(["sd print big.gcode"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    wait_for(&shell, "the SD print", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.bytes_done > 0)
    });
    received(&shell);
    buffer.clear();
    let mut input = Script::new(["babystep 0.05"], false);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    assert!(!text.contains("not homed"), "{text}");
    assert!(text.contains("Z nudged by 0.05 mm"), "{text}");
    shell.printer.cancel();
    wait_for(&shell, "the cancel", |s| !s.job_active());
}

#[test]
fn a_macro_that_asks_shows_its_lines_with_the_placeholders_filled_in() {
    let (mut shell, buffer) = connected();
    shell.config.macros.push(sprint_host::Macro {
        name: "Park and cool".into(),
        gcode: "{heaters_off}\nM107\nG1 Z{max_z}".into(),
        color: None,
        confirm: true,
    });
    let mut input = Script::new(["macro park and cool", "n"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    for line in ["  M104 S0", "  M140 S0", "  M107", "  G1 Z250"] {
        assert!(
            text.lines().any(|l| l == line),
            "missing {line:?} in\n{text}"
        );
    }
    assert!(!text.contains('{'), "{text}");
    assert!(text.contains("Not run"), "{text}");
    // A macro that does not ask runs without the listing.
    buffer.clear();
    let mut input = Script::new(["macro home"], true);
    shell.run_lines_until_eof_without_leaving(&mut input);
    let text = buffer.text();
    assert!(!text.contains("  G28"), "{text}");
    assert!(text.contains("Macro Home"), "{text}");
}
