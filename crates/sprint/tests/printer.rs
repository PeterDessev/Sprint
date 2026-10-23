//! The printer commands of the `sprint` binary, run as child processes
//! against the built-in virtual printer (`sim` targets). No test here
//! names a serial port: a real printer may be attached to the machine.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// A fast virtual printer.
const FAST_SIM: &str = "sim://?boot_delay_ms=20&time_scale=100&instant_moves=true";

/// A scratch directory for one test, removed on drop; it also holds the
/// configuration file the child uses (never the user's).
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("sprint-printer-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sprint"));
        command
            .args(args)
            .env_remove("RUST_LOG")
            .env("SPRINT_CONFIG", self.0.join("config.toml"))
            .current_dir(&self.0);
        command
    }

    /// Run with no input.
    fn run(&self, args: &[&str]) -> Output {
        self.command(args)
            .stdin(Stdio::null())
            .output()
            .expect("run sprint")
    }

    /// Run with `input` on stdin.
    fn run_with_input(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sprint");
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
        drop(stdin);
        child.wait_with_output().expect("wait for sprint")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `word` as the CLI's hints write one argument: quoted unless it holds
/// only letters, digits and `_-./:\@=,+`.
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

fn sample(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
        .display()
        .to_string()
}

#[test]
fn the_shell_runs_a_script_from_stdin() {
    let scratch = Scratch::new("shell");
    let output = scratch.run_with_input(
        &["shell", "--connect", FAST_SIM],
        "M115\nstatus\nhelp temp\nbogus\nexit\n",
    );
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    for fact in [
        "Connected to Virtual printer",
        "Ready  Marlin",
        "FIRMWARE_NAME:Marlin",
        "Printer    ● Connected",
        "usage: temp <celsius|preset|off> [tool]",
        "unknown command 'bogus'",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
    // Scripts get no prompt and no colors.
    assert!(!text.contains("sprint>"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text:?}");
}

#[test]
fn the_shell_prints_a_file_and_waits_for_it() {
    let scratch = Scratch::new("shell-print");
    let cube = sample("calibration-cube.gcode");
    let output =
        scratch.run_with_input(&["shell", "-c", FAST_SIM], &format!("load {cube}\nprint\n"));
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    assert!(text.contains("Loaded calibration-cube.gcode"), "{text}");
    assert!(
        text.contains("calibration-cube.gcode finished in"),
        "{text}"
    );
    // The loaded file became a recent file in the scratch configuration.
    let config = std::fs::read_to_string(scratch.0.join("config.toml")).unwrap_or_default();
    assert!(config.contains("calibration-cube.gcode"), "{config}");
}

#[test]
fn the_shell_without_a_target_stays_offline() {
    // The default profile does not auto-connect.
    let scratch = Scratch::new("shell-offline");
    let output = scratch.run_with_input(&["shell"], "status\nM105\n");
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    assert!(text.contains("● Disconnected"), "{text}");
    assert!(text.contains("Not connected"), "{text}");
    // No input at all: connect, then leave at once.
    let output = scratch.run(&["shell", "--connect", FAST_SIM]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Ready"), "{}", stdout(&output));
}

#[test]
fn send_prints_the_firmware_line() {
    let scratch = Scratch::new("send-m115");
    let output = scratch.run(&["send", "--connect", "sim", "M115"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    let firmware = text
        .lines()
        .find(|l| l.starts_with("FIRMWARE_NAME:"))
        .unwrap_or_else(|| panic!("no firmware line in\n{text}"));
    assert!(firmware.contains("Marlin"), "{firmware}");
    assert!(text.lines().any(|l| l.starts_with("Cap:")), "{text}");
    // The command's ok ends its replies.
    assert_eq!(text.lines().last(), Some("ok"), "{text}");
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
}

#[test]
fn send_runs_each_argument_in_order() {
    let scratch = Scratch::new("send-many");
    let output = scratch.run(&[
        "send",
        "-c",
        FAST_SIM,
        "G28 X",
        "M105",
        "M114 ; where are we",
        "M9999",
    ]);
    let text = stdout(&output);
    // M9999 is unknown: every command ran, and the exit status says so.
    assert_eq!(output.status.code(), Some(1), "{text}\n{}", stderr(&output));
    let temp = text
        .find("ok T:")
        .unwrap_or_else(|| panic!("no M105 reply in\n{text}"));
    let position = text
        .find("X:")
        .unwrap_or_else(|| panic!("no M114 reply in\n{text}"));
    assert!(temp < position, "{text}");
    assert!(text.contains("Unknown command"), "{text}");
}

#[test]
fn send_gives_up_after_the_timeout() {
    // A real-time printer dwelling for 5 s; the ok cannot come within 1 s.
    let scratch = Scratch::new("send-timeout");
    let started = std::time::Instant::now();
    let output = scratch.run(&[
        "send",
        "-c",
        "sim://?boot_delay_ms=20",
        "--timeout",
        "1",
        "G4 S5",
        "M105",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(
        err.contains("no ok for G4 S5 and no sign of progress within"),
        "{err}"
    );
    assert!(err.contains("remaining commands were not sent"), "{err}");
    assert!(!stdout(&output).contains("ok T:"), "{}", stdout(&output));
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

#[test]
fn send_reports_an_unreachable_printer() {
    // Loopback port 1 refuses connections: nothing leaves this machine.
    let scratch = Scratch::new("send-refused");
    let output = scratch.run(&["send", "-c", "tcp://127.0.0.1:1", "M105"]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.starts_with("sprint: error: cannot connect to"), "{err}");
}

/// The print test's virtual printer.
const PRINT_SIM: &str = "sim://?instant_moves=true&time_scale=100";

#[test]
fn print_streams_the_cube_to_the_simulator() {
    let scratch = Scratch::new("print");
    let cube = sample("calibration-cube.gcode");
    let output = scratch.run(&["print", &cube, "--connect", PRINT_SIM]);
    let (out, err) = (stdout(&output), stderr(&output));
    assert!(output.status.success(), "{out}\n{err}");
    assert!(
        out.starts_with("Completed calibration-cube.gcode: 100.0% in "),
        "{out}"
    );
    // Progress lines (stderr is not a terminal: one per 10%).
    assert!(err.contains("Connecting to Virtual printer"), "{err}");
    assert!(
        err.contains("Printing calibration-cube.gcode on Virtual printer: 100 layers"),
        "{err}"
    );
    assert!(
        err.lines().any(|l| l.starts_with("Completed 100.0%")),
        "{err}"
    );
    assert!(err.contains("layer "), "{err}");
    assert!(err.contains("T0 "), "{err}");
}

#[test]
fn print_without_progress_only_reports_the_result() {
    let scratch = Scratch::new("print-quiet");
    let cube = sample("calibration-cube.gcode");
    let output = scratch.run(&["print", &cube, "-c", PRINT_SIM, "--no-progress"]);
    let (out, err) = (stdout(&output), stderr(&output));
    assert!(output.status.success(), "{out}\n{err}");
    assert!(out.contains("100.0%"), "{out}");
    assert!(err.is_empty(), "{err}");
}

#[test]
fn print_resumes_from_a_line() {
    let scratch = Scratch::new("print-resume");
    let cube = sample("calibration-cube.gcode");
    let output = scratch.run(&[
        "print",
        &cube,
        "-c",
        PRINT_SIM,
        "--start-line",
        "6000",
        "--no-progress",
    ]);
    let (out, err) = (stdout(&output), stderr(&output));
    assert!(output.status.success(), "{out}\n{err}");
    assert!(
        out.contains("Completed calibration-cube.gcode: 100.0%"),
        "{out}"
    );
}

#[test]
fn print_checks_the_file_before_connecting() {
    let scratch = Scratch::new("print-bad");
    let cube = sample("calibration-cube.gcode");
    // Past the end: refused before any connection.
    let output = scratch.run(&["print", &cube, "-c", PRINT_SIM, "--start-line", "99999999"]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("is past the end of calibration-cube.gcode"),
        "{err}"
    );
    assert!(!err.contains("Connecting"), "{err}");
    let output = scratch.run(&["print", "no-such-file-3a.gcode", "-c", PRINT_SIM]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert_eq!(
        err.trim(),
        "sprint: error: no-such-file-3a.gcode: no such file"
    );
    // A 3D model is never streamed: refused before connecting.
    let stl = scratch.0.join("cube.stl");
    std::fs::write(
        &stl,
        "solid cube\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   \
         vertex 0 1 0\n  endloop\n endfacet\nendsolid cube\n",
    )
    .expect("write");
    let output = scratch.run(&["print", &stl.display().to_string(), "-c", PRINT_SIM]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("cube.stl: This is a 3D model, not G-code: slice it first."),
        "{err}"
    );
    assert!(!err.contains("Connecting"), "{err}");
}

#[test]
fn a_failed_print_turns_the_heaters_off_before_exiting() {
    let scratch = Scratch::new("print-failed");
    // The long M117 overflows the firmware's line buffer: the engine gives
    // up on the job while the link still works.
    let mut gcode = String::from("M104 S200\nM140 S60\nG28\n");
    for i in 0..20 {
        gcode.push_str(&format!("G1 X{} Y20 F3000\n", 20 + i % 2));
    }
    gcode.push_str(&format!("M117 {}\nG1 X30 Y30\n", "x".repeat(600)));
    let path = scratch.0.join("long.gcode");
    std::fs::write(&path, gcode).expect("write");
    let output = scratch.run(&[
        "print",
        &path.display().to_string(),
        "-c",
        PRINT_SIM,
        "--no-progress",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.contains("Turning the heaters off"), "{err}");
    assert!(err.contains("long.gcode failed at"), "{err}");
    assert!(err.contains("the heaters are off"), "{err}");
    // How to go on from where it stopped: the long M117, line 24. The
    // nozzle stayed there, and a board reset before resuming loses Z, so
    // the command declares it. (The path is quoted when a shell would
    // split or expand it: a temp dir such as `C:\Users\RUNNER~1\...`.)
    let resume = format!(
        "resume with: sprint print {} --start-line 24 --connect \"sim://",
        shell_word(&path.display().to_string())
    );
    assert!(err.contains(&resume), "{err}");
    let line = err
        .lines()
        .find(|l| l.contains("resume with: "))
        .expect("resume line");
    assert!(line.ends_with("\" --board-reset"), "{err}");
}

#[test]
fn print_pauses_at_the_layers_asked_for() {
    let scratch = Scratch::new("print-pause-layer");
    let cube = sample("calibration-cube.gcode");
    // A layer the file does not have: refused before connecting.
    let output = scratch.run(&["print", &cube, "-c", PRINT_SIM, "--pause-at-layer", "101"]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("--pause-at-layer 101: calibration-cube.gcode has 100 layers"),
        "{err}"
    );
    assert!(!err.contains("Connecting"), "{err}");
    // Paused before layer 2; Enter on standard input resumes it.
    let output = scratch.run_with_input(
        &[
            "print",
            &cube,
            "-c",
            PRINT_SIM,
            "--pause-at-layer",
            "2",
            "--progress-on-printer",
        ],
        "\n",
    );
    let err = stderr(&output);
    assert!(output.status.success(), "{err}");
    assert!(
        err.contains("Pausing before layer 2 (Enter resumes)"),
        "{err}"
    );
    assert!(
        err.contains("Paused before layer 2 (--pause-at-layer)"),
        "{err}"
    );
    assert!(stdout(&output).contains("Completed calibration-cube.gcode"));
}

#[test]
fn print_fails_when_the_printer_is_unreachable() {
    let scratch = Scratch::new("print-refused");
    let cube = sample("calibration-cube.gcode");
    let output = scratch.run(&["print", &cube, "-c", "tcp://127.0.0.1:1", "--no-progress"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("sprint: error: cannot connect to"),
        "{}",
        stderr(&output)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn send_and_print_use_the_profile_named() {
    use sprint_host::{AppConfig, ConnectTarget, PrinterProfile};
    let scratch = Scratch::new("profiles");
    let config = AppConfig {
        profiles: vec![
            // The active one: loopback with nothing listening (never a serial
            // port), so using it by mistake fails.
            PrinterProfile {
                name: "Loopback".into(),
                connection: ConnectTarget::parse("tcp://127.0.0.1:1").expect("target"),
                ..PrinterProfile::default()
            },
            PrinterProfile {
                name: "Bench sim".into(),
                connection: ConnectTarget::parse(FAST_SIM).expect("target"),
                ..PrinterProfile::default()
            },
        ],
        ..AppConfig::default()
    };
    config
        .save_to(&scratch.0.join("config.toml"))
        .expect("save the configuration");
    let output = scratch.run(&["send", "-c", "profile", "--profile", "bench", "M115"]);
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).contains("FIRMWARE_NAME:Marlin"));
    // Without --profile, the active profile's printer.
    let output = scratch.run(&["send", "-c", "profile", "M115"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("cannot connect to tcp://127.0.0.1:1"),
        "{}",
        stderr(&output)
    );
    let output = scratch.run(&[
        "print",
        &sample("calibration-cube.gcode"),
        "-c",
        "profile",
        "-p",
        "2",
        "--no-progress",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Completed calibration-cube.gcode"));
    let output = scratch.run(&["send", "-c", "sim", "-p", "voron", "M105"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("--profile: no profile 'voron' (profiles: Loopback, Bench sim)"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_shell_runs_execute_commands_instead_of_its_input() {
    let scratch = Scratch::new("shell-execute");
    let cube = sample("calibration-cube.gcode");
    let output = scratch.run_with_input(
        &[
            "shell",
            &cube,
            "-c",
            FAST_SIM,
            "-e",
            "M115",
            "--execute",
            "info",
        ],
        "bogus\n",
    );
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    for fact in [
        "Loaded calibration-cube.gcode",
        "FIRMWARE_NAME:Marlin",
        "Layers",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
    // Standard input is not read.
    assert!(!text.contains("bogus"), "{text}");
}

/// A configuration file with one broken line (a decimal comma) and a
/// macro that must survive it.
const BROKEN_CONFIG: &str = "console_max_lines = 5,5\n\n[[macros]]\nname = \"Kept\"\n\
                             gcode = \"M117 kept\"\nconfirm = false\n";

#[test]
fn a_partly_broken_configuration_is_reported_and_still_saved() {
    let scratch = Scratch::new("broken-config");
    let config = scratch.0.join("config.toml");
    std::fs::write(&config, BROKEN_CONFIG).expect("write config");
    let cube = sample("calibration-cube.gcode");
    let output = scratch.run_with_input(&["shell"], &format!("macro\nload {cube}\n"));
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    // What was left out, where the original is, and which file is used.
    assert!(
        text.contains(&format!("Configuration {}: ", config.display())),
        "{text}"
    );
    assert!(text.contains("config.toml.bak"), "{text}");
    assert!(text.contains("Kept"), "{text}");
    assert!(
        text.contains(&format!("Configuration file: {}", config.display())),
        "{text}"
    );
    // The broken line did not stop the recent file from being saved, and
    // the macro is still there.
    let saved = std::fs::read_to_string(&config).expect("read config");
    assert!(saved.contains("calibration-cube.gcode"), "{saved}");
    assert!(saved.contains("M117 kept"), "{saved}");
    let backup = std::fs::read_to_string(scratch.0.join("config.toml.bak")).expect("backup");
    assert_eq!(backup, BROKEN_CONFIG);
    // The headless commands say it on stderr.
    std::fs::write(&config, BROKEN_CONFIG).expect("write config");
    let output = scratch.run(&["send", "--connect", FAST_SIM, "M105"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("sprint: warning: Configuration "),
        "{}",
        stderr(&output)
    );
}
