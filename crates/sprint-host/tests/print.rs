//! Streaming prints end to end against the simulator: every command
//! arrives exactly once and in order, with corrupted lines, lost oks and
//! several commands in flight.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use sprint_host::sprint_gcode::GcodeFile;
use sprint_host::{ConnectOptions, JobStatus, PrintOptions, Printer};

fn cube() -> Arc<GcodeFile> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/calibration-cube.gcode"
    );
    Arc::new(GcodeFile::load(path).expect("sample file"))
}

/// The commands of `file` as the simulator records them.
fn expected_commands(file: &GcodeFile) -> Vec<String> {
    (0..file.len())
        .filter_map(|i| file.command(i).map(|c| c.into_owned()))
        .collect()
}

/// Print `file`, wait for completion and return what the simulator
/// executed during the job.
fn print(
    printer: &Printer,
    file: Arc<GcodeFile>,
    options: PrintOptions,
) -> (Vec<String>, Duration) {
    let control = printer.sim_control().expect("simulator");
    control.clear_received();
    let start = Instant::now();
    printer.start_print(file, options).expect("start print");
    wait_for(printer, "the print to complete", |s| {
        s.job.as_ref().is_some_and(|j| !j.status.is_active())
    });
    let elapsed = start.elapsed();
    let status = printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone()));
    assert_eq!(status, Some(JobStatus::Completed));
    (control.received_commands(), elapsed)
}

fn assert_same_commands(got: &[String], want: &[String]) {
    let got: Vec<&String> = got.iter().filter(|c| c.as_str() != "M105").collect();
    if got.len() != want.len() || got.iter().zip(want).any(|(a, b)| *a != b) {
        let first = got
            .iter()
            .zip(want)
            .position(|(a, b)| *a != b)
            .unwrap_or(got.len().min(want.len()));
        panic!(
            "commands differ at {first}: got {} commands, want {}\n got: {:?}\nwant: {:?}",
            got.len(),
            want.len(),
            &got[first.saturating_sub(2)..(first + 3).min(got.len())],
            &want[first.saturating_sub(2)..(first + 3).min(want.len())]
        );
    }
}

fn corrupted(max_in_flight: usize) {
    let mut sim = fast_sim(200.0);
    sim.checksum_error_rate = 0.1;
    sim.seed = 1234 + max_in_flight as u64;
    let printer = connect(
        sim,
        ConnectOptions {
            max_in_flight,
            ..options()
        },
    );
    let file = cube();
    let want = expected_commands(&file);
    let (got, elapsed) = print(&printer, Arc::clone(&file), PrintOptions::default());
    assert_same_commands(&got, &want);
    let s = printer.snapshot();
    assert!(s.link.resends > 100, "{:?}", s.link);
    let job = s.job.unwrap();
    assert_eq!(job.current_line, job.total_lines);
    assert_eq!(job.progress, 1.0);
    eprintln!(
        "max_in_flight {max_in_flight}: {} commands in {elapsed:?}, {:?}",
        want.len(),
        s.link
    );
}

#[test]
fn calibration_cube_with_corruption_ping_pong() {
    corrupted(1);
}

#[test]
fn calibration_cube_with_corruption_four_in_flight() {
    corrupted(4);
}
