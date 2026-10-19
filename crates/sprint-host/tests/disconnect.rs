//! Disconnecting safely: a host print or an upload is cancelled first, an
//! SD print goes on on the printer.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use sprint_host::sprint_gcode::GcodeFile;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    ConnectionStatus, ConsoleKind, DisconnectSummary, JobKind, JobStatus, PrintOptions, Printer,
};

/// A host print with enough moves to be under way for a while.
fn long_file() -> Arc<GcodeFile> {
    let mut text = String::from("G28\nM302 P1\nM104 S180\nM140 S50\nM83\nG1 Z0.3 F600\n");
    for i in 0..2000 {
        text.push_str(&format!(
            "G1 X{} Y{} E0.5 F3000\n",
            20 + i % 100,
            20 + (i / 100) * 5
        ));
    }
    Arc::new(GcodeFile::from_text("long.gcode", text))
}

fn sent(printer: &Printer) -> Vec<String> {
    console(printer)
        .into_iter()
        .filter(|e| e.kind == ConsoleKind::Sent)
        .map(|e| e.text)
        .collect()
}

fn printing(printer: &Printer) {
    wait_for(printer, "the job to run", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.current_line > 20)
    });
}

#[test]
fn a_host_print_is_cancelled_before_the_link_closes() {
    let sim = SimConfig {
        instant_moves: false,
        ..fast_sim(50.0)
    };
    let printer = connect(sim, options());
    let control = printer.sim_control().unwrap();
    printer
        .start_print(long_file(), PrintOptions::default())
        .unwrap();
    printing(&printer);
    let summary = printer.disconnect_safely(Duration::from_secs(20));
    assert_eq!(
        summary,
        DisconnectSummary {
            was_connected: true,
            cancelled: Some((JobKind::HostPrint, "long.gcode".into())),
            cancel_status: Some(JobStatus::Cancelled),
            sd_print_continues: None,
        }
    );
    assert!(summary.acknowledged());
    assert_eq!(
        summary.to_string(),
        "Cancelled long.gcode (the cancel script ran), then disconnected"
    );
    assert_eq!(
        printer.with_state(|s| s.status.clone()),
        ConnectionStatus::Disconnected
    );
    // The job record says it was cancelled, not lost to the disconnect.
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Cancelled)
    );
    // The printer ran the cancel script (heaters off, then the rest).
    let got = control.received_commands();
    for cmd in ["M104 S0", "M140 S0", "M107", "M84"] {
        assert!(got.iter().any(|c| c == cmd), "{cmd}: {got:?}");
    }
}

#[test]
fn a_cancel_that_does_not_finish_in_time_still_switches_the_heaters_off() {
    // A dwell the cancel cannot end holds the cancel script back.
    let printer = connect(fast_sim(1.0), options());
    let file = Arc::new(GcodeFile::from_text(
        "dwell.gcode",
        "M104 S180\nG4 S600\nG1 X10 F3000\n",
    ));
    let control = printer.sim_control().unwrap();
    printer.start_print(file, PrintOptions::default()).unwrap();
    wait_for(&printer, "the dwell", |_| {
        control.received_commands().iter().any(|c| c == "G4 S600")
    });
    let started = Instant::now();
    let summary = printer.disconnect_safely(Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        summary.cancelled,
        Some((JobKind::HostPrint, "dwell.gcode".into()))
    );
    assert_eq!(summary.cancel_status, None);
    assert!(!summary.acknowledged());
    assert_eq!(
        summary.to_string(),
        "dwell.gcode did not stop in time: the heaters and the part fan were switched off as \
         the link closed"
    );
    // Written as the link closed, after the cancel script was queued.
    let out = sent(&printer);
    let tail = &out[out.len() - 3..];
    assert_eq!(tail, ["M104 S0", "M140 S0", "M107"], "{out:?}");
    assert_eq!(
        printer.with_state(|s| s.status.clone()),
        ConnectionStatus::Disconnected
    );
}

#[test]
fn an_upload_is_cancelled_and_its_file_deleted() {
    let printer = connect(fast_sim(50.0), options());
    let control = printer.sim_control().unwrap();
    let mut text = String::new();
    for i in 0..20_000 {
        text.push_str(&format!("G1 X{} Y{} E0.1\n", i % 200, i % 150));
    }
    let file = Arc::new(GcodeFile::from_text("big.gcode", text));
    printer.upload_to_sd(file, "big.gco").unwrap();
    wait_for(&printer, "the upload to run", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.current_line > 100)
    });
    let summary = printer.disconnect_safely(Duration::from_secs(20));
    assert_eq!(
        summary.cancelled,
        Some((JobKind::SdUpload, "big.gco".into()))
    );
    assert_eq!(summary.cancel_status, Some(JobStatus::Cancelled));
    assert_eq!(
        summary.to_string(),
        "Cancelled the upload of big.gco (the partial file was deleted), then disconnected"
    );
    let got = control.received_commands();
    let m29 = got.iter().rposition(|c| c == "M29").expect("M29");
    assert_eq!(got.get(m29 + 1).map(String::as_str), Some("M30 big.gco"));
}

#[test]
fn an_sd_print_goes_on_on_the_printer() {
    let sim = SimConfig {
        sd_files: vec![("part.gcode".into(), 400_000)],
        instant_moves: false,
        ..fast_sim(50.0)
    };
    let printer = connect(sim, options());
    let control = printer.sim_control().unwrap();
    printer.start_sd_print("part.gcode").unwrap();
    wait_for(&printer, "printing", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
    let started = Instant::now();
    let summary = printer.disconnect_safely(Duration::from_secs(20));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "nothing to wait for"
    );
    assert_eq!(
        summary,
        DisconnectSummary {
            was_connected: true,
            cancelled: None,
            cancel_status: None,
            sd_print_continues: Some("part.gcode".into()),
        }
    );
    assert_eq!(
        summary.to_string(),
        "Disconnected; the printer goes on printing part.gcode from its SD card, unwatched"
    );
    let got = control.received_commands();
    assert!(
        !got.iter().any(|c| c == "M524" || c == "M25"),
        "not cancelled: {got:?}"
    );
    // The record says why Sprint no longer follows it.
    let status = printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone()));
    assert!(
        matches!(&status, Some(JobStatus::Failed(m)) if m.contains("goes on printing part.gcode")),
        "{status:?}"
    );
}

#[test]
fn a_job_that_ends_before_the_cancel_is_not_reported_as_stopped_in_time() {
    // The job finished between the look at the state and the cancel: no
    // cancel script ran, no heaters were switched off at the close.
    let summary = DisconnectSummary {
        was_connected: true,
        cancelled: Some((JobKind::HostPrint, "cube.gcode".into())),
        cancel_status: Some(JobStatus::Completed),
        sd_print_continues: None,
    };
    assert!(!summary.acknowledged());
    assert_eq!(
        summary.to_string(),
        "cube.gcode finished before it was cancelled, then disconnected"
    );
    let upload = DisconnectSummary {
        cancelled: Some((JobKind::SdUpload, "cube.gco".into())),
        ..summary
    };
    assert_eq!(
        upload.to_string(),
        "The upload of cube.gco finished before it was cancelled, then disconnected"
    );
}

#[test]
fn without_a_job_or_a_connection_it_just_disconnects() {
    let idle = Printer::new();
    let summary = idle.disconnect_safely(Duration::from_secs(1));
    assert_eq!(summary, DisconnectSummary::default());
    assert_eq!(summary.to_string(), "Not connected");

    let printer = connect(fast_sim(50.0), options());
    let summary = printer.disconnect_safely(Duration::from_secs(1));
    assert_eq!(
        summary,
        DisconnectSummary {
            was_connected: true,
            ..DisconnectSummary::default()
        }
    );
    assert_eq!(summary.to_string(), "Disconnected");
    assert_eq!(
        printer.with_state(|s| s.status.clone()),
        ConnectionStatus::Disconnected
    );
}
