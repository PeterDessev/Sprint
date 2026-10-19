//! SD card: listing, uploads (numbered lines, also in unnumbered mode),
//! printing from the card with progress, pause/resume and cancel.

mod common;

use std::sync::Arc;

use common::*;
use sprint_host::sprint_gcode::GcodeFile;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{ChecksumMode, ConnectOptions, ConsoleKind, Event, JobKind, JobStatus};

fn small_file() -> Arc<GcodeFile> {
    let mut text = String::from("; uploaded\nG28\nG90\nM28 nested.gco\n");
    for i in 0..120 {
        text.push_str(&format!(
            "G1 X{} Y{} F3000 ; move {i}\n",
            10 + i % 50,
            20 + i % 30
        ));
    }
    text.push_str("M29\nM84\n");
    Arc::new(GcodeFile::from_text("upload.gcode", text))
}

#[test]
fn listing_reports_the_card_contents() {
    let printer = connect(fast_sim(50.0), options());
    let events = printer.subscribe();
    printer.request_sd_files();
    wait_for(&printer, "the listing", |s| {
        !s.sd.files.is_empty() && !s.sd.listing
    });
    let sd = printer.with_state(|s| s.sd.clone());
    assert_eq!(sd.card_present, Some(true));
    let long: Vec<_> = sd
        .files
        .iter()
        .filter_map(|f| f.long_name.clone())
        .collect();
    assert!(long.iter().any(|n| n == "calibration_cube.gcode"), "{sd:?}");
    assert!(long.iter().any(|n| n == "first layer test.gcode"), "{sd:?}");
    assert!(sd.files.iter().all(|f| f.size.is_some()));
    // (The event follows the state change: wait for it rather than look.)
    wait_for_event(&events, "the listing", |e| {
        matches!(e, Event::SdFiles(f) if f.len() == sd.files.len()).then_some(())
    });
    // EXTENDED_M20 is advertised, so long names were requested.
    let sent: Vec<String> = console(&printer)
        .into_iter()
        .filter(|e| e.kind == ConsoleKind::Sent)
        .map(|e| e.text)
        .collect();
    assert!(sent.iter().any(|c| c == "M20 L"), "{sent:?}");
}

fn upload(mode: ChecksumMode) {
    let mut sim = fast_sim(50.0);
    sim.checksum_error_rate = 0.05;
    sim.seed = 77;
    let printer = connect(
        sim,
        ConnectOptions {
            checksum: mode,
            ..options()
        },
    );
    let control = printer.sim_control().unwrap();
    let file = small_file();
    control.clear_received();
    let before = last_seq(&printer);
    printer
        .upload_to_sd(Arc::clone(&file), "upload.gco")
        .unwrap();
    wait_for(&printer, "the upload to finish", |s| !s.job_active());
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    assert_eq!(job.kind, JobKind::SdUpload);
    assert_eq!(job.status, JobStatus::Completed, "{:?}", tail(&printer, 20));
    // The file's G28 was stored on the card, not run.
    assert_eq!(printer.with_state(|s| s.homed), None);
    // The file's own M28/M29 are not sent; everything else is, in order.
    let mut want = vec!["M28 upload.gco".to_string()];
    want.extend(
        (0..file.len())
            .filter_map(|i| file.command(i).map(|c| c.into_owned()))
            .filter(|c| !c.starts_with("M28") && !c.starts_with("M29")),
    );
    want.push("M29".into());
    let got = control.received_commands();
    assert_eq!(&got[..want.len()], want.as_slice());
    // Every line of the upload was numbered, whatever the mode.
    let sent: Vec<_> = printer
        .console_since(before, usize::MAX)
        .into_iter()
        .filter(|e| e.kind == ConsoleKind::Sent && e.text.starts_with("G1"))
        .collect();
    assert!(sent.iter().all(|e| e.line.is_some()));
    // The listing is refreshed and shows the new file.
    wait_for(&printer, "the new file in the listing", |s| {
        s.sd.files
            .iter()
            .any(|f| f.long_name.as_deref() == Some("upload.gco") || f.name == "UPLOAD.GCO")
    });
    wait_idle(&printer);
    // It prints from the card.
    printer.start_sd_print("upload.gco").unwrap();
    wait_for(&printer, "the SD print", |s| !s.job_active());
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Completed)
    );
}

#[test]
fn upload_with_numbered_lines() {
    upload(ChecksumMode::Always);
}

#[test]
fn upload_is_numbered_even_without_checksums() {
    upload(ChecksumMode::Never);
}

#[test]
fn upload_can_be_cancelled_and_deletes_the_partial_file() {
    let printer = connect(fast_sim(50.0), options());
    let control = printer.sim_control().unwrap();
    // Large enough that the cancel always arrives mid-upload, even on a
    // loaded machine (3000 lines could be done before it).
    let mut text = String::new();
    for i in 0..60_000 {
        text.push_str(&format!("G1 X{} F3000\n", i % 100));
    }
    printer
        .upload_to_sd(Arc::new(GcodeFile::from_text("big", text)), "big.gco")
        .unwrap();
    wait_for(&printer, "the upload to run", |s| {
        s.job.as_ref().is_some_and(|j| j.current_line > 100)
    });
    printer.cancel();
    wait_for(&printer, "cancelled", |s| !s.job_active());
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Cancelled)
    );
    wait_idle(&printer);
    let got = control.received_commands();
    let m29 = got.iter().rposition(|c| c == "M29").unwrap();
    assert_eq!(got.get(m29 + 1).map(String::as_str), Some("M30 big.gco"));
}

#[test]
fn sd_print_reports_progress_and_completes() {
    let sim = SimConfig {
        sd_files: vec![("part.gcode".into(), 40_000)],
        instant_moves: false,
        ..fast_sim(200.0)
    };
    let printer = connect(sim, options());
    let events = printer.subscribe();
    printer.start_sd_print("part.gcode").unwrap();
    wait_for(&printer, "SD progress", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.bytes_done > 0 && j.progress > 0.0)
    });
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    assert_eq!(job.kind, JobKind::SdPrint);
    assert_eq!(job.status, JobStatus::Printing);
    assert_eq!(job.bytes_total, 40_000);
    assert!(printer.with_state(|s| s.sd.printing.is_some()));
    wait_for(&printer, "the SD print to complete", |s| !s.job_active());
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.progress, 1.0);
    // The card is not printing any more.
    assert_eq!(printer.with_state(|s| s.sd.printing), None);
    let sent: Vec<String> = console(&printer)
        .into_iter()
        .filter(|e| e.kind == ConsoleKind::Sent)
        .map(|e| e.text)
        .collect();
    for cmd in ["M23 part.gcode", "M24", "M27 S2"] {
        assert!(sent.iter().any(|c| c == cmd), "{cmd}: {sent:?}");
    }
    // Auto-reports are switched off again.
    wait_for(&printer, "M27 S0", |_| {
        console(&printer)
            .iter()
            .any(|e| e.kind == ConsoleKind::Sent && e.text == "M27 S0")
    });
    let statuses: Vec<JobStatus> = events
        .try_iter()
        .filter_map(|e| match e {
            Event::Job(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(statuses.first(), Some(&JobStatus::Starting));
    assert_eq!(statuses.last(), Some(&JobStatus::Completed));
}

#[test]
fn sd_print_pause_resume_and_cancel() {
    let sim = SimConfig {
        sd_files: vec![("long.gcode".into(), 400_000)],
        instant_moves: false,
        ..fast_sim(100.0)
    };
    let printer = connect(sim, options());
    let control = printer.sim_control().unwrap();
    printer.start_sd_print("long.gcode").unwrap();
    wait_for(&printer, "printing", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    wait_for(&printer, "the card to stop", |_| {
        !control.snapshot().sd_printing
    });
    printer.resume();
    wait_for(&printer, "printing again", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
    wait_for(&printer, "the card to run", |_| {
        control.snapshot().sd_printing
    });
    printer.cancel();
    wait_for(&printer, "cancelled", |s| !s.job_active());
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Cancelled)
    );
    // The last progress report does not outlive the cancelled print.
    assert_eq!(printer.with_state(|s| s.sd.printing), None);
    let got = control.received_commands();
    for cmd in ["M23 long.gcode", "M24", "M25", "M108", "M524"] {
        assert!(got.iter().any(|c| c == cmd), "{cmd}: {got:?}");
    }
    wait_for(&printer, "the print to stop", |_| {
        !control.snapshot().sd_printing
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(printer.with_state(|s| s.sd.printing), None);
}

#[test]
fn a_print_started_on_the_printer_is_followed_until_it_ends() {
    let sim = SimConfig {
        sd_files: vec![("menu.gcode".into(), 30_000)],
        instant_moves: false,
        ..fast_sim(200.0)
    };
    let printer = connect(sim, options());
    let control = printer.sim_control().unwrap();
    // Started on the printer's own menu: the host knows nothing yet.
    control.run_command("M23 menu.gcode\nM24");
    wait_for(&printer, "the card to print", |_| {
        control.snapshot().sd_printing
    });
    assert_eq!(printer.with_state(|s| s.sd.printing), None);
    // One report (asked by the user) and the host follows the print.
    printer.send("M27");
    wait_for(&printer, "the progress", |s| s.sd.printing.is_some());
    let first = printer.with_state(|s| s.sd.printing).unwrap();
    assert_eq!(first.1, 30_000);
    wait_for(&printer, "later progress", |s| {
        s.sd.printing.is_some_and(|(done, _)| done > first.0)
    });
    assert!(!printer.with_state(|s| s.job_active()), "not Sprint's job");
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.kind == ConsoleKind::Sent && e.text == "M27" && e.auto),
        "followed with automatic M27 polls"
    );
    // "Done printing file": the card prints no more.
    wait_for(&printer, "the end of the print", |s| {
        s.sd.printing.is_none()
    });
    assert!(!control.snapshot().sd_printing);
    wait_idle(&printer);
    let polls = || {
        console(&printer)
            .iter()
            .filter(|e| e.kind == ConsoleKind::Sent && e.text == "M27")
            .count()
    };
    let before = polls();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(polls(), before, "polling stops with the print");
}

#[test]
fn missing_sd_file_fails_the_job() {
    let printer = connect(fast_sim(50.0), options());
    printer.start_sd_print("nope.gcode").unwrap();
    wait_for(&printer, "failure", |s| !s.job_active());
    let status = printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone()));
    assert!(
        matches!(&status, Some(JobStatus::Failed(m)) if m.contains("open failed")),
        "{status:?}"
    );
    assert!(matches!(
        printer.start_sd_print("bad\nname"),
        Err(sprint_host::HostError::InvalidArgument(_))
    ));
}

#[test]
fn an_sd_print_without_a_card_fails_instead_of_hanging() {
    let printer = connect(fast_sim(50.0), options());
    printer.send("M22"); // release the card: "No media" from now on
    wait_idle(&printer);
    printer.start_sd_print("calibration_cube.gcode").unwrap();
    wait_for_within(
        &printer,
        "the job to fail",
        std::time::Duration::from_secs(5),
        |s| !s.job_active(),
    );
    let s = printer.snapshot();
    assert!(
        matches!(s.job.as_ref().map(|j| &j.status), Some(JobStatus::Failed(m)) if m.contains("SD card")),
        "{:?}",
        s.job
    );
    assert_eq!(s.sd.card_present, Some(false));
}

#[test]
fn deleting_a_file_updates_the_listing() {
    let printer = connect(fast_sim(50.0), options());
    printer.request_sd_files();
    wait_for(&printer, "the listing", |s| s.sd.files.len() == 3);
    printer.delete_sd_file("first layer test.gcode");
    wait_for(&printer, "the file to disappear", |s| s.sd.files.len() == 2);
    assert!(printer.with_state(|s| {
        s.sd.files
            .iter()
            .all(|f| f.long_name.as_deref() != Some("first layer test.gcode"))
    }));
}

#[test]
fn a_halt_or_a_disconnect_forgets_the_card_progress() {
    for end in ["halt", "disconnect"] {
        let sim = SimConfig {
            sd_files: vec![("menu.gcode".into(), 300_000)],
            instant_moves: false,
            ..fast_sim(200.0)
        };
        let printer = connect(sim, options());
        let control = printer.sim_control().unwrap();
        control.run_command("M23 menu.gcode");
        control.run_command("M24");
        printer.send("M27");
        wait_for(&printer, "the progress", |s| s.sd.printing.is_some());
        if end == "halt" {
            printer.emergency_stop();
            wait_for(&printer, "halted", |s| s.halted);
        } else {
            printer.disconnect();
        }
        assert_eq!(printer.with_state(|s| s.sd.printing), None, "{end}");
    }
}
