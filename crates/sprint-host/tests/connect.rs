//! Handshake, firmware information, settings and manual commands against
//! the simulator.

mod common;

use common::*;
use sprint_host::{ConnectionStatus, ConsoleKind, Event, Printer};

#[test]
fn handshake_populates_firmware_capabilities_and_settings() {
    let printer = connect(fast_sim(20.0), options());
    let s = printer.snapshot();
    assert_eq!(s.status, ConnectionStatus::Connected);
    assert_eq!(s.target.as_deref(), Some("Virtual printer"));
    let fw = s.firmware.clone().expect("firmware info");
    assert!(fw.name.contains("Marlin"), "{}", fw.name);
    assert_eq!(fw.machine_type.as_deref(), Some("Sprint Virtual Printer"));
    assert!(s.has_capability("AUTOREPORT_TEMP"));
    assert!(s.has_capability("EEPROM"));
    assert!(s.capabilities.len() > 20);
    assert!(s.settings_loaded);
    assert!(s.settings.steps_per_mm().is_some());
    assert!(s.settings.hotend_pid(0).is_some());
    assert!(s.position.is_some());
    assert!(!s.halted);
    // The handshake and setup commands were sent and logged.
    let sent: Vec<String> = console(&printer)
        .into_iter()
        .filter(|e| e.kind == ConsoleKind::Sent)
        .map(|e| e.text)
        .collect();
    assert_eq!(sent[0], "M110 N0");
    for cmd in ["M115", "M155 S2", "M154 S1", "M503", "M114"] {
        assert!(sent.iter().any(|c| c == cmd), "{cmd} not sent: {sent:?}");
    }
    printer.disconnect();
    assert_eq!(
        printer.with_state(|s| s.status.clone()),
        ConnectionStatus::Disconnected
    );
}

#[test]
fn second_connect_is_refused() {
    let printer = connect(fast_sim(20.0), options());
    let again = printer.connect(
        sprint_host::ConnectTarget::Simulator(fast_sim(20.0)),
        options(),
    );
    assert!(matches!(
        again,
        Err(sprint_host::HostError::AlreadyConnected)
    ));
    printer.disconnect();
    // After disconnecting, connecting works again.
    printer
        .connect(
            sprint_host::ConnectTarget::Simulator(fast_sim(20.0)),
            options(),
        )
        .unwrap();
    wait_for(&printer, "reconnect", |s| s.is_connected());
    drop(printer);
}

#[test]
fn status_events_are_published() {
    let printer = Printer::new();
    let events = printer.subscribe();
    printer
        .connect(
            sprint_host::ConnectTarget::Simulator(fast_sim(20.0)),
            options(),
        )
        .unwrap();
    wait_for(&printer, "connected", |s| s.is_connected());
    let statuses: Vec<ConnectionStatus> = events
        .try_iter()
        .filter_map(|e| match e {
            Event::Status(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        [
            ConnectionStatus::Connecting,
            ConnectionStatus::Handshaking,
            ConnectionStatus::Connected
        ]
    );
}
