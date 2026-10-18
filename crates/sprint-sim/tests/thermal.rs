//! Heating curves, waits and autotune at high time scales.

mod common;

use common::{Host, PATIENCE, fast};
use std::time::{Duration, Instant};

/// Parse " T:205.12 /210.00 B:..." (or "ok T:...") into (hotend, bed).
fn temps(line: &str) -> Option<(f32, f32)> {
    let mut t = None;
    let mut b = None;
    for part in line.split_whitespace() {
        if let Some(v) = part.strip_prefix("T:") {
            t = v.parse().ok();
        } else if let Some(v) = part.strip_prefix("B:") {
            b = v.parse().ok();
        }
    }
    Some((t?, b.unwrap_or(0.0)))
}

/// Follow M155 reports; returns (simulated seconds until `reached`, peak).
fn follow(
    h: &mut Host,
    scale: f64,
    secs: f64,
    pick: impl Fn((f32, f32)) -> f32,
    reached: f32,
) -> (f64, f32) {
    let start = Instant::now();
    let mut hit = None;
    let mut peak = f32::MIN;
    while start.elapsed().as_secs_f64() * scale < secs {
        let Some(line) = h.line(Duration::from_millis(200)) else {
            continue;
        };
        if let Some(v) = temps(&line).map(&pick) {
            peak = peak.max(v);
            if hit.is_none() && v >= reached {
                hit = Some(start.elapsed().as_secs_f64() * scale);
            }
        }
    }
    (hit.unwrap_or(f64::INFINITY), peak)
}

#[test]
fn hotend_heating_curve() {
    let scale = 100.0;
    let mut h = Host::booted(fast(scale as f32));
    h.command("M155 S1");
    let first = h.until(PATIENCE, |l| l.starts_with(" T:"));
    let ambient = temps(first.last().unwrap()).unwrap().0;
    assert!((ambient - 22.0).abs() < 0.5, "{ambient}");
    h.command("M104 S200");
    let (t, peak) = follow(&mut h, scale, 150.0, |(t, _)| t, 199.0);
    assert!((45.0..=95.0).contains(&t), "reached 199 after {t:.1}s");
    assert!(peak > 200.2 && peak < 210.0, "peak {peak}");
    let snap = h.control().snapshot();
    assert!(
        (snap.hotend_temps[0] - 200.0).abs() < 1.5,
        "{:?}",
        snap.hotend_temps
    );
    assert_eq!(snap.hotend_targets, vec![200.0]);
    // Power is reported while holding temperature.
    let line = h.command("M105");
    let power: u32 = line[0]
        .split_whitespace()
        .find_map(|p| p.strip_prefix("@:"))
        .and_then(|v| v.parse().ok())
        .unwrap();
    assert!((20..=100).contains(&power), "{line:?}");
}

#[test]
fn bed_heating_curve() {
    let scale = 200.0;
    let mut h = Host::booted(fast(scale as f32));
    h.command("M155 S2");
    h.command("M140 S60");
    let (t, peak) = follow(&mut h, scale, 300.0, |(_, b)| b, 59.0);
    assert!((80.0..=180.0).contains(&t), "bed reached 59 after {t:.1}s");
    assert!(peak < 64.0, "peak {peak}");
}

#[test]
fn m109_waits_and_reports() {
    let scale = 200.0;
    let mut h = Host::booted(fast(scale as f32));
    let start = Instant::now();
    let out = h.command("M109 S210");
    let sim_secs = start.elapsed().as_secs_f64() * scale;
    assert!(
        (50.0..=130.0).contains(&sim_secs),
        "M109 took {sim_secs:.1}s"
    );
    let waits: Vec<&String> = out.iter().filter(|l| l.starts_with(" T:")).collect();
    assert!(waits.len() > 40, "{} wait reports", waits.len());
    assert!(
        waits[0].contains(" /210.00 B:") && waits[0].ends_with(" W:?"),
        "{}",
        waits[0]
    );
    assert!(out.iter().any(|l| l == "echo:busy: processing"));
    assert_eq!(out.last().map(String::as_str), Some("ok"));
    // Bed waits print the same lines.
    let out = h.command("M190 S40");
    assert!(
        out.iter()
            .any(|l| l.starts_with(" T:") && l.contains(" B:"))
    );
}

#[test]
fn m108_cancels_a_wait() {
    let mut h = Host::booted(fast(10.0));
    h.write("M109 S250");
    h.until(PATIENCE, |l| l.starts_with(" T:"));
    h.write("M108");
    let out = h.until(PATIENCE, |l| l == "ok");
    assert!(!out.iter().any(|l| l.starts_with("Error")));
    // The M108 itself is acknowledged too.
    h.until(PATIENCE, |l| l == "ok");
    assert_eq!(h.control().snapshot().hotend_targets, vec![250.0]);
}

#[test]
fn pid_autotune_output() {
    let mut h = Host::booted(fast(1000.0));
    let out = h.command("M303 E0 S200 C5 U1");
    assert_eq!(out[0], "PID Autotune start");
    let bias: Vec<&String> = out.iter().filter(|l| l.starts_with(" bias: ")).collect();
    assert!(bias.len() >= 4, "{out:?}");
    assert!(
        bias.iter()
            .any(|l| l.contains(" Ku: ") && l.contains(" Tu: "))
    );
    assert!(out.iter().any(|l| l == " Classic PID"));
    let kp_line = out.iter().rev().find(|l| l.starts_with(" Kp: ")).unwrap();
    let defines: Vec<&String> = out
        .iter()
        .filter(|l| l.starts_with("#define DEFAULT_K"))
        .collect();
    assert_eq!(defines.len(), 3, "{out:?}");
    let kp: f32 = defines[0]["#define DEFAULT_Kp ".len()..].parse().unwrap();
    assert!(
        kp_line.contains(&format!("Kp: {kp:.2}")),
        "{kp_line} vs {kp}"
    );
    assert!(out.contains(
        &"PID Autotune finished! Put the last Kp, Ki and Kd constants from below into Configuration.h"
            .to_string()
    ));
    // Heater state lines are printed while tuning.
    assert!(out.iter().any(|l| l.starts_with(" T:")));
    // U1 applied the result.
    let report = h.command("M301");
    assert!(
        report[1].starts_with(&format!("echo:  M301 P{kp:.2} ")),
        "{report:?}"
    );
}

#[test]
fn bed_autotune_uses_bed_defines() {
    let mut h = Host::booted(fast(2000.0));
    let out = h.command("M303 E-1 S60 C3");
    assert!(out.iter().any(|l| l == " No overshoot"), "{out:?}");
    assert!(out.iter().any(|l| l.starts_with("#define DEFAULT_bedKp ")));
}
