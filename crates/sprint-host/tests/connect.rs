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
fn settings_report_headers_are_not_firmware_messages() {
    let printer = connect(fast_sim(20.0), options());
    let events = printer.subscribe();
    printer.load_settings(); // M501 and M503 both print the report
    printer.save_settings(); // "echo:Settings Stored ..." is a message
    wait_for(&printer, "M500's message", |s| {
        s.firmware_messages
            .iter()
            .any(|m| m.contains("Settings Stored"))
    });
    wait_idle(&printer);
    let messages = printer.with_state(|s| s.firmware_messages.clone());
    assert!(
        messages.iter().all(|m| !m.starts_with(';')),
        "report headers kept as messages: {messages:?}"
    );
    // (The reports come before M500's message: read up to its event.)
    let headers = events_until(
        &events,
        "M500's message",
        |e| matches!(e, Event::FirmwareMessage(m) if m.contains("Settings Stored")),
    )
    .iter()
    .filter(|e| matches!(e, Event::FirmwareMessage(m) if m.starts_with(';')))
    .count();
    assert_eq!(headers, 0);
    // The headers are still in the console.
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.kind == ConsoleKind::Received && e.text == "echo:; Steps per unit:")
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
    // (The event follows the state change: wait for it rather than look.)
    let statuses: Vec<ConnectionStatus> = events_until(&events, "Connected", |e| {
        matches!(e, Event::Status(ConnectionStatus::Connected))
    })
    .into_iter()
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

fn failures(events: &crossbeam_channel::Receiver<Event>) -> Vec<Event> {
    events
        .try_iter()
        .filter(|e| {
            matches!(
                e,
                Event::Error(_) | Event::Status(ConnectionStatus::Failed(_))
            )
        })
        .collect()
}

#[test]
fn disconnecting_during_the_handshake_is_not_a_failure() {
    // A printer that never answers: the handshake is still running.
    let printer = Printer::new();
    let events = printer.subscribe();
    let silent = sprint_host::Link::new(SilentReader, std::io::sink());
    printer
        .connect_link(silent, "silent printer", options())
        .unwrap();
    wait_for(&printer, "the handshake", |s| {
        s.status == ConnectionStatus::Handshaking
    });
    printer.disconnect();
    assert_eq!(
        printer.with_state(|s| s.status.clone()),
        ConnectionStatus::Disconnected
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(failures(&events).is_empty());
    assert_eq!(printer.with_state(|s| s.last_error.clone()), None);
}

/// A reader that never has data.
struct SilentReader;

impl std::io::Read for SilentReader {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(std::time::Duration::from_millis(20));
        Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no data"))
    }
}

#[test]
fn a_cancelled_tcp_attempt_ends_disconnected() {
    // Nothing listens on the port; on Windows a refused loopback connect
    // takes about two seconds, long enough to cancel it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let printer = Printer::new();
    let events = printer.subscribe();
    let target = sprint_host::ConnectTarget::parse(&format!("tcp://127.0.0.1:{port}")).unwrap();
    let started = std::time::Instant::now();
    let connecting = {
        let printer = printer.clone();
        std::thread::spawn(move || printer.connect(target, options()))
    };
    wait_for(&printer, "connecting", |s| {
        s.status == ConnectionStatus::Connecting
    });
    printer.disconnect();
    let result = connecting.join().unwrap();
    if started.elapsed() < std::time::Duration::from_millis(50) {
        // The attempt failed before the cancel (fast refusal): nothing to
        // check.
        return;
    }
    assert!(
        matches!(result, Err(sprint_host::HostError::Cancelled)),
        "{result:?}"
    );
    assert_eq!(
        printer.with_state(|s| s.status.clone()),
        ConnectionStatus::Disconnected
    );
    assert!(failures(&events).is_empty(), "no failure was reported");
}
