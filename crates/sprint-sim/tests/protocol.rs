//! Line protocol, flow control and handle semantics through the threaded
//! link, using a tiny in-test host.

mod common;

use common::{Host, PATIENCE, fast, frame};
use sprint_sim::{SimConfig, spawn};
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

fn stream_config() -> SimConfig {
    SimConfig {
        instant_moves: true,
        ..fast(50.0)
    }
}

fn moves(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("G1 X{}.{} Y{} F6000", i % 200, i % 10, (i * 7) % 200))
        .collect()
}

#[test]
fn boot_banner_and_basic_replies() {
    let mut h = Host::new(fast(50.0));
    let banner = h.boot();
    assert_eq!(banner[0], "start");
    assert_eq!(banner[1], "echo:Marlin 2.1.2.1");
    assert!(
        banner
            .iter()
            .any(|l| l.starts_with("echo:V86 stored settings retrieved ("))
    );
    assert!(banner.iter().any(|l| l == "echo:; Steps per unit:"));
    assert_eq!(h.command("G90"), vec!["ok"]);
    let temps = h.command("M105");
    assert!(temps[0].starts_with("ok T:"), "{temps:?}");
    assert_eq!(
        h.command("G999"),
        vec!["echo:Unknown command: \"G999\"", "ok"]
    );
    let info = h.command("M115");
    assert!(info[0].starts_with("FIRMWARE_NAME:Marlin 2.1.2.1 (Sprint Virtual)"));
    assert!(info.iter().any(|l| l == "Cap:AUTOREPORT_TEMP:1"));
}

#[test]
fn numbered_ping_pong_and_error_replies() {
    let mut h = Host::booted(fast(50.0));
    assert_eq!(h.command("M110 N0"), vec!["ok"]);
    assert_eq!(h.numbered("G90"), vec!["ok"]);
    assert_eq!(h.numbered("G91"), vec!["ok"]);
    h.write(&frame(5, "G90"));
    assert_eq!(
        h.until(PATIENCE, |l| l == "ok"),
        vec![
            "Error:Line Number is not Last Line Number+1, Last Line: 2",
            "Resend: 3",
            "ok"
        ]
    );
    h.write("N3 G90*7");
    assert_eq!(
        h.until(PATIENCE, |l| l == "ok"),
        vec!["Error:checksum mismatch, Last Line: 2", "Resend: 3", "ok"]
    );
    h.write("N3 G90");
    assert_eq!(
        h.until(PATIENCE, |l| l == "ok"),
        vec![
            "Error:No Checksum with line number, Last Line: 2",
            "Resend: 3",
            "ok"
        ]
    );
    h.write("M105*12");
    assert_eq!(
        h.until(PATIENCE, |l| l == "ok"),
        vec!["Error:No Line Number with checksum, Last Line: 2", "ok"]
    );
    // A comment after the checksum is tolerated.
    h.write(&format!("{}   ; comment", frame(3, "G90")));
    assert_eq!(h.until(PATIENCE, |l| l == "ok"), vec!["ok"]);
    let snap = h.control().snapshot();
    assert_eq!(snap.last_line_number, 3);
    assert_eq!(snap.resends_requested, 3);
    // Rejected lines are not recorded; the comment is stripped.
    assert_eq!(
        h.control().received_commands(),
        vec!["M110 N0", "G90", "G91", "G90"]
    );
}

#[test]
fn streamed_file_arrives_once_and_in_order_despite_resends() {
    let mut h = Host::booted(SimConfig {
        checksum_error_rate: 0.25,
        seed: 1234,
        ..stream_config()
    });
    h.command("M110 N0");
    h.control().clear_received();
    let cmds = moves(300);
    let (resends, _) = h.stream(&cmds, Duration::from_secs(2));
    assert!(resends > 30, "only {resends} resends injected");
    assert_eq!(h.control().received_commands(), cmds);
    let snap = h.control().snapshot();
    assert_eq!(snap.resends_requested, resends);
    assert_eq!(snap.last_line_number, 300);
}

#[test]
fn lost_oks_are_recoverable_without_duplicates() {
    let mut h = Host::booted(SimConfig {
        drop_ok_rate: 0.2,
        seed: 77,
        ..stream_config()
    });
    h.command("M110 N0");
    h.control().clear_received();
    let cmds = moves(60);
    let (_, timeouts) = h.stream(&cmds, Duration::from_millis(150));
    assert!(timeouts > 3, "only {timeouts} oks dropped");
    let received: Vec<String> = h
        .control()
        .received_commands()
        .into_iter()
        .filter(|c| c != "M105")
        .collect();
    assert_eq!(received, cmds);
}

#[test]
fn rates_can_change_at_runtime() {
    let mut h = Host::booted(stream_config());
    h.command("M110 N0");
    h.control().set_checksum_error_rate(1.0);
    h.write(&frame(1, "G90"));
    let out = h.until(PATIENCE, |l| l == "ok");
    assert_eq!(out[0], "Error:checksum mismatch, Last Line: 0");
    h.control().set_checksum_error_rate(0.0);
    h.control().set_drop_ok_rate(1.0);
    h.write(&frame(1, "G90"));
    assert!(h.drain(Duration::from_millis(200)).is_empty());
    h.control().set_drop_ok_rate(0.0);
    h.next_line = 2;
    assert_eq!(h.numbered("G90"), vec!["ok"]);
}

#[test]
fn advanced_ok_reports_buffers() {
    let mut h = Host::booted(SimConfig {
        advanced_ok: true,
        ..stream_config()
    });
    h.command("M110 N0");
    assert_eq!(h.numbered("G90"), vec!["ok N1 P16 B3"]);
    assert_eq!(h.command("G91"), vec!["ok P16 B3"]);
}

#[test]
fn several_commands_in_flight() {
    let mut h = Host::booted(stream_config());
    h.command("M110 N0");
    h.control().clear_received();
    let cmds = moves(40);
    // Send everything at once; the firmware queues what fits and the
    // rest waits in the receive buffer.
    let mut burst = String::new();
    for (i, c) in cmds.iter().enumerate() {
        burst.push_str(&frame(i as u32 + 1, c));
        burst.push('\n');
    }
    h.link.writer.write_all(burst.as_bytes()).unwrap();
    let mut oks = 0;
    while oks < cmds.len() {
        let l = h.line(PATIENCE).expect("reply");
        if l == "ok" {
            oks += 1;
        }
    }
    assert_eq!(h.control().received_commands(), cmds);
}

#[test]
fn emergency_kill_and_reset() {
    let mut h = Host::booted(fast(50.0));
    h.command("M104 S200");
    h.write("M112");
    let out = h.until(PATIENCE, |l| l.starts_with("Error:"));
    assert_eq!(
        out.last().map(String::as_str),
        Some("Error:Printer halted. kill() called!")
    );
    let snap = h.control().snapshot();
    assert!(snap.halted);
    assert_eq!(snap.hotend_targets, vec![0.0]);
    h.write("M105");
    assert!(h.drain(Duration::from_millis(300)).is_empty());
    assert!(
        h.control()
            .received_commands()
            .contains(&"M112".to_string())
    );
    h.control().reset();
    let banner = h.boot();
    assert_eq!(banner[0], "start");
    assert!(!h.control().snapshot().halted);
    assert!(h.command("M105")[0].starts_with("ok T:"));
}

#[test]
fn reset_restarts_line_numbers() {
    let mut h = Host::booted(fast(50.0));
    h.command("M110 N0");
    h.numbered("G90");
    h.numbered("G90");
    assert_eq!(h.control().snapshot().last_line_number, 2);
    h.control().reset();
    h.boot();
    assert_eq!(h.control().snapshot().last_line_number, 0);
    h.next_line = 1;
    assert_eq!(h.numbered("G90"), vec!["ok"]);
}

#[test]
fn inject_line_reaches_the_host() {
    let mut h = Host::booted(fast(50.0));
    h.control().inject_line("//action:pause");
    assert_eq!(
        h.until(PATIENCE, |l| l.starts_with("//")),
        vec!["//action:pause"]
    );
}

#[test]
fn input_before_boot_is_ignored() {
    let mut h = Host::new(SimConfig {
        boot_delay_ms: 300,
        ..SimConfig::default()
    });
    h.write("M105");
    let banner = h.boot();
    assert!(!banner.iter().any(|l| l.starts_with("ok")), "{banner:?}");
    assert!(h.drain(Duration::from_millis(100)).is_empty());
    assert!(h.control().received_commands().is_empty());
}

#[test]
fn reader_times_out_and_shutdown_closes_both_ends() {
    let mut link = spawn(fast(50.0));
    link.reader.set_timeout(Duration::from_millis(30));
    // Drain the banner.
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + PATIENCE;
    let mut text = String::new();
    while !text.contains("echo:SD card ok\n") {
        assert!(Instant::now() < deadline);
        match link.reader.read(&mut buf) {
            Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) => assert_eq!(e.kind(), ErrorKind::TimedOut),
        }
    }
    let t = Instant::now();
    let err = link.reader.read(&mut buf).expect_err("nothing to read");
    assert_eq!(err.kind(), ErrorKind::TimedOut);
    let waited = t.elapsed();
    assert!(
        waited >= Duration::from_millis(25) && waited < Duration::from_secs(1),
        "{waited:?}"
    );
    assert!(link.control.is_running());
    link.control.shutdown();
    assert!(!link.control.is_running());
    let err = link.writer.write(b"M105\n").expect_err("closed");
    assert_eq!(err.kind(), ErrorKind::BrokenPipe);
    let deadline = Instant::now() + PATIENCE;
    loop {
        match link.reader.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => assert_eq!(e.kind(), ErrorKind::TimedOut),
        }
        assert!(Instant::now() < deadline, "reader never closed");
    }
}

#[test]
fn dropping_the_other_handles_stops_the_thread() {
    let link = spawn(fast(50.0));
    let mut reader = link.reader;
    drop(link.writer);
    drop(link.control);
    reader.set_timeout(Duration::from_millis(50));
    let mut buf = [0u8; 1024];
    let deadline = Instant::now() + PATIENCE;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => assert_eq!(e.kind(), ErrorKind::TimedOut),
        }
        assert!(Instant::now() < deadline, "thread kept running");
    }
}

#[test]
fn snapshot_tracks_state() {
    let mut h = Host::booted(stream_config());
    h.command("G92 X10 Y20 Z5 E1");
    h.command("M106 S200");
    h.command("M220 S120");
    h.command("M221 S90");
    h.command("M140 S50");
    let s = h.control().snapshot();
    assert_eq!(s.position, [10.0, 20.0, 5.0, 1.0]);
    assert_eq!(s.fan, 200);
    assert_eq!(s.feedrate_pct, 120.0);
    assert_eq!(s.flow_pct, 90.0);
    assert_eq!(s.bed_target, 50.0);
    assert_eq!(s.hotend_temps.len(), 1);
    assert!(s.commands_received >= 5);
    assert!(!s.sd_printing && !s.halted);
}

#[test]
fn garbage_bytes_do_not_break_the_link() {
    let mut h = Host::booted(stream_config());
    let junk: Vec<u8> = (0..4000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    h.link.writer.write_all(&junk).unwrap();
    h.link.writer.write_all(b"\nM108\nM410\n").unwrap();
    h.drain(Duration::from_millis(300));
    h.control().reset();
    h.boot();
    assert!(h.command("M105")[0].starts_with("ok T:"));
}
