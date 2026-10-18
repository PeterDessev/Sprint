//! Planner timing and motion commands through the threaded link.

mod common;

use common::{Host, PATIENCE, fast};
use sprint_sim::SimConfig;
use std::time::{Duration, Instant};

#[test]
fn hundred_mm_move_takes_expected_time() {
    let scale = 2.0;
    let mut h = Host::booted(fast(scale as f32));
    h.command("G92 X0 Y0 Z0");
    let start = Instant::now();
    h.command("G1 X100 F6000");
    h.command("M400");
    let real = start.elapsed().as_secs_f64();
    // 100 mm at 100 mm/s with 3000 mm/s² takes 1.033 s of machine time.
    let expected = (1.0 + 100.0 / 3000.0) / scale;
    assert!(
        (real - expected).abs() < 0.2,
        "took {real:.3}s, expected {expected:.3}s"
    );
    assert_eq!(
        h.command("M114")[0],
        "X:100.00 Y:0.00 Z:0.00 E:0.00 Count X:8000 Y:0 Z:0"
    );
}

#[test]
fn full_planner_delays_ok() {
    let mut h = Host::booted(SimConfig {
        planner_buffer: 4,
        ..fast(20.0)
    });
    h.command("G92 X0 Y0 Z0");
    // Each 10 mm move at 10 mm/s takes ~1 s simulated (50 ms real).
    for i in 1..=4 {
        h.command(&format!("G1 X{} F600", i * 10));
    }
    assert_eq!(h.control().snapshot().planner_queue, 4);
    let start = Instant::now();
    h.command("G1 X50 F600");
    let waited = start.elapsed();
    assert!(waited > Duration::from_millis(25), "{waited:?}");
    h.command("M400");
    assert_eq!(h.control().snapshot().planner_queue, 0);
}

#[test]
fn homing_and_position_report() {
    let mut h = Host::booted(fast(100.0));
    let out = h.command("G28");
    assert_eq!(out.last().map(String::as_str), Some("ok"));
    assert_eq!(
        h.command("M114")[0],
        "X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0"
    );
    h.command("M302 P1");
    h.command("G1 X10 Y20 Z0.3 E1.23 F3000");
    assert_eq!(
        h.command("M114")[0],
        "X:10.00 Y:20.00 Z:0.30 E:1.23 Count X:800 Y:1600 Z:120"
    );
    h.command("M154 S1");
    let line = h.until(PATIENCE, |l| l.starts_with("X:"));
    assert_eq!(
        line.last().unwrap(),
        "X:10.00 Y:20.00 Z:0.30 E:1.23 Count X:800 Y:1600 Z:120"
    );
    h.command("M154 S0");
}

#[test]
fn leveling_probe_and_report() {
    let mut h = Host::booted(SimConfig {
        mesh_size: (3, 4),
        ..fast(200.0)
    });
    let out = h.command("G29");
    let i = out
        .iter()
        .position(|l| l == "Bilinear Leveling Grid:")
        .unwrap();
    assert_eq!(out[i + 1], "      0      1      2");
    assert_eq!(out.len() - i - 1, 1 + 4 + 1, "{out:?}");
    let v = h.command("M420 V");
    assert_eq!(v[0], "Bilinear Leveling Grid:");
    assert_eq!(&v[1..6], &out[i + 1..i + 6]);
    assert!(v.contains(&"echo:Bed Leveling ON".to_string()));
    h.command("M500");
    h.control().reset();
    h.boot();
    let v = h.command("M420 V");
    assert_eq!(
        v[0], "Bilinear Leveling Grid:",
        "mesh not restored from EEPROM"
    );
    assert!(v.contains(&"echo:Bed Leveling OFF".to_string()));
}

#[test]
fn quickstop_from_the_emergency_parser() {
    let mut h = Host::booted(fast(1.0));
    h.command("G92 X0 Y0 Z0");
    h.command("G1 X200 F600");
    std::thread::sleep(Duration::from_millis(300));
    h.write("M410");
    h.until(PATIENCE, |l| l == "ok");
    let s = h.control().snapshot();
    assert_eq!(s.planner_queue, 0);
    assert!(
        s.position[0] > 0.5 && s.position[0] < 20.0,
        "{:?}",
        s.position
    );
}

#[test]
fn settings_commands_and_eeprom() {
    let mut h = Host::booted(fast(50.0));
    h.command("M92 X81.5");
    h.command("M203 Z12");
    h.command("M851 Z-1.25");
    h.command("M900 K0.05");
    let report = h.command("M503");
    for expected in [
        "echo:  M92 X81.50 Y80.00 Z400.00 E93.00",
        "echo:  M203 X300.00 Y300.00 Z12.00 E25.00",
        "echo:  M851 X10.00 Y10.00 Z-1.25 ; (mm)",
        "echo:  M900 K0.05",
        "echo:; Hotend PID:",
        "echo:; Bed PID:",
    ] {
        assert!(report.contains(&expected.to_string()), "missing {expected}");
    }
    let stored = h.command("M500");
    assert!(
        stored[0].starts_with("echo:Settings Stored ("),
        "{stored:?}"
    );
    h.control().reset();
    let banner = h.boot();
    assert!(banner.contains(&"echo:  M92 X81.50 Y80.00 Z400.00 E93.00".to_string()));
    h.command("M502");
    assert!(
        h.command("M503")
            .contains(&"echo:  M92 X80.00 Y80.00 Z400.00 E93.00".to_string())
    );
}

#[test]
fn time_scale_changes_at_runtime() {
    let mut h = Host::booted(fast(1.0));
    let start = Instant::now();
    h.command("G4 P400");
    let slow = start.elapsed();
    h.control().set_time_scale(20.0);
    let start = Instant::now();
    h.command("G4 P400");
    let fast_dwell = start.elapsed();
    assert!(slow >= Duration::from_millis(380), "{slow:?}");
    assert!(fast_dwell < Duration::from_millis(200), "{fast_dwell:?}");
}
