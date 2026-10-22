//! Handshake, firmware information, settings and manual commands against
//! the simulator.

mod common;

use common::*;
use sprint_host::{ConnectionStatus, ConsoleKind, Delivery, Event, Printer, Tracked};

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
    // Linux refuses at once: the attempt may be over before it shows.
    let deadline = std::time::Instant::now() + PATIENCE;
    while printer.with_state(|s| s.status != ConnectionStatus::Connecting) {
        if connecting.is_finished() {
            let result = connecting.join().unwrap();
            assert!(result.is_err(), "connected to nothing: {result:?}");
            return;
        }
        assert!(std::time::Instant::now() < deadline, "never connecting");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
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

/// Poll `tracked` until it is no longer pending.
fn settle(tracked: &mut Tracked) -> Delivery {
    let start = std::time::Instant::now();
    while tracked.poll() == Delivery::Pending {
        assert!(start.elapsed() < PATIENCE, "still pending");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    tracked.poll()
}

#[test]
fn a_save_queued_behind_a_heat_wait_is_acknowledged_after_it() {
    let printer = connect(fast_sim(20.0), options());
    printer.send("M190 S60");
    let mut save = printer.save_settings();
    // The firmware takes nothing else while it waits for the bed.
    wait_for(&printer, "the bed to heat", |s| {
        s.temps.bed.as_ref().is_some_and(|b| b.target == Some(60.0))
    });
    assert_eq!(save.poll(), Delivery::Pending);
    let warming = printer.with_state(|s| s.temps.bed.as_ref().map(|b| b.current));
    assert!(warming.is_some_and(|t| t < 59.0), "{warming:?}");
    assert_eq!(settle(&mut save), Delivery::Acknowledged);
    // It was not acknowledged before the bed reached its target.
    let bed = printer.with_state(|s| s.temps.bed.as_ref().map(|b| b.current));
    assert!(bed.is_some_and(|t| t > 58.0), "{bed:?}");
    wait_for(&printer, "M500's message", |s| {
        s.firmware_messages
            .iter()
            .any(|m| m.contains("Settings Stored"))
    });
    printer.send("M140 S0");
    wait_idle(&printer);
}

#[test]
fn a_tracked_command_is_lost_when_the_link_closes_first() {
    let printer = connect(fast_sim(1.0), options());
    printer.send("M190 S90");
    let mut save = printer.save_settings();
    assert_eq!(save.poll(), Delivery::Pending);
    printer.disconnect();
    assert_eq!(settle(&mut save), Delivery::Lost);
}

#[test]
fn send_tracked_follows_the_last_command_and_needs_one() {
    let printer = connect(fast_sim(20.0), options());
    let mut two = printer.send_tracked("G92 E0\nM114 ; where");
    assert_eq!(settle(&mut two), Delivery::Acknowledged);
    // Emergency commands bypass the queue: nothing to follow.
    assert_eq!(printer.send_tracked("M108").poll(), Delivery::Lost);
    assert_eq!(printer.send_tracked("; a comment").poll(), Delivery::Lost);
    printer.disconnect();
    // Not connected: nothing takes the command.
    assert_eq!(printer.send_tracked("M500").poll(), Delivery::Lost);
}
