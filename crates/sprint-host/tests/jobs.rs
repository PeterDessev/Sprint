//! Pause, resume and cancel of host prints: the scripts run in the right
//! places, firmware-initiated pauses, and the M600 runout prompt.

mod common;

use std::sync::Arc;

use common::*;
use sprint_host::sprint_gcode::{GcodeFile, state_at_line};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    Axes, Axis, Event, HostPrompt, JobStatus, PrintOptions, Printer, PrinterProfile, render_script,
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
    // the file extrudes relatively, so `G92 E{e}` is left out. The modes
    // the script is written for come first, the stream's modes after it.
    let state = state_at_line(&file, paused_at);
    let template = render_script(sprint_host::script::DEFAULT_RESUME_SCRIPT, &state);
    assert!(!template.iter().any(|l| l.starts_with("G92")));
    let mut resume = vec!["G90".to_string(), "M83".to_string()];
    resume.extend(template);
    resume.extend(["G90".to_string(), "M83".to_string()]);
    let r = after_pause + 3;
    assert_eq!(&got[r..r + resume.len()], resume.as_slice());
    assert!(resume[2].starts_with(&format!("G1 X{} Y{}", state.position[0], state.position[1])));
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
fn commands_typed_while_paused_run_before_the_resume_script() {
    // A purge typed while paused must not run after the head went back
    // onto the part: the resume script waits for the commands queued
    // before it.
    let printer = connect(sim(), options());
    let file = file();
    start(&printer, &file, PrintOptions::default());
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    wait_idle(&printer);
    let control = printer.sim_control().unwrap();
    control.clear_received();
    // The dwell keeps the rest of the purge queued when resume arrives.
    let purge = [
        "G4 S30",
        "M117 purge-start",
        "G91",
        "G1 E5 F300",
        "G90",
        "M117 purge-end",
    ];
    printer.send(&purge.join("\n"));
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    let got = control.received_commands();
    let head = &got[..got.len().min(20)];
    assert_eq!(&got[..purge.len()], purge, "{head:?}");
    let back = got
        .iter()
        .position(|c| c.starts_with("G1 X"))
        .expect("the resume move");
    assert!(back >= purge.len(), "{head:?}");
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
    assert_eq!(
        &got[r..r + 3],
        ["G21", "G90", "M83"],
        "{:?}",
        &got[p..r + 4]
    );
    let x = state.position[0];
    assert!(x > 20.0, "positions are in mm: {x}");
    assert!(got[r + 3].starts_with(&format!("G1 X{}", sprint_host::format_number(x, 3))));
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
fn cancel_switches_off_every_hotend_and_the_chamber() {
    // A bare M104 S0 only reaches the active tool: printing with T1 left
    // T0 (and the chamber) heating after a cancel. The profile says one
    // extruder; the printer reports two hotends and a chamber.
    let printer = connect(
        SimConfig {
            extruders: 2,
            heated_chamber: true,
            ..sim()
        },
        options(),
    );
    let mut text = String::from("M104 T0 S200\nM104 T1 S200\nM141 S40\nT1\nG28\nM302 P1\nM83\n");
    for i in 0..400 {
        text.push_str(&format!(
            "G1 X{} Y{} E0.5 F3000\n",
            20 + i % 100,
            20 + i / 4
        ));
    }
    let file = Arc::new(GcodeFile::from_text("two-tools.gcode", text));
    start(&printer, &file, PrintOptions::default());
    let control = printer.sim_control().unwrap();
    assert_eq!(control.snapshot().hotend_targets, [200.0, 200.0]);
    printer.cancel();
    wait_for(&printer, "cancelled", |s| !s.job_active());
    wait_idle(&printer);
    let snap = control.snapshot();
    assert_eq!(snap.hotend_targets, [0.0, 0.0]);
    assert_eq!(snap.bed_target, 0.0);
    let got = control.received_commands();
    let c = got.iter().position(|c| c == "M104 T0 S0").expect("T0 off");
    assert_eq!(
        &got[c..c + 5],
        ["M104 T0 S0", "M104 T1 S0", "M140 S0", "M141 S0", "M107"]
    );
}

#[test]
fn the_cool_down_macro_switches_off_every_hotend() {
    let printer = connect(
        SimConfig {
            extruders: 2,
            ..fast_sim(60.0)
        },
        options(),
    );
    printer.send("M104 T0 S180\nM104 T1 S190\nM140 S50");
    wait_idle(&printer);
    let control = printer.sim_control().unwrap();
    assert_eq!(control.snapshot().hotend_targets, [180.0, 190.0]);
    let cool = sprint_host::AppConfig::default()
        .macros
        .into_iter()
        .find(|m| m.name == "Cool down")
        .expect("the default macro");
    printer.run_macro(&cool);
    wait_idle(&printer);
    let snap = control.snapshot();
    assert_eq!(snap.hotend_targets, [0.0, 0.0]);
    assert_eq!(snap.bed_target, 0.0);
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

#[test]
fn a_pause_before_homing_does_not_park_or_lift() {
    // Start G-code that heats before G28: a pause during the heat-up must
    // not move axes that have no known position (no soft endstops).
    let printer = connect(sim(), options());
    let control = printer.sim_control().unwrap();
    let mut text =
        String::from("M140 S60\nM104 S200\nM190 S60\nM109 S200\nG28\nG1 Z0.3 F600\nM83\n");
    for i in 0..100 {
        text.push_str(&format!("G1 X{} Y20 E0.5 F3000\n", 20 + i));
    }
    let file = Arc::new(GcodeFile::from_text("heat-first.gcode", text));
    control.clear_received();
    printer
        .start_print(Arc::clone(&file), PrintOptions::default())
        .unwrap();
    wait_for(&printer, "the bed heat-up", |_| {
        control.received_commands().iter().any(|c| c == "M190 S60")
    });
    assert_eq!(printer.with_state(|s| s.homed), None);
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    let got = control.received_commands();
    assert!(
        !got.iter()
            .any(|c| c.starts_with("G1 Z") || c.starts_with("G1 X")),
        "{got:?}"
    );
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.text.starts_with("X Y Z not homed: the pause script")),
        "{:?}",
        tail(&printer, 20)
    );
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    assert_eq!(printer.with_state(|s| s.homed), Some(Axes::ALL));
}

#[test]
fn modes_typed_while_paused_do_not_move_the_rest_of_the_print() {
    // A G91 + Z lift without the closing G90, and a G20, typed while
    // paused: the resume moves and the rest of the layer must still go
    // where the file says.
    let printer = connect(sim(), options());
    let file = file();
    start(&printer, &file, PrintOptions::default());
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    printer.send("G91\nG1 Z5 F600\nG20");
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    wait_idle(&printer);
    let end = state_at_line(&file, file.len());
    let pos = printer.sim_control().unwrap().snapshot().position;
    for axis in 0..3 {
        assert!(
            (f64::from(pos[axis]) - end.position[axis]).abs() < 0.01,
            "axis {axis}: printer {pos:?}, file {:?}",
            end.position
        );
    }
}

#[test]
fn resume_heats_back_up_and_restores_the_fan() {
    // Turning the hotend off (and the fan) during a long pause is common:
    // resuming must not print with a cold nozzle.
    let printer = connect(sim(), options());
    let control = printer.sim_control().unwrap();
    let mut text = String::from(
        "M104 S200\nM140 S50\nM109 S200\nM190 S50\nG28\nM106 S200\nM83\nG1 Z0.3 F600\n",
    );
    for i in 0..300 {
        text.push_str(&format!(
            "G1 X{} Y{} E0.5 F3000\n",
            20 + i % 100,
            20 + i / 10
        ));
    }
    let file = Arc::new(GcodeFile::from_text("hot.gcode", text));
    start(&printer, &file, PrintOptions::default());
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    printer.set_temperature(sprint_host::Heater::Hotend(0), 0.0);
    printer.set_fan(0, 0);
    wait_for(&printer, "the hotend to cool down", |s| {
        s.temps.hotends.first().is_some_and(|h| h.current < 150.0)
    });
    control.clear_received();
    let seq = last_seq(&printer);
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    let got = control.received_commands();
    let heat = got
        .iter()
        .position(|c| c == "M109 S200")
        .expect("M109 S200");
    let back = got.iter().position(|c| c.starts_with("G1 X")).unwrap();
    assert!(
        heat < back,
        "heats before moving back: {:?}",
        &got[..back + 1]
    );
    assert!(got.iter().any(|c| c == "M104 S200"), "{got:?}");
    assert!(
        !got.iter().any(|c| c.starts_with("M190")),
        "the bed stayed hot"
    );
    let fan = got.iter().position(|c| c == "M106 S200").expect("the fan");
    let printing = got.iter().position(|c| c.ends_with("E0.5 F3000")).unwrap();
    assert!(fan < printing, "{:?}", &got[..printing + 1]);
    let cold = printer
        .console_since(seq, usize::MAX)
        .iter()
        .filter(|e| e.text.contains("cold extrusion"))
        .count();
    assert_eq!(cold, 0);
    assert_eq!(control.snapshot().hotend_targets, [200.0]);
}

#[test]
fn a_pause_during_a_heat_up_ends_it_and_the_resume_heats_again() {
    // Real-time heating: the M109 would hold the pause for a minute.
    let printer = connect(fast_sim(1.0), options());
    let control = printer.sim_control().unwrap();
    let mut text = String::from("M104 S200\nM109 S200\nG28\nM83\nG1 Z0.3 F600\n");
    for i in 0..50 {
        text.push_str(&format!("G1 X{} Y20 E0.5 F3000\n", 20 + i));
    }
    let file = Arc::new(GcodeFile::from_text("heat-up.gcode", text));
    control.clear_received();
    printer
        .start_print(Arc::clone(&file), PrintOptions::default())
        .unwrap();
    wait_for(&printer, "the heat-up", |_| {
        control.received_commands().iter().any(|c| c == "M109 S200")
    });
    let seq = last_seq(&printer);
    let asked = std::time::Instant::now();
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    assert!(asked.elapsed() < std::time::Duration::from_secs(5));
    let got = control.received_commands();
    assert!(got.iter().any(|c| c == "M108"), "{got:?}");
    assert!(
        !got.iter().any(|c| c.starts_with("G1 E")),
        "no cold retract: {got:?}"
    );

    control.clear_received();
    control.set_time_scale(100.0);
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    let got = control.received_commands();
    let heat = got
        .iter()
        .position(|c| c == "M109 S200")
        .expect("heats again");
    let home = got
        .iter()
        .position(|c| c == "G28")
        .expect("the rest of the file");
    assert!(heat < home, "{got:?}");
    assert!(
        !got.iter().any(|c| c == "G1 E2 F2700"),
        "no prime without a retract"
    );
    let cold = printer
        .console_since(seq, usize::MAX)
        .iter()
        .filter(|e| e.text.contains("cold extrusion"))
        .count();
    assert_eq!(cold, 0, "{:?}", tail(&printer, 30));
}

#[test]
fn a_pause_during_a_wait_for_the_user_ends_it() {
    let printer = connect(sim(), options());
    let control = printer.sim_control().unwrap();
    let mut text = String::from("G28\nM302 P1\nM83\nG1 Z0.3 F600\nM0 Insert the magnets\n");
    for i in 0..50 {
        text.push_str(&format!("G1 X{} Y20 E0.5 F3000\n", 20 + i));
    }
    let file = Arc::new(GcodeFile::from_text("magnets.gcode", text));
    control.clear_received();
    printer
        .start_print(Arc::clone(&file), PrintOptions::default())
        .unwrap();
    wait_for(&printer, "the M0", |_| {
        control
            .received_commands()
            .iter()
            .any(|c| c.starts_with("M0"))
    });
    printer.pause();
    // The M0 may still wait for the moves before it when the first M108
    // arrives: the pause repeats it while the printer waits.
    wait_for_within(&printer, "paused", std::time::Duration::from_secs(5), |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    assert!(control.received_commands().iter().any(|c| c == "M108"));
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
}

/// A short file with `marker` after the second move.
fn file_with(marker: &str) -> Arc<GcodeFile> {
    let mut text = format!("G28\nM302 P1\nM83\nG1 X10 F3000\nG1 X11 F3000\n{marker}\n");
    for i in 0..20 {
        text.push_str(&format!("G1 X{} Y20 E0.5 F3000\n", 20 + i));
    }
    Arc::new(GcodeFile::from_text("marked.gcode", text))
}

fn statuses(events: &crossbeam_channel::Receiver<Event>) -> Vec<JobStatus> {
    events
        .try_iter()
        .filter_map(|e| match e {
            Event::Job(s) => Some(s),
            _ => None,
        })
        .collect()
}

#[test]
fn host_pause_commands_in_the_file_pause_the_job() {
    for marker in [";@pause", "@pause"] {
        let printer = connect(sim(), options());
        let control = printer.sim_control().unwrap();
        let events = printer.subscribe();
        control.clear_received();
        printer
            .start_print(file_with(marker), PrintOptions::default())
            .unwrap();
        wait_for(&printer, "paused", |s| {
            s.job
                .as_ref()
                .is_some_and(|j| j.status == JobStatus::Paused)
        });
        let got = control.received_commands();
        let pause = got
            .iter()
            .position(|c| c == "M400")
            .expect("the pause script");
        assert_eq!(got[pause - 1], "G1 X11 F3000", "{marker}: {got:?}");
        printer.resume();
        wait_for(&printer, "completed", |s| {
            s.job
                .as_ref()
                .is_some_and(|j| j.status == JobStatus::Completed)
        });
        let got = control.received_commands();
        assert!(!got.iter().any(|c| c.contains('@')), "{marker}: {got:?}");
        assert_eq!(got.iter().filter(|c| c.ends_with("E0.5 F3000")).count(), 20);
        assert!(statuses(&events).contains(&JobStatus::Paused), "{marker}");
        assert!(
            !console(&printer)
                .iter()
                .any(|e| e.text.contains("Unknown command")),
            "{marker}"
        );
    }
}

#[test]
fn host_cancel_and_unknown_host_commands() {
    let printer = connect(sim(), options());
    let control = printer.sim_control().unwrap();
    control.clear_received();
    printer
        .start_print(file_with("@cancel"), PrintOptions::default())
        .unwrap();
    wait_for(&printer, "cancelled", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Cancelled)
    });
    let got = control.received_commands();
    assert!(got.iter().any(|c| c == "G28 X Y"), "the cancel script ran");
    assert!(!got.iter().any(|c| c.ends_with("E0.5 F3000")));

    control.clear_received();
    printer
        .start_print(file_with("@unheard_of"), PrintOptions::default())
        .unwrap();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    assert!(!control.received_commands().iter().any(|c| c.contains('@')));
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.text.contains("\"@unheard_of\" is not known"))
    );
}

#[test]
fn pause_points_pause_before_their_line() {
    let printer = connect(sim(), options());
    let file = file();
    let want = commands(&file);
    start(&printer, &file, PrintOptions::default());
    let at = printer.with_state(|s| s.job.as_ref().unwrap().current_line) + 100;
    printer.pause_at_lines(&[at, 3, at + 50]);
    wait_for(&printer, "the pause points", |s| {
        s.job.as_ref().is_some_and(|j| !j.pause_at.is_empty())
    });
    // Lines already streamed are left out.
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().unwrap().pause_at.clone()),
        [at, at + 50]
    );
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    assert_eq!(job.current_line, at, "every line before the point printed");
    assert_eq!(job.pause_at, [at + 50]);
    printer.pause_at_lines(&[]);
    printer.resume();
    wait_for(&printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    let got: Vec<String> = printer
        .sim_control()
        .unwrap()
        .received_commands()
        .into_iter()
        .filter(|c| want.contains(c))
        .collect();
    assert!(got.len() >= want.len(), "nothing was skipped");
}

#[test]
fn a_quick_stop_during_a_print_stops_the_print() {
    // M410 throws away the planned moves: streaming on would print with
    // segments missing.
    let printer = connect(sim(), options());
    let file = file();
    start(&printer, &file, PrintOptions::default());
    let control = printer.sim_control().unwrap();
    printer.send("M410");
    wait_for(&printer, "the job to stop", |s| !s.job_active());
    let status = job_status(&printer).unwrap();
    assert!(
        matches!(&status, JobStatus::Failed(m) if m.contains("M410")),
        "{status:?}"
    );
    wait_idle(&printer);
    let got = control.received_commands();
    let m410 = got.iter().position(|c| c == "M410").expect("M410");
    let streamed_after = got[m410..]
        .iter()
        .filter(|c| c.ends_with("E0.5 F3000"))
        .count();
    assert!(
        streamed_after <= 1,
        "the stream stopped: {:?}",
        &got[m410..]
    );
    assert!(
        got[m410..].iter().any(|c| c == "M114"),
        "position read back"
    );
    assert!(got[m410..].iter().any(|c| c == "M104 S0"), "heaters off");
}
