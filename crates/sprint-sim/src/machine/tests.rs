//! Deterministic tests driving the [`Machine`] directly in simulated time.

use super::*;

/// A test rig: a machine plus the output collected so far.
struct Rig {
    m: Machine,
    out: Vec<String>,
}

fn frame(n: i64, cmd: &str) -> String {
    let payload = format!("N{n} {cmd}");
    let cs = crate::line::checksum(&payload);
    format!("{payload}*{cs}")
}

fn cfg() -> SimConfig {
    SimConfig {
        boot_delay_ms: 100,
        ..SimConfig::default()
    }
}

impl Rig {
    fn new(cfg: SimConfig) -> Rig {
        let mut rig = Rig {
            m: Machine::new(cfg),
            out: Vec::new(),
        };
        rig.run(0.5);
        rig
    }

    fn booted() -> Rig {
        let mut r = Rig::new(cfg());
        r.take();
        r
    }

    fn collect(&mut self) {
        self.out.extend(self.m.take_output());
    }

    fn raw(&mut self, bytes: &str) {
        self.m.on_bytes(bytes.as_bytes());
        self.m.advance(0.0);
        self.collect();
    }

    fn send(&mut self, line: &str) {
        self.raw(&format!("{line}\n"));
    }

    fn run(&mut self, secs: f64) {
        let mut t = 0.0;
        while t < secs {
            let d = (secs - t).min(0.5);
            self.m.advance(d);
            self.collect();
            t += d;
        }
    }

    fn take(&mut self) -> Vec<String> {
        std::mem::take(&mut self.out)
    }

    /// Send and return the output until the "ok" (inclusive), running
    /// simulated time up to `limit` seconds.
    fn cmd(&mut self, line: &str, limit: f64) -> Vec<String> {
        self.send(line);
        let mut waited = 0.0;
        while !self.out.iter().any(|l| l.starts_with("ok")) && waited < limit {
            self.run(0.05);
            waited += 0.05;
        }
        self.take()
    }

    fn ok(&mut self, line: &str) -> Vec<String> {
        let out = self.cmd(line, 600.0);
        assert!(
            out.iter().any(|l| l.starts_with("ok")),
            "no ok for {line}: {out:?}"
        );
        out
    }
}

#[test]
fn boot_banner_after_delay() {
    let mut r = Rig {
        m: Machine::new(cfg()),
        out: Vec::new(),
    };
    r.m.advance(0.05);
    r.collect();
    assert!(r.out.is_empty(), "{:?}", r.out);
    r.run(0.2);
    let out = r.take();
    assert_eq!(out[0], "start");
    assert_eq!(out[1], "echo:Marlin 2.1.2.1");
    assert!(out[2].starts_with("echo: Last Updated:"));
    assert!(out[3].starts_with("echo: Compiled:"));
    assert!(out[4].starts_with("echo: Free Memory:"));
    assert!(out[5].starts_with("echo:V86 stored settings retrieved ("));
    assert!(out[5].contains(" bytes; crc "));
    assert!(out.contains(&"echo:  M92 X80.00 Y80.00 Z400.00 E93.00".to_string()));
    assert_eq!(out.last().map(String::as_str), Some("echo:SD card ok"));
}

#[test]
fn input_during_boot_is_discarded() {
    let mut r = Rig {
        m: Machine::new(cfg()),
        out: Vec::new(),
    };
    r.raw("M105\n");
    r.run(0.5);
    let out = r.take();
    assert!(!out.iter().any(|l| l.starts_with("ok")), "{out:?}");
    assert!(r.m.take_received().is_empty());
}

#[test]
fn plain_and_unknown_commands() {
    let mut r = Rig::booted();
    assert_eq!(r.ok("G90"), vec!["ok"]);
    assert_eq!(
        r.ok("M12345 X1"),
        vec!["echo:Unknown command: \"M12345 X1\"", "ok"]
    );
    assert_eq!(
        r.ok("FOO BAR"),
        vec!["echo:Unknown command: \"FOO BAR\"", "ok"]
    );
    // Comments and whitespace are tolerated; blank lines produce nothing.
    assert_eq!(r.ok("   G91   ; relative"), vec!["ok"]);
    r.send("");
    r.send("; just a comment");
    r.run(0.1);
    assert!(r.take().is_empty());
}

#[test]
fn numbered_lines_and_errors() {
    let mut r = Rig::booted();
    assert_eq!(r.ok(&frame(1, "G90")), vec!["ok"]);
    assert_eq!(r.ok(&frame(2, "G91")), vec!["ok"]);
    assert_eq!(
        r.ok(&frame(4, "G90")),
        vec![
            "Error:Line Number is not Last Line Number+1, Last Line: 2",
            "Resend: 3",
            "ok"
        ]
    );
    assert_eq!(
        r.ok("N3 G90*1"),
        vec!["Error:checksum mismatch, Last Line: 2", "Resend: 3", "ok"]
    );
    assert_eq!(
        r.ok("N3 G90"),
        vec![
            "Error:No Checksum with line number, Last Line: 2",
            "Resend: 3",
            "ok"
        ]
    );
    assert_eq!(
        r.ok("G90*12"),
        vec!["Error:No Line Number with checksum, Last Line: 2", "ok"]
    );
    assert_eq!(r.ok(&frame(3, "G90")), vec!["ok"]);
    let s = r.m.snapshot();
    assert_eq!(s.last_line_number, 3);
    assert_eq!(s.resends_requested, 3);
    assert_eq!(r.m.take_received(), vec!["G90", "G91", "G90"]);
}

#[test]
fn m110_sets_line_number() {
    let mut r = Rig::booted();
    r.ok(&frame(1, "G90"));
    r.ok("M110 N0");
    assert_eq!(r.ok(&frame(1, "G90")), vec!["ok"]);
    r.ok(&frame(41, "M110"));
    assert_eq!(r.m.snapshot().last_line_number, 41);
    assert_eq!(r.ok(&frame(42, "G90")), vec!["ok"]);
    r.ok(&frame(43, "M110 N100"));
    assert_eq!(r.ok(&frame(101, "G90")), vec!["ok"]);
}

#[test]
fn checksum_fault_injection_and_dropped_oks() {
    let mut r = Rig::booted();
    r.m.set_fault_rates(1.0, 0.0);
    let out = r.ok(&frame(1, "G90"));
    assert_eq!(out[0], "Error:checksum mismatch, Last Line: 0");
    r.m.set_fault_rates(0.0, 1.0);
    let out = r.cmd(&frame(1, "G90"), 1.0);
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(r.m.snapshot().last_line_number, 1);
    r.m.set_fault_rates(0.0, 0.0);
    assert_eq!(r.ok(&frame(2, "G90")), vec!["ok"]);
}

#[test]
fn temperature_report_format() {
    let mut r = Rig::booted();
    let out = r.ok("M105");
    assert_eq!(out.len(), 1);
    let line = &out[0];
    assert!(line.starts_with("ok T:"), "{line}");
    let parts: Vec<&str> = line.split(' ').collect();
    assert_eq!(parts[0], "ok");
    assert!(parts[1].starts_with("T:2"));
    assert_eq!(parts[2], "/0.0");
    assert!(parts[3].starts_with("B:2"));
    assert_eq!(parts[4], "/0.0");
    assert_eq!(&parts[5..], &["@:0", "B@:0"]);
}

#[test]
fn multi_extruder_temperature_report() {
    let mut r = Rig::new(SimConfig {
        extruders: 2,
        heated_chamber: true,
        ..cfg()
    });
    r.take();
    let out = r.ok("M105");
    let line = &out[0];
    for key in [
        " T:", " B:", " C:", " T0:", " T1:", " @:", " B@:", " C@:", " @0:", " @1:",
    ] {
        assert!(line.contains(key), "{key} missing in {line}");
    }
    assert_eq!(r.ok("T1"), vec!["echo:Active Extruder: 1", "ok"]);
    assert_eq!(r.ok("T5"), vec!["echo:T5 Invalid extruder", "ok"]);
}

#[test]
fn position_report_after_homing() {
    let mut r = Rig::booted();
    r.ok("G28");
    assert_eq!(
        r.ok("M114"),
        vec!["X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0", "ok"]
    );
    r.ok("G1 X10 Y20 Z0.3 F3000");
    r.ok("G92 E1.23");
    assert_eq!(
        r.ok("M114"),
        vec![
            "X:10.00 Y:20.00 Z:0.30 E:1.23 Count X:800 Y:1600 Z:120",
            "ok"
        ]
    );
    r.ok("G91");
    r.ok("G1 X5");
    r.ok("G90");
    r.ok("G92 X0");
    let out = r.ok("M114");
    assert!(out[0].starts_with("X:0.00 Y:20.00"), "{out:?}");
    assert!(out[0].contains("Count X:1200"), "{out:?}");
}

#[test]
fn homing_probing_and_home_offsets_report_the_position() {
    // Marlin's G28, G29 and M206 end with report_current_position().
    let mut r = Rig::booted();
    let out = r.ok("G28");
    assert_eq!(
        &out[out.len() - 2..],
        &["X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0", "ok"]
    );
    let out = r.ok("M206 X5");
    assert_eq!(
        out,
        vec!["X:5.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0", "ok"]
    );
    let out = r.ok("G29");
    let pos = r.ok("M114");
    assert_eq!(&out[out.len() - 2..], &[pos[0].clone(), "ok".to_string()]);
    // Errors and reports do not move anything.
    assert_eq!(r.ok("M206")[0], "echo:; Home offset:");
}

#[test]
fn homing_takes_realistic_time() {
    let mut r = Rig::booted();
    r.send("G28");
    let start = r.m.now;
    while !r.out.iter().any(|l| l == "ok") {
        r.run(0.1);
    }
    let t = r.m.now - start;
    assert!((8.0..40.0).contains(&t), "homing took {t}s");
    let out = r.take();
    assert!(out.iter().any(|l| l == "echo:busy: processing"), "{out:?}");
}

#[test]
fn move_timing_matches_planner() {
    let mut r = Rig::booted();
    r.ok("G28");
    let start = r.m.now;
    r.ok("G1 X100 F6000");
    r.ok("M400");
    let t = r.m.now - start;
    let expected = 1.0 + 100.0 / 3000.0;
    assert!((t - expected).abs() < 0.15, "took {t}, expected {expected}");
}

#[test]
fn planner_full_delays_ok() {
    let mut r = Rig::new(SimConfig {
        planner_buffer: 4,
        ..cfg()
    });
    r.take();
    r.ok("G28");
    r.ok("M302 P1");
    // Four slow moves fill the planner, the fifth must wait.
    for i in 1..=4 {
        assert_eq!(r.ok(&format!("G1 X{} F600", i * 10)), vec!["ok"]);
    }
    assert_eq!(r.m.snapshot().planner_queue, 4);
    let out = r.cmd("G1 X50 F600", 0.2);
    assert!(out.is_empty(), "{out:?}");
    r.run(2.0);
    assert!(r.take().contains(&"ok".to_string()));
}

#[test]
fn advanced_ok_format() {
    let mut r = Rig::new(SimConfig {
        advanced_ok: true,
        ..cfg()
    });
    r.take();
    assert_eq!(r.ok(&frame(1, "G90")), vec!["ok N1 P16 B3"]);
    assert_eq!(r.ok("G90"), vec!["ok P16 B3"]);
}

#[test]
fn rx_buffer_limits_queued_commands() {
    let mut r = Rig::booted();
    r.ok("G28");
    r.ok("M302 P1");
    // A long dwell blocks the queue; only rx_buffer commands are accepted.
    let mut burst = String::from("G4 S5\n");
    for _ in 0..10 {
        burst.push_str("G90\n");
    }
    r.raw(&burst);
    r.run(0.1);
    assert_eq!(r.m.queue.len() + usize::from(r.m.task.is_some()), 4);
    r.run(6.0);
    let oks = r.take().iter().filter(|l| *l == "ok").count();
    assert_eq!(oks, 11);
}

#[test]
fn firmware_info_and_capabilities() {
    let mut r = Rig::booted();
    let out = r.ok("M115");
    let first = &out[0];
    assert!(first.starts_with(
        "FIRMWARE_NAME:Marlin 2.1.2.1 (Sprint Virtual) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:Sprint Virtual Printer EXTRUDER_COUNT:1 UUID:"
    ));
    let uuid = first.rsplit("UUID:").next().unwrap_or("");
    assert_eq!(uuid.len(), 36);
    assert_eq!(uuid, r.m.uuid());
    for cap in [
        "Cap:EEPROM:1",
        "Cap:AUTOREPORT_TEMP:1",
        "Cap:AUTOREPORT_POS:1",
        "Cap:EMERGENCY_PARSER:1",
        "Cap:PROMPT_SUPPORT:1",
        "Cap:HOST_ACTION_COMMANDS:1",
        "Cap:SDCARD:1",
        "Cap:SD_WRITE:1",
        "Cap:LONG_FILENAME:1",
        "Cap:EXTENDED_M20:1",
        "Cap:AUTOLEVEL:1",
        "Cap:BABYSTEPPING:1",
        "Cap:ARCS:1",
        "Cap:CHAMBER_TEMPERATURE:0",
    ] {
        assert!(out.contains(&cap.to_string()), "missing {cap}");
    }
    assert_eq!(out.last().map(String::as_str), Some("ok"));

    let mut r = Rig::new(SimConfig {
        auto_report_temps: false,
        machine_name: "Bench".into(),
        ..cfg()
    });
    r.take();
    let out = r.ok("M115");
    assert!(out[0].contains("MACHINE_TYPE:Bench EXTRUDER_COUNT:1"));
    assert!(out.contains(&"Cap:AUTOREPORT_TEMP:0".to_string()));
    assert_eq!(
        r.ok("M155 S1"),
        vec!["echo:Unknown command: \"M155 S1\"", "ok"]
    );
}

#[test]
fn settings_round_trip_through_eeprom() {
    let mut r = Rig::booted();
    r.ok("M92 X100");
    r.ok("M301 P30 I2 D100");
    let out = r.ok("M503");
    assert!(out.contains(&"echo:  M92 X100.00 Y80.00 Z400.00 E93.00".to_string()));
    assert!(out.contains(&"echo:  M301 P30.00 I2.00 D100.00".to_string()));
    let stored = r.ok("M500");
    assert!(
        stored[0].starts_with("echo:Settings Stored ("),
        "{stored:?}"
    );
    r.ok("M92 X50");
    let loaded = r.ok("M501");
    assert!(loaded[0].starts_with("echo:V86 stored settings retrieved"));
    assert!(loaded.contains(&"echo:  M92 X100.00 Y80.00 Z400.00 E93.00".to_string()));
    // Survives a reset.
    r.m.reset();
    r.run(0.5);
    let banner = r.take();
    assert_eq!(banner[0], "start");
    assert!(banner.contains(&"echo:  M92 X100.00 Y80.00 Z400.00 E93.00".to_string()));
    assert_eq!(
        r.ok("M502"),
        vec!["echo:Hardcoded Default Settings Loaded", "ok"]
    );
    let out = r.ok("M503 S0");
    assert!(out.contains(&"  M92 X80.00 Y80.00 Z400.00 E93.00".to_string()));
    assert!(!out.iter().any(|l| l.starts_with("echo:")));
}

#[test]
fn overrides_report() {
    let mut r = Rig::booted();
    assert_eq!(r.ok("M220"), vec!["FR:100%", "ok"]);
    r.ok("M220 S150");
    assert_eq!(r.ok("M220"), vec!["FR:150%", "ok"]);
    assert_eq!(r.ok("M221"), vec!["echo:E0 Flow: 100%", "ok"]);
    r.ok("M221 S95");
    assert_eq!(r.ok("M221"), vec!["echo:E0 Flow: 95%", "ok"]);
    let s = r.m.snapshot();
    assert_eq!((s.feedrate_pct, s.flow_pct), (150.0, 95.0));
    r.ok("M290 Z0.05");
    r.ok("M290 Z0.05");
    assert_eq!(r.ok("M290"), vec!["echo:Babystep Z0.10", "ok"]);
    r.ok("M106 S128");
    assert_eq!(r.m.snapshot().fan, 128);
    r.ok("M106");
    assert_eq!(r.m.snapshot().fan, 255);
    r.ok("M107");
    assert_eq!(r.m.snapshot().fan, 0);
}

#[test]
fn endstop_report() {
    let mut r = Rig::booted();
    assert_eq!(
        r.ok("M119"),
        vec![
            "Reporting endstop status",
            "x_min: open",
            "y_min: open",
            "z_min: open",
            "z_probe: open",
            "filament: open",
            "ok"
        ]
    );
    r.ok("G28");
    let out = r.ok("M119");
    assert_eq!(out[1], "x_min: TRIGGERED");
}

#[test]
fn bed_leveling_grid() {
    let mut r = Rig::booted();
    let out = r.ok("M420 V");
    assert_eq!(
        out,
        vec!["echo:Bed Leveling OFF", "echo:Fade Height 10.00", "ok"]
    );
    let out = r.ok("M420 S1");
    assert_eq!(out[0], "Error:Failed to enable Bed Leveling");
    let out = r.ok("G29");
    let grid = out
        .iter()
        .position(|l| l == "Bilinear Leveling Grid:")
        .unwrap_or_else(|| panic!("{out:?}"));
    assert_eq!(out[grid + 1], "      0      1      2      3      4");
    assert_eq!(out[grid + 2..grid + 7].len(), 5);
    assert!(out[grid + 2].starts_with(" 0 "));
    let out = r.ok("M420 V");
    assert_eq!(out[0], "Bilinear Leveling Grid:");
    assert!(out.contains(&"echo:Bed Leveling ON".to_string()));
    let out = r.ok("M503");
    assert!(out.contains(&"echo:  M420 S1 Z10.00 ; Leveling ON".to_string()));
}

#[test]
fn heating_wait_reports_and_busy() {
    let mut r = Rig::booted();
    r.send("M109 S200");
    let start = r.m.now;
    while !r.out.iter().any(|l| l == "ok") && r.m.now - start < 400.0 {
        r.run(0.5);
    }
    let t = r.m.now - start;
    assert!((50.0..110.0).contains(&t), "M109 took {t}s");
    let out = r.take();
    let waits: Vec<&String> = out.iter().filter(|l| l.starts_with(" T:")).collect();
    assert!(waits.len() as f64 > t * 0.8, "{} wait lines", waits.len());
    assert!(waits[0].ends_with(" W:?"), "{}", waits[0]);
    assert!(waits[0].contains(" /200.00 B:"), "{}", waits[0]);
    assert!(
        waits.iter().any(|l| l.ends_with(" W:5")),
        "{:?}",
        &waits[waits.len().saturating_sub(20)..]
    );
    assert!(out.iter().any(|l| l == "echo:busy: processing"));
    // Marlin's PID dips a little after the bang-bang heat-up overshoot.
    let temp = r.m.snapshot().hotend_temps[0];
    assert!((temp - 200.0).abs() < 5.0, "{temp}");
    // Already hot and S lower than current: no wait.
    assert_eq!(
        r.cmd("M109 S150", 0.1).last().map(String::as_str),
        Some("ok")
    );
}

#[test]
fn m108_breaks_heating_wait() {
    let mut r = Rig::booted();
    r.send("M190 S90");
    r.run(5.0);
    r.send("M108");
    r.run(0.1);
    let out = r.take();
    assert_eq!(out.iter().filter(|l| *l == "ok").count(), 2, "{out:?}");
    assert!(r.m.snapshot().bed_target == 90.0);
}

#[test]
fn quickstop_flushes_planner() {
    let mut r = Rig::booted();
    r.ok("G28");
    r.ok("G1 X200 F600");
    r.run(1.0);
    r.send("M410");
    let s = r.m.snapshot();
    assert_eq!(s.planner_queue, 0);
    assert!(
        s.position[0] > 1.0 && s.position[0] < 50.0,
        "{:?}",
        s.position
    );
}

#[test]
fn kill_halts_until_reset() {
    let mut r = Rig::booted();
    r.ok("M104 S200");
    r.send("M112");
    let out = r.take();
    assert_eq!(out, vec!["Error:Printer halted. kill() called!"]);
    assert!(r.m.snapshot().halted);
    assert_eq!(r.m.snapshot().hotend_targets, vec![0.0]);
    r.send("M105");
    r.run(5.0);
    assert!(r.take().is_empty());
    r.m.reset();
    r.run(0.5);
    let out = r.take();
    assert_eq!(out[0], "start");
    assert!(!r.m.snapshot().halted);
    assert_eq!(r.m.snapshot().last_line_number, 0);
    assert!(r.ok("M105")[0].starts_with("ok T:"));
}

#[test]
fn autoreports() {
    let mut r = Rig::booted();
    r.ok("M155 S1");
    r.ok("M154 S2");
    r.run(4.05);
    let out = r.take();
    let temps = out.iter().filter(|l| l.starts_with(" T:")).count();
    let pos = out.iter().filter(|l| l.starts_with("X:")).count();
    assert_eq!((temps, pos), (4, 2), "{out:?}");
    r.ok("M155 S0");
    r.ok("M154 S0");
    r.run(3.0);
    assert!(r.take().is_empty());
}

#[test]
fn autoreport_commands_without_a_value_change_nothing() {
    // Marlin only changes the interval for seenval('S').
    let mut r = Rig::booted();
    r.ok("M155 S1");
    r.ok("M154 S1");
    r.ok("M155");
    r.ok("M154 S");
    r.run(2.05);
    let out = r.take();
    let temps = out.iter().filter(|l| l.starts_with(" T:")).count();
    let pos = out.iter().filter(|l| l.starts_with("X:")).count();
    assert_eq!((temps, pos), (2, 2), "{out:?}");
    r.ok("M27 S1");
    assert_eq!(r.ok("M27 S"), vec!["Not SD printing", "ok"]);
    r.ok("M27");
    r.run(1.05);
    assert!(r.take().contains(&"Not SD printing".to_string()));
}

#[test]
fn dwell_and_sync() {
    let mut r = Rig::booted();
    let start = r.m.now;
    r.ok("G4 P1500");
    assert!((r.m.now - start - 1.5).abs() < 0.11);
    let start = r.m.now;
    r.ok("G4 S3");
    assert!((r.m.now - start - 3.0).abs() < 0.11);
    let out = r.take();
    assert!(out.is_empty());
}

#[test]
fn arcs_take_arc_length_time() {
    let mut r = Rig::booted();
    r.ok("G28");
    r.ok("G1 X100 Y50 F6000");
    r.ok("M400");
    let start = r.m.now;
    // Half circle of radius 50: 157 mm at 100 mm/s.
    r.ok("G2 X100 Y150 I0 J50 F6000");
    r.ok("M400");
    let t = r.m.now - start;
    assert!((1.4..2.2).contains(&t), "arc took {t}");
    let out = r.ok("M114");
    assert!(out[0].starts_with("X:100.00 Y:150.00"), "{out:?}");
    assert_eq!(r.ok("G2 X1 Y1"), vec!["Error:G2/G3 bad parameters", "ok"]);
}

#[test]
fn full_circle_arcs_despite_rounding() {
    // Recomputing the end point from the centre is off by an ulp for these
    // starts, so the angle is +-1e-17 rather than 0. Marlin compares the end
    // points (NEAR) and still draws the full circle.
    for (code, i, j) in [("G2", 0.2, 1.7), ("G3", 0.3, -5.1)] {
        let mut r = Rig::booted();
        r.ok("G28");
        r.ok("G1 X0.1 Y0.1 F6000");
        r.ok("M400");
        let start = r.m.now;
        r.ok(&format!("{code} X0.1 Y0.1 I{i} J{j} F3000"));
        r.ok("M400");
        let t = r.m.now - start;
        let circumference = std::f64::consts::TAU * f64::hypot(i, j);
        assert!(
            t > circumference / 50.0,
            "{code} I{i} J{j}: {circumference:.1} mm circle took {t}s"
        );
        assert!(r.ok("M114")[0].starts_with("X:0.10 Y:0.10"));
    }
    // A target on the start ray at another radius is no arc at all:
    // Marlin skips it and stays put.
    let mut r = Rig::booted();
    r.ok("G28");
    r.ok("G1 X10 Y10 F6000");
    r.ok("G2 X5 Y10 I10 J0");
    assert!(r.ok("M114")[0].starts_with("X:10.00 Y:10.00"));
}

#[test]
fn small_arcs_use_minimum_segments() {
    // Small arcs still get Marlin's minimum segment count
    // (MIN_CIRCLE_SEGMENTS), so a 1.8 mm circle is not a no-op and a
    // 1.6 mm semicircle is not a 1 mm chord.
    let mut r = Rig::booted();
    r.ok("G28");
    r.ok("G1 X0.1 Y0.1 F6000");
    r.ok("M400");
    let start = r.m.now;
    r.ok("G2 X0.1 Y0.1 I0.2 J0.2 F600");
    r.ok("M400");
    let t = r.m.now - start;
    assert!(t > 0.17, "1.78 mm circle at 10 mm/s took {t}s");
    let start = r.m.now;
    r.ok("G2 X1.1 Y0.1 I0.5 J0 F60");
    r.ok("M400");
    let t = r.m.now - start;
    assert!(t > 1.5, "1.57 mm semicircle at 1 mm/s took {t}s");
}

#[test]
fn cold_extrusion_prevented() {
    let mut r = Rig::booted();
    r.ok("G28");
    let out = r.ok("G1 X10 E5 F1200");
    assert_eq!(out, vec!["echo: cold extrusion prevented", "ok"]);
    assert_eq!(
        r.ok("M302"),
        vec!["echo:Cold extrudes are disabled (min temp 170C)", "ok"]
    );
    r.ok("M302 P1");
    assert_eq!(r.ok("G1 X20 E10"), vec!["ok"]);
}

#[test]
fn sd_listing_write_and_print() {
    let mut r = Rig::booted();
    r.ok("M302 P1");
    let out = r.ok("M20 L");
    assert_eq!(out[0], "Begin file list");
    assert!(out.contains(&"CALIBR~1.GCO 48213 calibration_cube.gcode".to_string()));
    assert_eq!(out[out.len() - 2], "End file list");
    let out = r.ok("M20");
    assert!(out.contains(&"CALIBR~1.GCO 48213".to_string()));

    // Upload a file with line numbers.
    r.ok("M110 N0");
    let out = r.ok(&frame(1, "M28 test.gco"));
    assert_eq!(out, vec!["Writing to file: test.gco", "ok"]);
    let lines = ["G28", "G1 X10 Y10 F3000", "G1 X20 Y10 E1", "M400"];
    for (i, l) in lines.iter().enumerate() {
        assert_eq!(r.ok(&frame(i as i64 + 2, l)), vec!["ok"]);
    }
    // Unnumbered lines are rejected while writing (Marlin behaviour).
    let out = r.ok("M105");
    assert_eq!(out[0], "Error:No Checksum with line number, Last Line: 5");
    assert_eq!(r.ok(&frame(6, "M29")), vec!["Done saving file.", "ok"]);
    let out = r.ok("M20 L");
    assert!(out.contains(&"TEST.GCO 40 test.gco".to_string()), "{out:?}");

    // Print it.
    assert_eq!(
        r.ok("M23 test.gco"),
        vec!["File opened: test.gco Size: 40", "File selected", "ok"]
    );
    assert_eq!(r.ok("M27"), vec!["SD printing byte 0/40", "ok"]);
    r.ok("M24");
    assert!(r.m.snapshot().sd_printing);
    let mut waited = 0.0;
    while !r.out.iter().any(|l| l == "Done printing file") && waited < 120.0 {
        r.run(0.5);
        waited += 0.5;
    }
    assert!(r.take().contains(&"Done printing file".to_string()));
    assert!(!r.m.snapshot().sd_printing);
    assert_eq!(r.ok("M27"), vec!["Not SD printing", "ok"]);
    let out = r.ok("M114");
    assert!(out[0].starts_with("X:20.00 Y:10.00"), "{out:?}");
    // SD commands are not host commands.
    let received = r.m.take_received();
    assert!(received.contains(&"G1 X20 Y10 E1".to_string()));
    assert_eq!(received.iter().filter(|c| *c == "G1 X20 Y10 E1").count(), 1);
    let stats = r.ok("M78");
    assert!(
        stats[0].starts_with("Stats: Prints: 1, Finished: 1, Failed: 0"),
        "{stats:?}"
    );
    assert_eq!(r.ok("M30 test.gco"), vec!["File deleted:test.gco", "ok"]);
    assert_eq!(
        r.ok("M23 test.gco"),
        vec!["open failed, File: test.gco.", "ok"]
    );
}

#[test]
fn emergency_parser_is_off_while_writing_to_sd() {
    // Marlin disables the emergency parser between M28 and M29, so an
    // uploaded file may contain M112 / M108 / M410 without side effects.
    let mut r = Rig::booted();
    r.ok("M110 N0");
    r.ok(&frame(1, "M28 estop.gco"));
    for (i, l) in ["M112", "M108", "M410", "M876 S0"].iter().enumerate() {
        assert_eq!(r.ok(&frame(i as i64 + 2, l)), vec!["ok"]);
    }
    assert!(!r.m.snapshot().halted);
    assert_eq!(r.ok(&frame(6, "M29")), vec!["Done saving file.", "ok"]);
    let index = r.m.sd.find("estop.gco").expect("file saved");
    assert_eq!(r.m.sd.data(index), b"M112\nM108\nM410\nM876 S0\n");
    // Enabled again after M29.
    r.send("M112");
    assert!(r.m.snapshot().halted);
}

#[test]
fn sd_status_auto_report() {
    let mut r = Rig::booted();
    r.ok("M23 calibration_cube.gcode");
    r.ok("M27 S2");
    r.ok("M24");
    r.run(4.05);
    let out = r.take();
    let reports: Vec<&String> = out
        .iter()
        .filter(|l| l.starts_with("SD printing byte "))
        .collect();
    assert_eq!(reports.len(), 2, "{out:?}");
    assert!(reports[0].ends_with("/48213"));
    r.ok("M27 S0");
    r.run(3.0);
    assert!(!r.take().iter().any(|l| l.starts_with("SD printing byte ")));
    // Queued behind the file's heating commands, so look at the end.
    let out = r.ok("M27 C");
    assert_eq!(
        &out[out.len() - 2..],
        &["Current file: CALIBR~1.GCO calibration_cube.gcode", "ok"]
    );
}

#[test]
fn sd_pause_resume_prompt_and_abort() {
    let mut r = Rig::booted();
    r.ok("M302 P1");
    r.ok("G28");
    r.ok("M110 N0");
    r.ok(&frame(1, "M28 slow.gco"));
    for i in 0..200 {
        let x = if i % 2 == 0 { 100 } else { 10 };
        r.ok(&frame(i + 2, &format!("G1 X{x} F3000")));
    }
    r.ok(&frame(202, "M29"));
    r.ok("M23 slow.gco");
    r.ok("M24");
    r.run(2.0);
    assert!(r.m.snapshot().sd_printing);
    let out = r.ok("M25");
    assert!(
        out.contains(&"//action:prompt_begin Pause SD".to_string()),
        "{out:?}"
    );
    assert!(out.contains(&"//action:prompt_button Resume".to_string()));
    assert!(!r.m.snapshot().sd_printing);
    // Commands already in the buffer still run (and wait for the planner),
    // but no new lines are read while paused.
    let progress = |out: Vec<String>| out.into_iter().find(|l| l.starts_with("SD printing byte"));
    let paused_at = progress(r.ok("M27"));
    assert!(paused_at.is_some());
    r.run(30.0);
    r.take();
    assert_eq!(progress(r.ok("M27")), paused_at);
    // Answering the prompt resumes the print (Marlin injects M24).
    r.send("M876 S0");
    r.run(1.0);
    assert!(r.m.snapshot().sd_printing);
    r.take();
    // Pausing again and resuming from the host closes the prompt.
    let out = r.ok("M25");
    assert!(out.contains(&"//action:prompt_show".to_string()), "{out:?}");
    let out = r.ok("M24");
    assert!(out.contains(&"//action:prompt_end".to_string()), "{out:?}");
    assert!(r.m.snapshot().sd_printing);
    r.ok("M106 S255");
    r.ok("M524");
    r.run(0.5);
    let s = r.m.snapshot();
    assert!(!s.sd_printing);
    assert_eq!(s.planner_queue, 0);
    assert_eq!(s.fan, 0);
    assert_eq!(r.ok("M27"), vec!["Not SD printing", "ok"]);
    let stats = r.ok("M78");
    assert!(stats[0].ends_with("Failed: 1"), "{stats:?}");
}

#[test]
fn m600_prompt_flow() {
    let mut r = Rig::booted();
    r.ok("G28");
    r.send("M600");
    r.run(1.0);
    let out = r.take();
    assert_eq!(out[0], "//action:paused");
    r.run(20.0);
    let out = r.take();
    let begin = out
        .iter()
        .position(|l| l == "//action:prompt_begin Nozzle Parked")
        .unwrap_or_else(|| panic!("{out:?}"));
    assert_eq!(out[begin - 1], "//action:prompt_end");
    assert_eq!(out[begin + 1], "//action:prompt_button Continue");
    assert_eq!(out[begin + 2], "//action:prompt_show");
    r.run(5.0);
    let out = r.take();
    assert!(
        out.contains(&"echo:busy: paused for user".to_string()),
        "{out:?}"
    );
    assert!(!out.contains(&"ok".to_string()));
    r.send("M876 S0");
    r.run(30.0);
    let out = r.take();
    assert!(out.contains(&"//action:resumed".to_string()), "{out:?}");
    assert_eq!(out.iter().filter(|l| *l == "ok").count(), 2, "{out:?}");
    let pos = r.ok("M114");
    assert!(pos[0].starts_with("X:0.00 Y:0.00 Z:0.00"), "{pos:?}");
}

#[test]
fn m876_without_a_response_keeps_the_prompt_open() {
    // OctoPrint sends "M876 P1" to announce prompt support; Marlin's
    // emergency parser only answers prompts for "M876 S<digit>".
    let mut r = Rig::booted();
    r.ok("G28");
    r.send("M600");
    r.run(30.0);
    assert!(r.take().contains(&"//action:prompt_show".to_string()));
    r.send("M876 P1");
    r.send("M876");
    r.run(30.0);
    let out = r.take();
    assert!(!out.contains(&"//action:resumed".to_string()), "{out:?}");
    assert!(out.contains(&"echo:busy: paused for user".to_string()));
    r.send("M876 S0");
    r.run(30.0);
    let out = r.take();
    assert!(out.contains(&"//action:resumed".to_string()), "{out:?}");
    // M600 and the three M876 lines are all acknowledged.
    assert_eq!(out.iter().filter(|l| *l == "ok").count(), 4, "{out:?}");
}

#[test]
fn runout_triggers_filament_change() {
    let mut r = Rig::booted();
    r.m.filament_runout();
    r.m.advance(0.0);
    r.collect();
    let out = r.take();
    assert_eq!(out[0], "//action:out_of_filament T0");
    assert_eq!(out[1], "//action:paused");
    assert!(r.m.runout);
    r.run(30.0);
    r.send("M108");
    r.run(30.0);
    let out = r.take();
    assert!(out.contains(&"//action:resumed".to_string()), "{out:?}");
    assert_eq!(r.ok("M119")[5], "filament: open");
}

#[test]
fn autotune_hotend() {
    let mut r = Rig::booted();
    let out = r.ok("M303 E0 S200 C5 U1");
    assert_eq!(out[0], "PID Autotune start");
    assert!(out.iter().any(|l| l.starts_with(" bias: ")), "{out:?}");
    assert!(
        out.iter()
            .any(|l| l.contains(" Ku: ") && l.contains(" Tu: "))
    );
    assert!(out.contains(&" Classic PID ".to_string()), "{out:?}");
    assert!(out.iter().any(|l| l.starts_with(" Kp: ")));
    assert!(out.contains(
        &"PID Autotune finished! Put the last Kp, Ki and Kd constants from below into Configuration.h"
            .to_string()
    ));
    let kp_line = out
        .iter()
        .find(|l| l.starts_with("#define DEFAULT_Kp "))
        .unwrap_or_else(|| panic!("{out:?}"));
    let kp: f64 = kp_line["#define DEFAULT_Kp ".len()..]
        .parse()
        .unwrap_or(0.0);
    assert!(kp > 5.0 && kp < 80.0, "{kp}");
    let report = r.ok("M301");
    assert!(
        report[1].starts_with(&format!("echo:  M301 P{kp:.2} ")),
        "{report:?}"
    );
    assert_eq!(r.m.snapshot().hotend_targets, vec![0.0]);
    assert_eq!(
        r.ok("M303 E7 S200"),
        vec!["PID Autotune failed! Bad heater id", "ok"]
    );
    assert_eq!(
        r.ok("M303 E0 S400"),
        vec!["PID Autotune failed! Temperature too high", "ok"]
    );
}

#[test]
fn print_job_counter() {
    let mut r = Rig::booted();
    r.ok("M75");
    r.run(75.2);
    let out = r.ok("M31");
    assert_eq!(out[0], "echo:Print time: 1m 15s");
    r.ok("M77");
    let out = r.ok("M78");
    assert_eq!(out[0], "Stats: Prints: 1, Finished: 1, Failed: 0");
    assert_eq!(out[1], "Stats: Total time: 1m 15s, Longest job: 1m 15s");
    assert_eq!(out[2], "Stats: Filament used: 0.00m");
}

#[test]
fn m118_and_inject() {
    let mut r = Rig::booted();
    assert_eq!(r.ok("M118 hello world"), vec!["hello world", "ok"]);
    assert_eq!(r.ok("M118 E1 hi"), vec!["echo:hi", "ok"]);
    assert_eq!(r.ok("M118 A1 action:pause"), vec!["//action:pause", "ok"]);
    assert_eq!(r.ok("M117 Printing..."), vec!["ok"]);
    r.m.inject_line("//action:cancel");
    r.collect();
    assert_eq!(r.take(), vec!["//action:cancel"]);
}

#[test]
fn garbage_never_panics() {
    let mut r = Rig::booted();
    let mut rng = crate::rng::Rng::new(99, 99);
    let alphabet = b"GMTNXYZEFSPIJR0123456789.-* ;\n\r()!#~\xff\x00\xc3";
    for _ in 0..200 {
        let len = (rng.next_u64() % 60) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|_| alphabet[(rng.next_u64() % alphabet.len() as u64) as usize])
            .collect();
        r.m.on_bytes(&bytes);
        r.m.advance(0.3);
    }
    for line in [
        "G1 X1e308 Y-1e308",
        "G2 R0",
        "G3 I1e300 J1e300 P999999",
        "M104 S999999",
        "M106 P999 S-5",
        "G4 S1e300",
        "M27 S-1",
        "M155 S1e30",
        "M23 ",
        "M28 ",
        "M30 /",
        "M906 T99 E5",
        "N99999999999999999999 G1*1",
        "M110 N-5",
        "T65535",
    ] {
        r.m.on_bytes(format!("{line}\n").as_bytes());
        r.m.advance(0.5);
    }
    // Still responsive afterwards (unblock any pending dwell or wait).
    r.m.on_bytes(b"M108\nM410\n");
    r.m.reset();
    r.run(0.5);
    r.take();
    assert!(r.ok("M105")[0].starts_with("ok T:"));
}

#[test]
fn m109_r_waits_for_cooling() {
    let mut r = Rig::booted();
    r.ok("M109 S200");
    let start = r.m.now;
    let out = r.ok("M109 R150");
    let t = r.m.now - start;
    assert!(t > 20.0, "cooling wait took {t}s");
    let temp = r.m.snapshot().hotend_temps[0];
    assert!((temp - 150.0).abs() < 3.0, "{temp}");
    assert!(out.iter().any(|l| l.contains(" /150.00 ")));
    // R0 cannot be reached: the cooling-slope check ends the wait.
    let out = r.cmd("M109 R0", 2000.0);
    assert_eq!(out.last().map(String::as_str), Some("ok"));
}

#[test]
fn temperature_parameters_follow_marlin() {
    let mut r = Rig::booted();
    // S wins over R; M104 / M140 ignore R entirely.
    r.ok("M104 S200 R180");
    assert_eq!(r.m.snapshot().hotend_targets, vec![200.0]);
    r.ok("M104 R150");
    assert_eq!(r.m.snapshot().hotend_targets, vec![200.0]);
    r.ok("M140 S60 R40");
    r.ok("M140 R50");
    assert_eq!(r.m.snapshot().bed_target, 60.0);
    r.ok("M104 S0");
    r.ok("M140 S0");
    // "M109 S50 R180": S selects "heat only", so with the nozzle already
    // above 50 C there is no wait (R would wait for cooling to 180).
    r.ok("M109 S190");
    let start = r.m.now;
    r.ok("M109 S50 R180");
    assert_eq!(r.m.snapshot().hotend_targets, vec![50.0]);
    assert!(r.m.now - start < 1.0, "waited {}s", r.m.now - start);
}

#[test]
fn print_timer_autostart_rules() {
    // PRINTJOB_TIMER_AUTOSTART: only commands that set a temperature touch
    // the timer, and M190 may start it but never stops it.
    let mut r = Rig::booted();
    r.ok("M75");
    r.ok("M104");
    r.ok("M104 T0");
    r.ok("M190 S0");
    let stats = r.ok("M78");
    assert_eq!(stats[0], "Stats: Prints: 1, Finished: 0, Failed: 0");
    r.ok("M140 S0");
    let stats = r.ok("M78");
    assert_eq!(stats[0], "Stats: Prints: 1, Finished: 1, Failed: 0");
}

#[test]
fn chamber_heater() {
    let mut r = Rig::new(SimConfig {
        heated_chamber: true,
        ..cfg()
    });
    r.take();
    assert!(
        r.ok("M115")
            .contains(&"Cap:CHAMBER_TEMPERATURE:1".to_string())
    );
    let out = r.ok("M191 S35");
    assert!(
        out.iter()
            .any(|l| l.contains(" C:") && l.contains(" /35.00")),
        "{out:?}"
    );
    let report = r.ok("M105");
    assert!(report[0].contains(" C:3"), "{report:?}");
    let mut plain = Rig::booted();
    assert_eq!(
        plain.ok("M141 S40"),
        vec!["echo:Unknown command: \"M141 S40\"", "ok"]
    );
}

#[test]
fn sd_abort_and_autotune_turn_every_heater_off() {
    // Marlin's disable_all_heaters() includes the chamber.
    let mut r = Rig::new(SimConfig {
        heated_chamber: true,
        ..cfg()
    });
    r.take();
    r.ok("M141 S40");
    r.ok("M23 calibration_cube.gcode");
    r.ok("M24");
    r.run(1.0);
    r.ok("M524");
    let report = r.ok("M105");
    assert!(
        report[0].contains(" C:") && report[0].contains(" /0.0 @:"),
        "{report:?}"
    );
    assert!(!report[0].contains("/40.0"), "{report:?}");
    r.ok("M141 S40");
    r.ok("M303 E0 S200 C3");
    let report = r.ok("M105");
    assert!(!report[0].contains("/40.0"), "{report:?}");
}

#[test]
fn no_heated_bed() {
    let mut r = Rig::new(SimConfig {
        heated_bed: false,
        ..cfg()
    });
    r.take();
    let out = r.ok("M105");
    assert!(!out[0].contains("B:"), "{out:?}");
    assert_eq!(
        r.ok("M190 S60"),
        vec!["echo:Unknown command: \"M190 S60\"", "ok"]
    );
    assert!(!r.ok("M503").iter().any(|l| l.contains("M304")));
}

#[test]
fn firmware_retraction_and_offsets() {
    let mut r = Rig::booted();
    r.ok("M302 P1");
    r.ok("G28");
    r.ok("G10");
    assert!(r.ok("M114")[0].contains("E:-3.00"));
    r.ok("G10");
    assert!(
        r.ok("M114")[0].contains("E:-3.00"),
        "double G10 retracts once"
    );
    r.ok("G11");
    assert!(r.ok("M114")[0].contains("E:0.00"));
    r.ok("M206 X5");
    assert!(r.ok("M114")[0].starts_with("X:5.00"));
    r.ok("G1 X10");
    let out = r.ok("M114");
    assert!(
        out[0].starts_with("X:10.00") && out[0].contains("Count X:400"),
        "{out:?}"
    );
    r.ok("M206 X0");
    r.ok("G20");
    r.ok("G1 X1");
    assert!(r.ok("M114")[0].starts_with("X:25.40"));
    r.ok("G21");
    // M83 relative E survives G91/G90 only until G90 resets it.
    r.ok("M83");
    r.ok("G1 E1");
    r.ok("G1 E1");
    assert!(r.ok("M114")[0].contains("E:2.00"));
    r.ok("G90");
    r.ok("G1 E1");
    assert!(r.ok("M114")[0].contains("E:1.00"));
}

#[test]
fn soft_endstops_clamp_homed_axes() {
    let mut r = Rig::booted();
    r.ok("G28");
    r.ok("G1 X500 Y-20 Z999");
    assert!(r.ok("M114")[0].starts_with("X:220.00 Y:0.00 Z:250.00"));
    let out = r.ok("M211");
    assert_eq!(out[0], "echo:; Soft endstops:");
    assert_eq!(out[1], "echo:  M211 S1 ; ON");
    r.ok("M211 S0");
    r.ok("G1 X500");
    assert!(r.ok("M114")[0].starts_with("X:500.00"));
}

#[test]
fn multi_extruder_targets_and_flow() {
    let mut r = Rig::new(SimConfig {
        extruders: 2,
        ..cfg()
    });
    r.take();
    r.ok("M104 T1 S180");
    assert_eq!(r.m.snapshot().hotend_targets, vec![0.0, 180.0]);
    // get_target_extruder_from_command(): "M<code> Invalid extruder <e>",
    // and T is read as a byte (T-1 is T0).
    assert_eq!(
        r.ok("M104 T2 S180"),
        vec!["echo:M104 Invalid extruder 2", "ok"]
    );
    assert_eq!(r.ok("M105 T7")[0], "echo:M105 Invalid extruder 7");
    assert_eq!(r.ok("M221 T9"), vec!["echo:M221 Invalid extruder 9", "ok"]);
    r.ok("M104 T-1 S170");
    assert_eq!(r.m.snapshot().hotend_targets, vec![170.0, 180.0]);
    r.ok("M221 T1 S80");
    assert_eq!(r.ok("M221 T1"), vec!["echo:E1 Flow: 80%", "ok"]);
    let out = r.ok("M115");
    assert!(out[0].contains("EXTRUDER_COUNT:2"));
    let out = r.ok("M503");
    assert!(out.contains(&"echo:  M906 T1 E800".to_string()));
}

#[test]
fn time_scale_is_honoured_by_the_machine_clock() {
    // The machine only sees simulated seconds; the link multiplies real
    // time by the scale. Advancing 10 simulated seconds runs a 10 s dwell.
    let mut r = Rig::booted();
    r.send("G4 S10");
    r.run(9.9);
    let out = r.take();
    assert!(!out.contains(&"ok".to_string()));
    assert_eq!(out.len(), 4, "keepalive every 2 s: {out:?}");
    r.run(0.2);
    assert_eq!(r.take(), vec!["ok"]);
}
