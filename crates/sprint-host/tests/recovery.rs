//! Recovering from halts: emergency stops, killed and stopped firmware,
//! board resets.

mod common;

use std::time::Duration;

use common::*;
use sprint_host::sprint_sim::{SimConfig, SimServer};
use sprint_host::{ConnectTarget, HostError, Link, Printer};

fn sim() -> SimConfig {
    fast_sim(50.0)
}

#[test]
fn m999_cannot_recover_a_killed_marlin_but_a_board_reset_can() {
    let printer = connect(sim(), options());
    printer.emergency_stop();
    wait_for(&printer, "halted", |s| s.halted);
    assert!(
        !printer.with_state(|s| s.restartable),
        "M112 kills Marlin: M999 cannot restart it"
    );
    // The virtual printer behaves like Marlin: M999 changes nothing.
    printer.firmware_restart();
    std::thread::sleep(Duration::from_millis(300));
    assert!(printer.with_state(|s| s.halted));
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.text.contains("ignores M999")),
        "{:?}",
        tail(&printer, 10)
    );

    let control = printer.sim_control().unwrap();
    control.clear_received();
    printer.reset_board().expect("the simulator can be reset");
    wait_for(&printer, "a fresh handshake", |s| {
        s.is_connected() && !s.halted
    });
    // (settings_loaded is still true from before the reset: wait for the
    // setup queries themselves, M114 is the last.)
    wait_for(&printer, "the setup after the reset", |_| {
        control.received_commands().iter().any(|c| c == "M114")
    });
    wait_idle(&printer);
    control.clear_received();
    printer.send("G28\nM114");
    wait_idle(&printer);
    assert_eq!(control.received_commands(), ["G28", "M114"]);
    assert!(!control.snapshot().halted);
}

#[test]
fn a_stopped_firmware_is_restartable() {
    let printer = connect(sim(), options());
    printer
        .sim_control()
        .unwrap()
        .inject_line("Error:Printer stopped due to errors. Fix the error and use M999 to restart.");
    wait_for(&printer, "halted", |s| s.halted);
    assert!(printer.with_state(|s| s.restartable));
    // A kill reported afterwards needs a reset after all.
    printer
        .sim_control()
        .unwrap()
        .inject_line("Error:Printer halted. kill() called!");
    wait_for(&printer, "killed", |s| !s.restartable);
    assert!(printer.with_state(|s| s.halted));
}

#[test]
fn a_board_behind_tcp_or_a_custom_link_cannot_be_reset() {
    let printer = Printer::new();
    assert!(matches!(
        printer.reset_board(),
        Err(HostError::NotConnected)
    ));

    let server = SimServer::bind("127.0.0.1:0", sim()).expect("bind");
    let target =
        ConnectTarget::parse(&format!("tcp://127.0.0.1:{}", server.local_addr().port())).unwrap();
    printer.connect(target, options()).unwrap();
    wait_for(&printer, "connected over TCP", |s| s.is_connected());
    let err = printer.reset_board().unwrap_err();
    assert!(
        matches!(&err, HostError::Unsupported(m) if m.contains("reset button")),
        "{err}"
    );
    printer.disconnect();

    let (reader, writer, _) = Link::simulator(sim()).split();
    printer
        .connect_link(Link::new(reader, writer), "custom", options())
        .unwrap();
    assert!(matches!(
        printer.reset_board(),
        Err(HostError::Unsupported(_))
    ));
    printer.disconnect();
}

#[test]
fn commands_queued_before_a_board_reset_are_not_replayed() {
    // A jog queued behind a long dwell must not run on the rebooted,
    // unhomed printer.
    let printer = connect(fast_sim(1.0), options());
    let control = printer.sim_control().unwrap();
    printer.send("G4 S30");
    wait_for(&printer, "the dwell to run", |_| {
        control.received_commands().iter().any(|c| c == "G4 S30")
    });
    printer.send("G91\nG1 Z-5 F600\nG90\nM117 stale-command");
    wait_for(&printer, "the jog to be queued", |s| s.link.queue_len >= 4);
    control.clear_received();
    control.reset();
    wait_for(&printer, "the handshake after the reset", |s| {
        s.is_connected() && s.settings_loaded && s.position.is_some()
    });
    wait_idle(&printer);
    let got = control.received_commands();
    assert!(
        !got.iter()
            .any(|c| c.starts_with("G1") || c.contains("stale")),
        "{got:?}"
    );
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.text.contains("Dropped 4 queued command(s)")),
        "{:?}",
        tail(&printer, 20)
    );
}

#[test]
fn commands_queued_before_a_halt_are_dropped() {
    let printer = connect(fast_sim(1.0), options());
    let control = printer.sim_control().unwrap();
    printer.send("G4 S30");
    wait_for(&printer, "the dwell to run", |_| {
        control.received_commands().iter().any(|c| c == "G4 S30")
    });
    printer.send("G91\nG1 Z-5 F600\nG90");
    wait_for(&printer, "the jog to be queued", |s| s.link.queue_len >= 3);
    control
        .inject_line("Error:Printer stopped due to errors. Fix the error and use M999 to restart.");
    wait_for(&printer, "halted", |s| s.halted);
    wait_for(&printer, "the queue to empty", |s| s.link.queue_len == 0);
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.text.contains("Dropped 3 queued command(s)"))
    );
}

#[test]
fn a_halt_closes_the_host_prompt() {
    // The halted firmware ignores M876: an open prompt would only cover
    // the recovery actions.
    let printer = connect(sim(), options());
    let events = printer.subscribe();
    let control = printer.sim_control().unwrap();
    for line in [
        "//action:prompt_begin Insert filament",
        "//action:prompt_button Continue",
        "//action:prompt_show",
    ] {
        control.inject_line(line);
    }
    wait_for(&printer, "the prompt", |s| s.prompt.is_some());
    printer.emergency_stop();
    wait_for(&printer, "halted", |s| s.halted);
    assert_eq!(printer.with_state(|s| s.prompt.clone()), None);
    // (The event follows the state change: wait for it rather than look.)
    let deadline = std::time::Instant::now() + PATIENCE;
    let closed = std::iter::from_fn(|| events.recv_deadline(deadline).ok())
        .any(|e| matches!(e, sprint_host::Event::Prompt(None)));
    assert!(closed, "an Event::Prompt(None) tells the UIs");
}

#[test]
fn homing_cut_short_by_a_halt_leaves_the_axes_unhomed() {
    // An emergency stop during G28 is common (homing that goes wrong): the
    // G28 released by the halt was never finished.
    let homing_takes_time = SimConfig {
        instant_moves: false,
        ..fast_sim(1.0)
    };
    // Typed commands time out soon (a G28 after six times this).
    let quick = sprint_host::ConnectOptions {
        ok_timeout_secs: 0.5,
        ..options()
    };
    let printer = connect(homing_takes_time, quick);
    let control = printer.sim_control().unwrap();
    printer.send("G28");
    wait_for(&printer, "the homing to run", |_| {
        control.received_commands().iter().any(|c| c == "G28")
    });
    wait_for(&printer, "the homing in flight", |s| s.link.in_flight > 0);
    printer.emergency_stop();
    wait_for(&printer, "halted", |s| s.halted);
    wait_for(&printer, "the window to clear", |s| s.link.in_flight == 0);
    assert_eq!(
        printer.with_state(|s| s.homed),
        Some(sprint_host::Axes::NONE)
    );
    // Commands typed while halted are ignored by the killed firmware;
    // released after the ok timeout, they did nothing either.
    printer.send("G28\nM290 Z0.5");
    wait_for(&printer, "the typed commands in flight", |s| {
        s.link.in_flight > 0
    });
    wait_for(&printer, "the typed commands to time out", |s| {
        s.link.in_flight == 0 && s.link.queue_len == 0
    });
    assert_eq!(
        printer.with_state(|s| (s.homed, s.babystep_z)),
        (Some(sprint_host::Axes::NONE), 0.0)
    );
}

#[test]
fn a_board_reset_forgets_the_extruder_mode_a_stop_keeps_it() {
    // After M83 the host restores nothing when it extrudes; after a reset
    // the firmware is back in M82, which extruding must keep.
    let printer = connect(sim(), options());
    let control = printer.sim_control().unwrap();
    let extruded = |printer: &Printer| {
        wait_idle(printer);
        control.clear_received();
        printer.extrude(0, 1.0, 100.0);
        wait_for(printer, "the extrusion", |_| {
            control.received_commands().len() >= 2
        });
        wait_idle(printer);
        control.received_commands()
    };
    printer.send("M83");
    assert_eq!(extruded(&printer), ["M83", "G1 E1 F100"]);
    control.clear_received();
    printer.reset_board().unwrap();
    wait_for(&printer, "the setup after the reset", |_| {
        control.received_commands().iter().any(|c| c == "M114")
    });
    assert_eq!(extruded(&printer), ["M83", "G1 E1 F100", "M82"]);

    // A stopped Marlin restarted with M999 keeps its modes (M999 only
    // sets it running again), M83 included.
    printer.send("M83");
    assert_eq!(extruded(&printer), ["M83", "G1 E1 F100"], "M83 is known");
    control
        .inject_line("Error:Printer stopped due to errors. Fix the error and use M999 to restart.");
    wait_for(&printer, "halted", |s| s.halted);
    printer.firmware_restart();
    wait_for(&printer, "running again", |s| !s.halted);
    assert_eq!(extruded(&printer), ["M83", "G1 E1 F100"], "M83 is kept");
    // The printer still extrudes relatively: the host did not switch it.
    let e = || control.snapshot().position[3];
    let before = e();
    printer.send("G1 E1 F100");
    wait_idle(&printer);
    assert!((e() - (before + 1.0)).abs() < 1e-3, "{before} -> {}", e());
}
