//! Manual commands, convenience controls, the console, temperatures and
//! heat waits against the simulator.

mod common;

use std::time::Duration;

use common::*;
use sprint_host::sprint_proto::SettingCommand;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{Axes, Axis, ConnectOptions, ConsoleKind, Event, Heater, Printer};

/// Run `f`, wait until the link is idle and return what the simulator
/// executed meanwhile.
fn executed(printer: &Printer, f: impl FnOnce(&Printer)) -> Vec<String> {
    let control = printer.sim_control().expect("simulator");
    wait_idle(printer);
    control.clear_received();
    f(printer);
    wait_idle(printer);
    control.received_commands()
}

#[test]
fn manual_commands_are_cleaned_numbered_and_logged() {
    let printer = connect(fast_sim(20.0), options());
    let before = last_seq(&printer);
    let got = executed(&printer, |p| {
        p.send("G28 ; home\n\n  g1 x10 y20 f3000\r\nM114\n; only a comment");
    });
    assert_eq!(got, ["G28", "G1 X10 Y20 F3000", "M114"]);
    wait_for(&printer, "position", |s| {
        s.position
            .as_ref()
            .is_some_and(|p| p.x == 10.0 && p.y == 20.0)
    });
    let entries = printer.console_since(before, usize::MAX);
    let sent: Vec<_> = entries
        .iter()
        .filter(|e| e.kind == ConsoleKind::Sent)
        .collect();
    assert_eq!(
        sent.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        ["G28", "G1 X10 Y20 F3000", "M114"]
    );
    // Consecutive line numbers.
    let numbers: Vec<u32> = sent.iter().map(|e| e.line.expect("numbered")).collect();
    assert!(numbers.windows(2).all(|w| w[1] == w[0] + 1), "{numbers:?}");
    assert!(
        entries
            .iter()
            .any(|e| e.kind == ConsoleKind::Received && e.text == "ok" && !e.auto)
    );
    assert!(entries.iter().all(|e| e.seq > before));
    // Sequence numbers increase by one.
    assert!(entries.windows(2).all(|w| w[1].seq == w[0].seq + 1));
}

#[test]
fn convenience_controls_send_the_documented_gcode() {
    let printer = connect(fast_sim(20.0), options());
    let got = executed(&printer, |p| {
        p.home(Axes::XY);
        p.home(Axes::ALL);
        p.jog(Axis::X, 5.0, 3000.0);
        p.jog(Axis::Z, -0.5, 600.0);
        p.move_to(Some(1.5), None, Some(5.0), 600.0);
        p.move_to(None, None, None, 600.0);
        p.set_temperature(Heater::Hotend(0), 200.0);
        p.set_temperature(Heater::Bed, 60.0);
        p.set_fan(0, 128);
        p.set_fan(0, 0);
        p.set_feedrate_override(120.0);
        p.set_flow(0, 95.0);
        p.babystep_z(0.05);
        p.select_tool(0);
        p.request_temperatures();
        p.request_firmware_info();
        p.request_endstops();
        p.request_stats();
        p.motors_off();
    });
    let want = [
        "G28 X Y",
        "G28",
        "G91",
        "G1 X5 F3000",
        "G90",
        "G91",
        "G1 Z-0.5 F600",
        "G90",
        "G90",
        "G1 X1.5 Z5 F600",
        "M104 T0 S200",
        "M140 S60",
        "M106 S128",
        "M107",
        "M220 S120",
        "M221 T0 S95",
        "M290 Z0.05",
        "T0",
        "M105",
        "M115",
        "M119",
        "M78",
        "M84",
    ];
    assert_eq!(got, want);
    let s = printer.snapshot();
    assert_eq!(s.feedrate_pct, 120.0);
    assert_eq!(s.flow_pct[0], 95.0);
    assert_eq!(s.fan_speeds[0], 0);
    assert!(!s.endstops.is_empty(), "M119 report collected");
    assert!(s.stats.is_some(), "M78 report collected");
}

#[test]
fn extrude_restores_the_extruder_mode() {
    let printer = connect(fast_sim(20.0), options());
    let got = executed(&printer, |p| p.extrude(0, 5.0, 300.0));
    assert_eq!(got, ["M83", "G1 E5 F300", "M82"]);
    // In relative mode nothing needs restoring.
    let got = executed(&printer, |p| {
        p.send("M83");
        p.extrude(0, -2.0, 1800.0);
    });
    assert_eq!(got, ["M83", "M83", "G1 E-2 F1800"]);
    // Jogging E extrudes.
    let got = executed(&printer, |p| p.jog(Axis::E, 1.0, 100.0));
    assert_eq!(got, ["M83", "G1 E1 F100"]);
}

#[test]
fn settings_can_be_applied_and_reloaded() {
    let printer = connect(fast_sim(20.0), options());
    let events = printer.subscribe();
    let mut steps = SettingCommand::parse("M92 X80 Y80 Z400 E101.5").unwrap();
    steps.set('E', 101.5);
    let got = executed(&printer, |p| p.apply_setting(&steps));
    assert_eq!(got, ["M92 X80 Y80 Z400 E101.5"]);
    // Optimistic update.
    assert_eq!(
        printer.with_state(|s| s.settings.value("M92", 'E')),
        Some(101.5)
    );
    // A refresh follows shortly after the last edit.
    wait_for(&printer, "the settings refresh", |_| {
        events
            .try_iter()
            .filter(|e| matches!(e, Event::SettingsUpdated))
            .count()
            > 0
    });
    let control = printer.sim_control().unwrap();
    wait_for(&printer, "M503 after the edit", |_| {
        control.received_commands().iter().any(|c| c == "M503")
    });
    wait_idle(&printer);
    assert_eq!(
        printer.with_state(|s| s.settings.value("M92", 'E')),
        Some(101.5)
    );
    let got = executed(&printer, |p| p.factory_reset_settings());
    assert_eq!(got, ["M502", "M503"]);
    assert_eq!(
        printer.with_state(|s| s.settings.value("M92", 'E')),
        Some(93.0)
    );
}

#[test]
fn auto_reported_temperatures_fill_state_history_and_events() {
    let printer = connect(fast_sim(40.0), options());
    let events = printer.subscribe();
    wait_for(&printer, "a few temperature samples", |s| {
        s.temp_history.len() >= 2
    });
    let s = printer.snapshot();
    let hotend = &s.temps.hotends[0];
    assert!((hotend.current - 22.0).abs() < 3.0, "{hotend:?}");
    assert_eq!(hotend.target, Some(0.0));
    assert!(s.temps.bed.is_some());
    let samples: Vec<f64> = s.temp_history.iter().map(|t| t.t).collect();
    assert!(
        samples.windows(2).all(|w| w[1] - w[0] >= 0.99),
        "{samples:?}"
    );
    assert!(
        events
            .iter()
            .take(200)
            .any(|e| matches!(e, Event::Temperatures(_)))
    );
    // Auto-reports are automatic traffic; nothing was polled.
    let entries = console(&printer);
    assert!(
        entries
            .iter()
            .any(|e| e.kind == ConsoleKind::Received && e.text.starts_with(" T:") && e.auto)
    );
    assert!(
        !entries
            .iter()
            .any(|e| e.kind == ConsoleKind::Sent && e.text == "M105")
    );
}

#[test]
fn without_auto_reports_temperatures_are_polled() {
    let sim = SimConfig {
        auto_report_temps: false,
        ..fast_sim(20.0)
    };
    let printer = connect(
        sim,
        ConnectOptions {
            temp_poll_secs: 0.05,
            ..options()
        },
    );
    wait_for(&printer, "polled temperatures", |s| {
        !s.temps.hotends.is_empty() && !s.temp_history.is_empty()
    });
    let entries = console(&printer);
    let polls: Vec<_> = entries
        .iter()
        .filter(|e| e.kind == ConsoleKind::Sent && e.text == "M105")
        .collect();
    assert!(!polls.is_empty());
    assert!(polls.iter().all(|e| e.auto), "polls are automatic traffic");
    assert!(
        entries
            .iter()
            .any(|e| e.kind == ConsoleKind::Received && e.text.starts_with("ok T:") && e.auto)
    );
}

#[test]
fn an_auto_report_request_the_firmware_cannot_honour_keeps_polling() {
    let sim = SimConfig {
        auto_report_temps: false,
        ..fast_sim(20.0)
    };
    let printer = connect(
        sim,
        ConnectOptions {
            temp_poll_secs: 0.05,
            ..options()
        },
    );
    assert!(!printer.with_state(|s| s.has_capability("AUTOREPORT_TEMP")));
    // The firmware answers "Unknown command" (and ok): it will not report.
    printer.send("M155 S1");
    wait_idle(&printer);
    let seq = last_seq(&printer);
    wait_for_within(
        &printer,
        "polls after the M155",
        Duration::from_secs(5),
        |_| {
            printer
                .console_since(seq, usize::MAX)
                .iter()
                .filter(|e| e.kind == ConsoleKind::Sent && e.text == "M105" && e.auto)
                .count()
                >= 3
        },
    );
}

#[test]
fn heat_wait_with_busy_messages_does_not_time_out_or_poll() {
    let sim = SimConfig {
        auto_report_temps: false,
        ..fast_sim(30.0)
    };
    let printer = connect(
        sim,
        ConnectOptions {
            // Shorter than the heat wait, even with the long-command
            // allowance: only busy/temperature reports keep it alive.
            ok_timeout_secs: 0.1,
            temp_poll_secs: 0.05,
            ..options()
        },
    );
    let control = printer.sim_control().unwrap();
    wait_idle(&printer);
    let start = std::time::Instant::now();
    control.clear_received();
    printer.send("M109 S200");
    wait_for(&printer, "busy during the heat wait", |s| s.busy.is_some());
    assert_eq!(
        printer.with_state(|s| s.busy.clone()).as_deref(),
        Some("processing")
    );
    wait_for(&printer, "the hotend to reach 200", |s| {
        s.busy.is_none()
            && s.temps.hotends.first().is_some_and(|h| h.current > 195.0)
            && s.link.in_flight == 0
    });
    let waited = start.elapsed();
    assert!(
        waited > Duration::from_millis(600),
        "{waited:?}: heat wait too short to prove anything"
    );
    let s = printer.snapshot();
    assert_eq!(s.link.timeouts, 0, "{:?}", tail(&printer, 30));
    // No M105 while M109 was running (polls resume afterwards).
    let got = control.received_commands();
    let m109 = got.iter().position(|c| c == "M109 S200").unwrap();
    assert!(!got[..m109].iter().any(|c| c == "M105"));
    let entries = console(&printer);
    let busy_at = entries
        .iter()
        .position(|e| e.text.contains("busy: processing"))
        .expect("busy line logged");
    let ok_at = entries
        .iter()
        .rposition(|e| e.kind == ConsoleKind::Received && e.text == "ok")
        .unwrap();
    assert!(busy_at < ok_at);
    let polls_during = entries[busy_at..ok_at]
        .iter()
        .filter(|e| e.kind == ConsoleKind::Sent && e.text == "M105")
        .count();
    assert_eq!(polls_during, 0);
}

#[test]
fn clear_console_keeps_sequence_numbers() {
    let printer = connect(fast_sim(20.0), options());
    let last = console(&printer).last().map(|e| e.seq).unwrap();
    printer.clear_console();
    assert!(console(&printer).is_empty());
    printer.send("M114");
    wait_idle(&printer);
    let entries = console(&printer);
    assert!(entries.first().unwrap().seq > last);
    let small = Printer::with_console_capacity(1);
    small.send("G28");
    small.send("G28");
    assert_eq!(console(&small).len(), 1);
}

#[test]
fn the_waker_is_called_and_rate_limited() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let printer = Printer::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    printer.set_waker(move || {
        c.fetch_add(1, Ordering::Relaxed);
    });
    printer
        .connect(
            sprint_host::ConnectTarget::Simulator(fast_sim(20.0)),
            options(),
        )
        .unwrap();
    let start = std::time::Instant::now();
    wait_for(&printer, "connected", |s| s.settings_loaded);
    for _ in 0..200 {
        printer.send("M114");
    }
    wait_idle(&printer);
    let elapsed = start.elapsed().as_secs_f64();
    let n = calls.load(Ordering::Relaxed) as f64;
    assert!(n >= 2.0, "waker never called");
    // At most one call per 16 ms (plus slack for the first call).
    assert!(n <= elapsed / 0.016 + 2.0, "{n} calls in {elapsed} s");
}

#[test]
fn an_idle_connection_does_not_keep_waking_the_ui() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Nothing reports or polls: only the connection clock moves.
    let sim = SimConfig {
        auto_report_temps: false,
        auto_report_position: false,
        ..fast_sim(20.0)
    };
    let printer = connect(
        sim,
        ConnectOptions {
            temp_poll_secs: 0.0,
            ..options()
        },
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    printer.set_waker(move || {
        c.fetch_add(1, Ordering::Relaxed);
    });
    // Let the 2 s lines-per-second window empty.
    std::thread::sleep(Duration::from_millis(2200));
    let before = calls.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(1000));
    let woken = calls.load(Ordering::Relaxed) - before;
    // The clock is worth one wake-up per second, not one per 20 ms tick.
    assert!(woken <= 2, "{woken} wake-ups in a second of idling");
    let secs = printer.with_state(|s| s.link.connected_secs);
    assert!(secs > 3.0, "the clock still runs: {secs}");
}

#[test]
fn a_manual_m110_renumbers_the_following_lines() {
    let printer = connect(fast_sim(20.0), options());
    let before = last_seq(&printer);
    let got = executed(&printer, |p| p.send("M110 N100\nG28\nM114"));
    assert_eq!(got, ["M110 N100", "G28", "M114"]);
    let lines: Vec<(String, Option<u32>)> = printer
        .console_since(before, usize::MAX)
        .into_iter()
        .filter(|e| e.kind == ConsoleKind::Sent)
        .map(|e| (e.text, e.line))
        .collect();
    assert_eq!(
        lines,
        [
            ("M110 N100".to_string(), Some(100)),
            ("G28".to_string(), Some(101)),
            ("M114".to_string(), Some(102))
        ]
    );
    let control = printer.sim_control().unwrap();
    assert_eq!(control.snapshot().last_line_number, 102);
}

#[test]
fn emergency_commands_bypass_the_queue() {
    // One planner slot: every move is acknowledged only when the previous
    // one finished, so the queue drains slowly.
    let sim = SimConfig {
        instant_moves: false,
        planner_buffer: 1,
        ..fast_sim(10.0)
    };
    let printer = connect(sim, options());
    let control = printer.sim_control().unwrap();
    wait_idle(&printer);
    control.clear_received();
    let moves: String = (0..40)
        .map(|i| format!("G1 X{} F600\n", 10 + i % 2 * 10))
        .collect();
    printer.send(&moves);
    wait_for(&printer, "a few moves", |_| {
        control.received_commands().len() > 3
    });
    printer.send("M108");
    printer.send_immediate("M105");
    wait_for(&printer, "M108 and M105", |_| {
        let got = control.received_commands();
        got.iter().any(|c| c == "M108") && got.iter().any(|c| c == "M105")
    });
    control.set_time_scale(1000.0);
    wait_idle(&printer);
    let got = control.received_commands();
    let m108 = got.iter().position(|c| c == "M108").expect("M108 arrived");
    assert!(
        m108 < 20,
        "M108 at {m108} of {}: it waited in the queue",
        got.len()
    );
    let m105 = got.iter().position(|c| c == "M105").expect("M105 arrived");
    assert!(m105 < 20, "M105 at {m105}");
    assert_eq!(got.iter().filter(|c| c.starts_with("G1")).count(), 40);
    // Both went out unnumbered.
    let sent: Vec<_> = console(&printer)
        .into_iter()
        .filter(|e| e.kind == ConsoleKind::Sent && (e.text == "M108" || e.text == "M105"))
        .collect();
    assert!(sent.iter().all(|e| e.line.is_none()), "{sent:?}");
}
