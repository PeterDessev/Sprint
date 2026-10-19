//! Streaming prints end to end against the simulator: every command
//! arrives exactly once and in order, with corrupted lines, lost oks and
//! several commands in flight; start lines, preambles, progress reports,
//! checksum modes, disconnects, emergency stops and board resets.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use sprint_host::sprint_gcode::GcodeFile;
use sprint_host::{
    ChecksumMode, ConnectOptions, ConnectionStatus, ConsoleKind, Event, HostError, JobStatus,
    PrintOptions, Printer,
};

fn cube() -> Arc<GcodeFile> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/calibration-cube.gcode"
    );
    Arc::new(GcodeFile::load(path).expect("sample file"))
}

/// A synthetic file with comments, blank lines and a mix of commands.
fn generated(moves: usize) -> Arc<GcodeFile> {
    let mut text =
        String::from("; generated test file\nG28\nM302 P1 ; allow cold extrusion\nG90\nM83\n\n");
    for i in 0..moves {
        let x = 10.0 + (i * 37 % 200) as f64 * 0.5;
        let y = 10.0 + (i * 53 % 200) as f64 * 0.5;
        match i % 25 {
            0 => text.push_str(&format!(";LAYER:{}\nM117 Layer {}\n", i / 25, i / 25)),
            7 => text.push_str(&format!("M106 S{}\n", i % 256)),
            13 => text.push_str("G92 E0 ; reset\n\n"),
            19 => text.push_str(&format!("G0 X{x:.2} Y{y:.2} F9000\n")),
            _ => {}
        }
        text.push_str(&format!(
            "G1 X{x:.3} Y{y:.3} E0.0{} F{}\n",
            i % 10,
            1200 + i % 7 * 300
        ));
    }
    text.push_str("M107\nM84\n; end\n");
    Arc::new(GcodeFile::from_text("generated.gcode", text))
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

/// `got` (without recovery M105s) equals `want` exactly.
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

fn corrupted(file: Arc<GcodeFile>, max_in_flight: usize, seed: u64) {
    let mut sim = fast_sim(200.0);
    sim.checksum_error_rate = 0.1;
    sim.seed = seed;
    let printer = connect(
        sim,
        ConnectOptions {
            max_in_flight,
            ..options()
        },
    );
    let want = expected_commands(&file);
    let (got, elapsed) = print(&printer, Arc::clone(&file), PrintOptions::default());
    assert_same_commands(&got, &want);
    let s = printer.snapshot();
    assert!(s.link.resends > 100, "{:?}", s.link);
    let job = s.job.unwrap();
    assert_eq!(job.current_line, job.total_lines);
    assert_eq!(job.bytes_done, job.bytes_total);
    assert_eq!(job.progress, 1.0);
    eprintln!(
        "{} with max_in_flight {max_in_flight}: {} commands in {elapsed:?}, {:?}",
        file.name(),
        want.len(),
        s.link
    );
}

#[test]
fn calibration_cube_with_corruption_ping_pong() {
    corrupted(cube(), 1, 1235);
}

#[test]
fn calibration_cube_with_corruption_four_in_flight() {
    corrupted(cube(), 4, 1238);
}

#[test]
fn generated_file_with_corruption_ping_pong() {
    corrupted(generated(2500), 1, 100);
}

#[test]
fn generated_file_with_corruption_four_in_flight() {
    corrupted(generated(2500), 4, 103);
}

fn lost_oks(max_in_flight: usize) {
    let mut sim = fast_sim(100.0);
    sim.drop_ok_rate = 0.02;
    sim.checksum_error_rate = 0.02;
    sim.seed = 7 + max_in_flight as u64;
    let printer = connect(
        sim,
        ConnectOptions {
            max_in_flight,
            ok_timeout_secs: 0.25,
            ..options()
        },
    );
    let file = generated(400);
    let want = expected_commands(&file);
    let (got, _) = print(&printer, Arc::clone(&file), PrintOptions::default());
    assert_same_commands(&got, &want);
    let s = printer.snapshot();
    assert!(s.link.timeouts > 0, "{:?}", s.link);
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.kind == ConsoleKind::Warning && e.text.contains("resynchronising"))
    );
}

#[test]
fn lost_oks_are_recovered_ping_pong() {
    lost_oks(1);
}

#[test]
fn lost_oks_are_recovered_four_in_flight() {
    lost_oks(4);
}

#[test]
fn start_line_and_preamble() {
    let printer = connect(fast_sim(100.0), options());
    let file = generated(300);
    let start = file.len() / 2;
    let options = PrintOptions {
        start_line: start,
        preamble: vec!["M104 S0 ; preamble".into(), "G92 E0\n\nG1 F1200".into()],
        ..PrintOptions::default()
    };
    let events = printer.subscribe();
    let control = printer.sim_control().unwrap();
    control.clear_received();
    printer.start_print(Arc::clone(&file), options).unwrap();
    let first = printer.with_state(|s| s.job.clone()).unwrap();
    assert_eq!(first.current_line, start);
    assert!(first.bytes_done > 0 && first.progress > 0.3, "{first:?}");
    wait_for(&printer, "completion", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    let mut want = vec!["M104 S0".to_string(), "G92 E0".into(), "G1 F1200".into()];
    want.extend((start..file.len()).filter_map(|i| file.command(i).map(|c| c.into_owned())));
    assert_same_commands(&control.received_commands(), &want);
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
            JobStatus::Completed
        ]
    );
}

#[test]
fn start_preconditions() {
    let printer = connect(fast_sim(100.0), options());
    let file = generated(50);
    let past_end = PrintOptions {
        start_line: file.len() + 1,
        ..PrintOptions::default()
    };
    assert!(matches!(
        printer.start_print(Arc::clone(&file), past_end),
        Err(HostError::InvalidArgument(_))
    ));
    // Slow the moves down so the job is still running for the next calls.
    let mut sim = fast_sim(1.0);
    sim.instant_moves = false;
    let slow = connect(sim, options());
    slow.start_print(cube(), PrintOptions::default()).unwrap();
    assert!(matches!(
        slow.start_print(Arc::clone(&file), PrintOptions::default()),
        Err(HostError::Busy(_))
    ));
    assert!(matches!(
        slow.start_sd_print("x.gco"),
        Err(HostError::Busy(_))
    ));
    assert!(matches!(
        slow.upload_to_sd(Arc::clone(&file), "x.gco"),
        Err(HostError::Busy(_))
    ));
    slow.cancel();
    wait_for(&slow, "cancelled", |s| !s.job_active());
    assert!(matches!(
        Printer::new().start_print(file, PrintOptions::default()),
        Err(HostError::NotConnected)
    ));
}

#[test]
fn progress_reports_with_m73() {
    let mut sim = fast_sim(50.0);
    sim.instant_moves = false;
    let printer = connect(sim, options());
    let file = generated(300);
    let (got, _) = print(
        &printer,
        Arc::clone(&file),
        PrintOptions {
            emit_progress: true,
            ..PrintOptions::default()
        },
    );
    let pcts: Vec<u32> = got
        .iter()
        .filter_map(|c| c.strip_prefix("M73 P"))
        .filter_map(|r| r.split_whitespace().next()?.parse().ok())
        .collect();
    assert!(pcts.len() >= 2, "{pcts:?}");
    assert!(pcts.windows(2).all(|w| w[0] < w[1]), "{pcts:?}");
    let without: Vec<String> = got.into_iter().filter(|c| !c.starts_with("M73 ")).collect();
    assert_same_commands(&without, &expected_commands(&file));
}

#[test]
fn unnumbered_and_print_only_checksum_modes() {
    for mode in [ChecksumMode::Never, ChecksumMode::PrintOnly] {
        let printer = connect(
            fast_sim(100.0),
            ConnectOptions {
                checksum: mode,
                ..options()
            },
        );
        let file = generated(200);
        let before = last_seq(&printer);
        let (got, _) = print(&printer, Arc::clone(&file), PrintOptions::default());
        assert_same_commands(&got, &expected_commands(&file));
        let sent: Vec<_> = printer
            .console_since(before, usize::MAX)
            .into_iter()
            .filter(|e| e.kind == ConsoleKind::Sent && e.text.starts_with("G1"))
            .collect();
        assert!(!sent.is_empty());
        match mode {
            ChecksumMode::Never => assert!(sent.iter().all(|e| e.line.is_none())),
            _ => assert!(sent.iter().all(|e| e.line.is_some())),
        }
        // Outside a job nothing is numbered.
        let seq = last_seq(&printer);
        printer.send("M114");
        wait_idle(&printer);
        let m114 = printer
            .console_since(seq, usize::MAX)
            .into_iter()
            .find(|e| e.kind == ConsoleKind::Sent)
            .unwrap();
        assert_eq!(m114.line, None, "{mode:?}");
    }
}

/// Start a print with real (scaled) move times so it runs for a while.
fn slow_print(printer: &Printer) {
    printer.sim_control().unwrap().set_time_scale(2.0);
    printer
        .start_print(cube(), PrintOptions::default())
        .expect("start print");
    wait_for(printer, "the print to run", |s| {
        s.job.as_ref().is_some_and(|j| j.current_line > 50)
    });
}

fn slow_sim() -> sprint_host::sprint_sim::SimConfig {
    let mut sim = fast_sim(100.0);
    sim.instant_moves = false;
    sim
}

#[test]
fn disconnect_during_a_print_does_not_hang() {
    let printer = connect(slow_sim(), options());
    let control = printer.sim_control().unwrap();
    slow_print(&printer);
    let start = Instant::now();
    printer.disconnect();
    let took = start.elapsed();
    assert!(took < Duration::from_secs(2), "disconnect took {took:?}");
    let s = printer.snapshot();
    assert_eq!(s.status, ConnectionStatus::Disconnected);
    assert_eq!(
        s.job.map(|j| j.status),
        Some(JobStatus::Failed("disconnected".into()))
    );
    assert!(!control.is_running(), "the simulator was shut down");
    assert!(printer.sim_control().is_none());
}

#[test]
fn dropping_every_handle_stops_the_engine() {
    let printer = connect(slow_sim(), options());
    let control = printer.sim_control().unwrap();
    let clone = printer.clone();
    slow_print(&printer);
    drop(printer);
    assert!(control.is_running(), "a clone is still alive");
    let start = Instant::now();
    drop(clone);
    assert!(start.elapsed() < Duration::from_secs(3));
    let deadline = Instant::now() + PATIENCE;
    while control.is_running() {
        assert!(Instant::now() < deadline, "the engine kept the link open");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn emergency_stop_halts_and_a_reset_recovers() {
    let printer = connect(slow_sim(), options());
    let control = printer.sim_control().unwrap();
    slow_print(&printer);
    printer.emergency_stop();
    wait_for(&printer, "halted", |s| s.halted);
    let s = printer.snapshot();
    assert_eq!(
        s.job.map(|j| j.status),
        Some(JobStatus::Failed("emergency stop".into()))
    );
    wait_for(&printer, "the simulator to halt", |_| {
        control.snapshot().halted
    });
    assert!(control.received_commands().iter().any(|c| c == "M112"));
    assert!(matches!(
        printer.start_print(generated(10), PrintOptions::default()),
        Err(HostError::Halted)
    ));
    // Resetting the board brings the printer back.
    control.reset();
    wait_for(&printer, "reconnected after the reset", |s| {
        s.is_connected() && !s.halted && s.settings_loaded
    });
    wait_idle(&printer);
    control.set_time_scale(100.0);
    let file = generated(50);
    let (got, _) = print(&printer, Arc::clone(&file), PrintOptions::default());
    assert_same_commands(&got, &expected_commands(&file));
}

#[test]
fn a_board_reset_mid_print_fails_the_job_and_reconnects() {
    let printer = connect(slow_sim(), options());
    let control = printer.sim_control().unwrap();
    let events = printer.subscribe();
    slow_print(&printer);
    control.reset();
    wait_for(&printer, "the job to fail", |s| !s.job_active());
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Failed("printer reset".into()))
    );
    wait_for(&printer, "reconnected", |s| {
        s.is_connected() && s.settings_loaded
    });
    let statuses: Vec<_> = events
        .try_iter()
        .filter_map(|e| match e {
            Event::Status(s) => Some(s),
            _ => None,
        })
        .collect();
    assert!(statuses.contains(&ConnectionStatus::Handshaking));
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.kind == ConsoleKind::Warning && e.text.contains("reset"))
    );
    // The line counter was reset and printing works again.
    wait_idle(&printer);
    control.set_time_scale(100.0);
    let file = generated(100);
    let (got, _) = print(&printer, Arc::clone(&file), PrintOptions::default());
    assert_same_commands(&got, &expected_commands(&file));
}

#[test]
#[ignore = "benchmark: cargo test -p sprint-host --test print -- --ignored --nocapture"]
fn throughput() {
    for max_in_flight in [1, 2, 4, 8] {
        let printer = connect(
            fast_sim(1000.0),
            ConnectOptions {
                max_in_flight,
                ..options()
            },
        );
        let file = generated(20_000);
        let n = expected_commands(&file).len();
        let (_, elapsed) = print(&printer, file, PrintOptions::default());
        eprintln!(
            "max_in_flight {max_in_flight}: {n} commands in {elapsed:.2?} = {:.0} lines/s",
            n as f64 / elapsed.as_secs_f64()
        );
    }
}
