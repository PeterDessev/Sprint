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
        "Printer   ● Connected",
        "usage: temp <celsius|off> [tool]",
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
