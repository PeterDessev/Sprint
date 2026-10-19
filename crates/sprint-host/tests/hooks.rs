//! Job hooks: the profile's start, finish, fail and cancel commands run
//! with the job in their environment, and their output reaches the
//! console. Only harmless `echo` commands are run.

mod common;

use std::sync::Arc;

use common::*;
use sprint_host::sprint_gcode::GcodeFile;
use sprint_host::{ConsoleKind, JobStatus, PrintOptions, Printer, PrinterProfile};

/// `echo <tag> <status> <printer> <file>` (and `<reason>`) in the
/// platform shell.
fn echo(tag: &str, reason: bool) -> String {
    let vars = [
        "SPRINT_STATUS",
        "SPRINT_PRINTER",
        "SPRINT_FILE",
        "SPRINT_REASON",
    ];
    let vars = &vars[..if reason { 4 } else { 3 }];
    let refer = |v: &str| {
        if cfg!(windows) {
            format!("%{v}%")
        } else {
            format!("${v}")
        }
    };
    let mut line = format!("echo {tag}");
    for (i, v) in vars.iter().enumerate() {
        line.push(' ');
        if i == 1 {
            line.push_str(&format!("[{}]", refer(v)));
        } else {
            line.push_str(&refer(v));
        }
    }
    line
}

fn hooked_printer() -> Printer {
    let printer = connect(fast_sim(50.0), options());
    printer.set_profile(PrinterProfile {
        name: "Hook test".into(),
        on_start_command: echo("hook-start", false),
        on_finish_command: echo("hook-finish", false),
        on_fail_command: echo("hook-fail", true),
        on_cancel_command: echo("hook-cancel", false),
        ..PrinterProfile::default()
    });
    printer
}

/// Wait for a console line starting with `prefix` (hook output is indented
/// by two spaces).
fn hook_output(printer: &Printer, prefix: &str) -> String {
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        let found = console(printer)
            .into_iter()
            .find(|e| e.text.trim_start().starts_with(prefix));
        if let Some(e) = found {
            assert_eq!(e.kind, ConsoleKind::Info, "{e:?}");
            return e.text.trim().to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no {prefix}: {:?}",
            tail(printer, 20)
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn start_and_finish_hooks_run_with_the_job_in_their_environment() {
    let printer = hooked_printer();
    let file = Arc::new(GcodeFile::from_text("hooked.gcode", "G28\nG1 X10 F3000\n"));
    printer.start_print(file, PrintOptions::default()).unwrap();
    wait_for(&printer, "completed", |s| !s.job_active());
    assert_eq!(
        hook_output(&printer, "hook-start"),
        "hook-start started [Hook test] hooked.gcode"
    );
    assert_eq!(
        hook_output(&printer, "hook-finish"),
        "hook-finish completed [Hook test] hooked.gcode"
    );
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.text.starts_with("The finish hook ran"))
    );
}

#[test]
fn fail_and_cancel_hooks() {
    let printer = hooked_printer();
    let mut text = String::from("G28\n");
    text.push_str(&format!("M117 {}\n", "x".repeat(600)));
    printer
        .start_print(
            Arc::new(GcodeFile::from_text("failing.gcode", text)),
            PrintOptions::default(),
        )
        .unwrap();
    wait_for(&printer, "failed", |s| !s.job_active());
    let line = hook_output(&printer, "hook-fail");
    assert!(
        line.starts_with("hook-fail failed [Hook test] failing.gcode Line"),
        "{line}"
    );

    let mut text = String::from("G28\nG4 S30\n");
    for i in 0..10 {
        text.push_str(&format!("G1 X{i} F3000\n"));
    }
    printer
        .start_print(
            Arc::new(GcodeFile::from_text("cancelled.gcode", text)),
            PrintOptions::default(),
        )
        .unwrap();
    printer.cancel();
    wait_for(&printer, "cancelled", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Cancelled)
    });
    assert_eq!(
        hook_output(&printer, "hook-cancel"),
        "hook-cancel cancelled [Hook test] cancelled.gcode"
    );
}
