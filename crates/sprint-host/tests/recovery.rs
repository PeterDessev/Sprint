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

    printer.reset_board().expect("the simulator can be reset");
    wait_for(&printer, "a fresh handshake", |s| {
        s.is_connected() && !s.halted && s.settings_loaded
    });
    let control = printer.sim_control().unwrap();
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
