//! The `sprint` binary run as a child process.
//!
//! Nothing here connects to a printer (tests/printer.rs drives the
//! printer commands against the virtual printer); `sim` serves the virtual
//! printer on loopback.

use std::path::PathBuf;
use std::process::{Command, Output};

use sprint_gcode::{AnalyzeOptions, GcodeFile, analyze_simple};

fn sprint(args: &[&str]) -> Output {
    // Never the user's configuration (its profile could auto-connect).
    let config = std::env::temp_dir().join(format!(
        "sprint-cli-config-{}/config.toml",
        std::process::id()
    ));
    Command::new(env!("CARGO_BIN_EXE_sprint"))
        .args(args)
        .env_remove("RUST_LOG")
        .env("SPRINT_CONFIG", config)
        .output()
        .expect("run sprint")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

fn sample_arg(name: &str) -> String {
    sample(name).display().to_string()
}

/// A scratch directory for this test process, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("sprint-cli-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn file(&self, name: &str, text: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, text).expect("write scratch file");
        path.display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn help_lists_every_subcommand() {
    let output = sprint(&["--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Usage: sprint [OPTIONS] [FILE]"), "{text}");
    for command in [
        "gui", "tui", "shell", "print", "analyze", "send", "ports", "sim",
    ] {
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with(&format!("{command} "))),
            "{command} missing from\n{text}"
        );
    }
    let output = sprint(&["analyze", "--help"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("--filament-diameter <MM>"));
}

#[test]
fn help_says_what_the_configuration_and_targets_do() {
    let output = sprint(&["--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    // Clap wraps the text: compare it with single spaces.
    let text = stdout(&output)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for fact in [
        "COM3 (115200 baud)",
        "COM3@auto detects the rate",
        "at startup (GUI, TUI and shell) to the active profile's printer \
         when the profile turns auto_connect on",
        "SPRINT_CONFIG names another configuration file; every front end uses it, \
         the GUI included",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
    assert!(!text.contains("in the TUI and the shell to the"), "{text}");
}

#[test]
fn print_help_explains_recovery_pauses_progress_and_hooks() {
    let output = sprint(&["print", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for fact in [
        "the line it stopped at is printed as the command that resumes it (--start-line)",
        "Recover an interrupted print: start at line N of the file",
        "after a resume preamble that heats to the temperatures in effect at line N",
        "homes X and Y (never Z: the part is below the nozzle)",
        "--pause-at-layer <L>",
        "Enter resumes the print",
        "--progress-on-printer",
        "on_start_command, on_finish_command, on_fail_command and on_cancel_command",
        "SPRINT_FILE",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
}

#[test]
fn shell_help_explains_scripts() {
    let output = sprint(&["shell", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for fact in [
        "[FILE]",
        "-e, --execute <CMD>",
        "sprint shell -c T < script.txt",
        "waits for the queued commands and for a running print to finish",
        "-p, --profile <NAME>",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
}

#[test]
fn version_is_printed() {
    let output = sprint(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output).trim(),
        format!("sprint {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn analyze_reports_the_calibration_cube() {
    let output = sprint(&["analyze", &sample_arg("calibration-cube.gcode")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    for fact in [
        "calibration-cube.gcode",
        "Slicer       PrusaSlicer 2.7.1",
        "Print time   ",
        "(slicer ",
        "Filament     ",
        "Material     PLA, 1.75 mm filament",
        "Size         ",
        "Bounds       X ",
        "Layers       100, 0.20 mm",
        "Print speed  ",
        "Moves        ",
        "Tools        T0",
        "Hotend       215 °C",
        "Bed          60 °C",
        "Thumbnails   2: 16x16 PNG, 220x124 PNG",
        "Flags        relative E",
        "External perimeter",
        "Internal infill",
        "Travel",
        "Total",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
    // No progress line when stderr is not a terminal.
    assert!(
        !stderr(&output).contains("Analyzing"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn analyze_json_matches_the_library() {
    let path = sample("calibration-cube.gcode");
    let output = sprint(&["analyze", "--json", &path.display().to_string()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    let file = GcodeFile::load(&path).expect("sample");
    let toolpath = analyze_simple(&file, &AnalyzeOptions::default());
    let stats = &toolpath.stats;
    let number = |v: &serde_json::Value| v.as_f64().map(|n| n as f32);
    assert_eq!(json["file"]["name"], "calibration-cube.gcode");
    assert_eq!(json["file"]["lines"], file.len());
    assert_eq!(json["file"]["commands"], file.command_count());
    assert_eq!(
        number(&json["stats"]["estimated_time"]),
        Some(stats.estimated_time)
    );
    assert_eq!(
        number(&json["stats"]["filament_length"]),
        Some(stats.filament_length)
    );
    assert_eq!(
        number(&json["stats"]["filament_weight"]),
        Some(stats.filament_weight)
    );
    assert_eq!(json["stats"]["layer_count"], stats.layer_count);
    assert_eq!(json["stats"]["extrude_moves"], stats.extrude_moves);
    assert_eq!(json["stats"]["travel_moves"], stats.travel_moves);
    assert_eq!(
        json["stats"]["features"].as_array().map(Vec::len),
        Some(stats.features.len())
    );
    assert_eq!(
        json["metadata"]["slicer"].as_str(),
        toolpath.metadata.slicer.as_deref()
    );
    assert_eq!(
        number(&json["metadata"]["estimated_time"]),
        toolpath.metadata.estimated_time
    );
    let thumbnails = json["metadata"]["thumbnails"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(thumbnails.len(), 2);
    for thumb in &thumbnails {
        assert!(
            thumb.get("data").is_none(),
            "thumbnail bytes leaked: {thumb}"
        );
        assert!(thumb["width"].is_number());
    }
    for axis in 0..3 {
        assert_eq!(
            number(&json["bounds"]["min"][axis]),
            Some(toolpath.bounds.min[axis])
        );
        assert_eq!(
            number(&json["bounds"]["max"][axis]),
            Some(toolpath.bounds.max[axis])
        );
    }
    assert_eq!(
        json["layers"].as_array().map(Vec::len),
        Some(toolpath.layers.len())
    );
    assert_eq!(number(&json["layers"][0]["z"]), Some(toolpath.layers[0].z));
}

#[test]
fn filament_diameter_applies_without_metadata() {
    let scratch = Scratch::new("diameter");
    let path = scratch.file(
        "plain.gcode",
        "M83\nG1 Z0.2 F600\nG1 X10 Y10\nG1 X50 Y10 E5 F1200\nG1 X50 Y50 E5\n",
    );
    let volume = |args: &[&str]| {
        let mut all = vec!["analyze", "--json"];
        all.extend_from_slice(args);
        all.push(&path);
        let output = sprint(&all);
        assert!(output.status.success(), "{}", stderr(&output));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
        json["stats"]["filament_volume"]
            .as_f64()
            .unwrap_or(f64::NAN)
    };
    let default = volume(&[]);
    let thick = volume(&["--filament-diameter", "2.85"]);
    let ratio = thick / default;
    let expected = (2.85f64 / 1.75).powi(2);
    assert!((ratio - expected).abs() < 1e-3, "{ratio} vs {expected}");
}

#[test]
fn missing_file_is_a_friendly_error() {
    let output = sprint(&["analyze", "no-such-file-7c1e.gcode"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let text = stderr(&output);
    assert_eq!(
        text.trim(),
        "sprint: error: no-such-file-7c1e.gcode: no such file"
    );
    let scratch = Scratch::new("dir");
    let output = sprint(&["analyze", &scratch.0.display().to_string()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("is a directory"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn empty_and_binary_files_still_get_a_report() {
    // Analysed all the same, but called out: their statistics mean nothing.
    let scratch = Scratch::new("odd");
    let empty = scratch.file("empty.gcode", "");
    let output = sprint(&["analyze", &empty]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("0 lines"));
    assert!(
        stderr(&output)
            .contains("empty.gcode: The file is empty. The statistics below mean nothing."),
        "{}",
        stderr(&output)
    );
    let binary_path = scratch.0.join("image.png");
    let bytes: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    std::fs::write(&binary_path, bytes).expect("write");
    let output = sprint(&["analyze", "--json", &binary_path.display().to_string()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert!(json["stats"].is_object());
    assert_eq!(json["file"]["content"], "Binary");
    assert!(
        stderr(&output).contains("This is a binary file, not G-code."),
        "{}",
        stderr(&output)
    );
    // A model: the warning says what to do.
    let stl = scratch.file(
        "cube.stl",
        "solid cube\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   \
         vertex 0 1 0\n  endloop\n endfacet\nendsolid cube\n",
    );
    let output = sprint(&["analyze", &stl]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("This is a 3D model, not G-code: slice it first."),
        "{}",
        stderr(&output)
    );
}

#[test]
fn ports_are_listed_without_opening_them() {
    let output = sprint(&["ports"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.starts_with("PORT ") || text.starts_with("No serial ports found."),
        "{text}"
    );
    // Piped output has no colors.
    assert!(!text.contains('\u{1b}'), "{text:?}");
}

#[test]
fn the_tui_needs_a_terminal() {
    // Piped output: the TUI refuses before connecting to anything.
    let output = sprint(&["tui", "--connect", "sim"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains("the terminal UI needs an interactive terminal"),
        "{text}"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn usage_errors_exit_with_2() {
    for args in [
        vec!["analyze"],
        vec!["analyze", "x.gcode", "--filament-diameter", "0"],
        vec!["print", "x.gcode"],
        vec!["send", "--connect", "sim"],
        vec!["send", "--connect", "nope://x", "M105"],
        vec!["shell", "--connect", ""],
        vec!["sim", "--extruders", "9"],
        vec!["sim", "--time-scale", "fast"],
        vec!["a.gcode", "b.gcode"],
    ] {
        let output = sprint(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(stderr(&output).contains("error:"), "{args:?}");
    }
}
