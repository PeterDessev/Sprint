//! Pause, resume and cancel of host prints: the scripts run in the right
//! places, firmware-initiated pauses, and the M600 runout prompt.

mod common;

use std::sync::Arc;

use common::*;
use sprint_host::sprint_gcode::{GcodeFile, state_at_line};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    Axis, Event, HostPrompt, JobStatus, PrintOptions, Printer, PrinterProfile, render_script,
};

/// A file whose moves take (scaled) time, so jobs can be paused midway.
fn file() -> Arc<GcodeFile> {
    let mut text = String::from("G28\nM302 P1\nG90\nM83\nG1 Z0.3 F600\n");
    for i in 0..400 {
        let x = 20.0 + (i % 20) as f64 * 9.0;
        let y = 20.0 + (i / 20) as f64 * 9.0;
        text.push_str(&format!("G1 X{x} Y{y} E0.5 F3000\n"));
        if i % 50 == 0 {
            text.push_str("; a comment\n");
        }
    }
    text.push_str("M84\n");
    Arc::new(GcodeFile::from_text("pause-test.gcode", text))
}

fn commands(file: &GcodeFile) -> Vec<String> {
    (0..file.len())
        .filter_map(|i| file.command(i).map(|c| c.into_owned()))
        .collect()
}

fn sim() -> SimConfig {
    SimConfig {
        instant_moves: false,
        ..fast_sim(60.0)
    }
}

/// Start the job and wait until it is well under way.
fn start(printer: &Printer, file: &Arc<GcodeFile>, options: PrintOptions) {
    printer.sim_control().unwrap().clear_received();
    printer.start_print(Arc::clone(file), options).unwrap();
    wait_for(printer, "the job to run", |s| {
        s.job.as_ref().is_some_and(|j| j.current_line > 40)
    });
}

fn job_status(printer: &Printer) -> Option<JobStatus> {
    printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone()))
}

/// File line index (+1) after the `count`-th command.
fn line_after_commands(file: &GcodeFile, count: usize) -> usize {
    let mut seen = 0;
    for i in 0..file.len() {
        if file.command(i).is_some() {
            seen += 1;
            if seen == count {
                return i + 1;
            }
        }
    }
    file.len()
}

#[test]
fn pause_and_resume_run_the_scripts_between_stream_lines() {
    let printer = connect(sim(), options());
    let file = file();
    let want = commands(&file);
    let events = printer.subscribe();
    start(&printer, &file, PrintOptions::default());

    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    // While paused the user may move the head.
    printer.jog(Axis::X, 5.0, 3000.0);
    wait_idle(&printer);
    let paused_at = printer.with_state(|s| s.job.as_ref().unwrap().current_line);
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });

    let got = printer.sim_control().unwrap().received_commands();
    let p = got
        .iter()
        .position(|c| c == "M400")
        .expect("pause script ran");
    // Everything before the pause script is the stream, in order.
    assert_eq!(&got[..p], &want[..p]);
    assert_eq!(
        line_after_commands(&file, p),
        paused_at,
        "paused after line {paused_at}"
    );
    let pause = [
        "M400",
        "G91",
        "G1 E-2 F2700",
        "G1 Z10 F600",
        "G90",
        "G1 X10 Y210 F6000",
    ];
    assert_eq!(&got[p..p + pause.len()], pause);
    let after_pause = p + pause.len();
    assert_eq!(
        &got[after_pause..after_pause + 3],
        ["G91", "G1 X5 F3000", "G90"]
    );
    // The resume script restores the position of the last streamed line;
    // the file extrudes relatively, so `G92 E{e}` is left out and M83 is
    // restored after the script's G90.
    let state = state_at_line(&file, paused_at);
    let mut resume = render_script(sprint_host::script::DEFAULT_RESUME_SCRIPT, &state);
    assert!(!resume.iter().any(|l| l.starts_with("G92")));
    resume.push("M83".to_string());
    let r = after_pause + 3;
    assert_eq!(&got[r..r + resume.len()], resume.as_slice());
    assert!(resume[0].starts_with(&format!("G1 X{} Y{}", state.position[0], state.position[1])));
    // Then the rest of the stream, nothing lost or repeated.
    assert_eq!(&got[r + resume.len()..], &want[p..]);

    let statuses: Vec<JobStatus> = events
        .try_iter()
        .filter_map(|e| match e {
            Event::Job(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        [
            JobStatus::Starting,
            JobStatus::Printing,
            JobStatus::Pausing,
            JobStatus::Paused,
            JobStatus::Resuming,
            JobStatus::Printing,
            JobStatus::Completed
        ]
    );
}

#[test]
fn scripts_run_in_millimetres_when_the_file_uses_inches() {
    // The scripts' "G1 Z10" and the resume position (in mm) would be read
    // as inches while the stream is in G20.
    let printer = connect(sim(), options());
    let mut text = String::from("G28\nM302 P1\nG20\nG90\nM83\nG1 Z0.02 F20\n");
    for i in 0..300 {
        let x = 1.0 + (i % 20) as f64 * 0.3;
        let y = 1.0 + (i / 20) as f64 * 0.3;
        text.push_str(&format!("G1 X{x:.2} Y{y:.2} E0.02 F100\n"));
    }
    let file = Arc::new(GcodeFile::from_text("inches.gcode", text));
    let want = commands(&file);
    start(&printer, &file, PrintOptions::default());
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    let paused_at = printer.with_state(|s| s.job.as_ref().unwrap().current_line);
    printer.resume();
    wait_for(&printer, "completed", |s| !s.job_active());
    let got = printer.sim_control().unwrap().received_commands();
    let p = got
        .iter()
        .position(|c| c == "M400")
        .expect("pause script ran");
    assert_eq!(got[p - 1], "G21", "the pause script switches to mm first");
    assert_eq!(&got[..p - 1], &want[..p - 1]);
    let state = state_at_line(&file, paused_at);
    let pause_len = 6;
    let r = p + pause_len;
    assert_eq!(got[r], "G21", "{:?}", &got[p..r + 3]);
    let x = state.position[0];
    assert!(x > 20.0, "positions are in mm: {x}");
    assert!(got[r + 1].starts_with(&format!("G1 X{}", sprint_host::format_number(x, 3))));
    // The resume script ends by restoring the stream's modes.
    let end = got[r..].iter().position(|c| c == "G20").expect("G20 again") + r;
    assert_eq!(got[end - 1], "M83");
    assert_eq!(&got[end + 1..], &want[p - 1..]);
}

#[test]
fn elapsed_time_excludes_the_pause() {
    let printer = connect(sim(), options());
    let file = file();
    start(&printer, &file, PrintOptions::default());
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    let e1 = printer.with_state(|s| s.job.as_ref().unwrap().elapsed);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let e2 = printer.with_state(|s| s.job.as_ref().unwrap().elapsed);
    assert_eq!(e1, e2, "the clock stops while paused");
    printer.cancel();
    wait_for(&printer, "cancelled", |s| !s.job_active());
}

#[test]
fn cancel_drops_the_stream_and_runs_the_cancel_script() {
    let printer = connect(sim(), options());
    let file = file();
    let want = commands(&file);
    start(&printer, &file, PrintOptions::default());
    printer.cancel();
    wait_for(&printer, "cancelled", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Cancelled)
    });
    wait_idle(&printer);
    let got = printer.sim_control().unwrap().received_commands();
    let cancel = [
        "M104 S0",
        "M140 S0",
        "M107",
        "G91",
        "G1 Z10 F600",
        "G90",
        "G28 X Y",
        "M84",
    ];
    let c = got.len() - cancel.len();
    assert_eq!(&got[c..], cancel);
    assert_eq!(&got[..c], &want[..c], "the stream up to the cancel");
    assert!(c < want.len() - 100, "the rest of the stream was dropped");
}

#[test]
fn cancel_while_paused_and_custom_scripts() {
    let printer = connect(sim(), options());
    printer.set_profile(PrinterProfile {
        bed_size: [300.0, 300.0],
        ..PrinterProfile::default()
    });
    let file = file();
    start(
        &printer,
        &file,
        PrintOptions {
            cancel_script: Some("M117 Bye {tool}\nM104 S0 ; heaters off".into()),
            ..PrintOptions::default()
        },
    );
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    printer.cancel();
    wait_for(&printer, "cancelled", |s| !s.job_active());
    wait_idle(&printer);
    let got = printer.sim_control().unwrap().received_commands();
    // The profile's bed size moves the parking position.
    assert!(got.iter().any(|c| c == "G1 X10 Y290 F6000"), "{got:?}");
    assert_eq!(&got[got.len() - 2..], ["M117 Bye 0", "M104 S0"]);
    assert_eq!(job_status(&printer), Some(JobStatus::Cancelled));
}

#[test]
fn firmware_pause_and_resume_need_no_scripts() {
    let printer = connect(sim(), options());
    let file = file();
    let want = commands(&file);
    let control = printer.sim_control().unwrap();
    start(&printer, &file, PrintOptions::default());
    control.inject_line("//action:pause");
    wait_for(&printer, "paused by the printer", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    wait_idle(&printer);
    let received = control.received_commands().len();
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(
        control.received_commands().len(),
        received,
        "streaming stopped"
    );
    control.inject_line("//action:resume");
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    assert_eq!(
        control.received_commands(),
        want,
        "no script lines were sent"
    );
}

#[test]
fn firmware_cancel_runs_the_cancel_script() {
    let printer = connect(sim(), options());
    let file = file();
    let control = printer.sim_control().unwrap();
    start(&printer, &file, PrintOptions::default());
    control.inject_line("//action:cancel");
    wait_for(&printer, "cancelled", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Cancelled)
    });
    wait_idle(&printer);
    assert_eq!(
        control.received_commands().last().map(String::as_str),
        Some("M84")
    );
}

#[test]
fn filament_runout_prompt_and_answer() {
    let printer = connect(sim(), options());
    let file = file();
    let want = commands(&file);
    let control = printer.sim_control().unwrap();
    let events = printer.subscribe();
    start(&printer, &file, PrintOptions::default());
    control.trigger_filament_runout();
    wait_for(&printer, "the runout prompt", |s| s.prompt.is_some());
    let prompt = printer.with_state(|s| s.prompt.clone()).unwrap();
    assert_eq!(
        prompt,
        HostPrompt {
            message: "Nozzle Parked".into(),
            buttons: vec!["Continue".into()]
        }
    );
    assert_eq!(job_status(&printer), Some(JobStatus::Paused));
    wait_for(&printer, "busy: paused for user", |s| {
        s.busy
            .as_deref()
            .is_some_and(|b| b.contains("paused for user"))
    });
    assert!(printer.with_state(|s| s.firmware_messages.iter().any(|m| m.contains("runout"))));
    printer.answer_prompt(0);
    wait_for(&printer, "the prompt to close", |s| s.prompt.is_none());
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    let got = control.received_commands();
    assert!(got.iter().any(|c| c == "M876 S0"));
    let stream: Vec<String> = got.into_iter().filter(|c| c != "M876 S0").collect();
    assert_eq!(stream, want);
    let prompts: Vec<Option<HostPrompt>> = events
        .try_iter()
        .filter_map(|e| match e {
            Event::Prompt(p) => Some(p),
            _ => None,
        })
        .collect();
    assert!(prompts.first().is_some_and(Option::is_some));
    assert!(prompts.iter().any(Option::is_none));
    assert_eq!(printer.with_state(|s| s.link.timeouts), 0);
}

#[test]
fn resume_after_a_firmware_pause_releases_a_waiting_printer() {
    let printer = connect(sim(), options());
    let file = file();
    let control = printer.sim_control().unwrap();
    start(&printer, &file, PrintOptions::default());
    control.trigger_filament_runout();
    wait_for(&printer, "waiting for the user", |s| {
        s.busy
            .as_deref()
            .is_some_and(|b| b.contains("paused for user"))
    });
    // Resuming from the host sends M108, which releases the wait.
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
}
