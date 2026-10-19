//! Tuning and calibration flows: PID autotune and bed meshes.

mod common;

use common::*;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{Event, Heater};

#[test]
fn pid_autotune_reports_the_result_and_refreshes_settings() {
    let printer = connect(fast_sim(300.0), options());
    let events = printer.subscribe();
    printer.start_pid_autotune(Heater::Hotend(0), 200.0, 3);
    wait_for(&printer, "autotune to start", |s| {
        s.autotune.as_ref().is_some_and(|a| a.running)
    });
    let at = printer.with_state(|s| s.autotune.clone()).unwrap();
    assert_eq!(
        (at.heater, at.target, at.cycles),
        (Heater::Hotend(0), 200.0, 3)
    );
    wait_for(&printer, "autotune to finish", |s| {
        s.autotune.as_ref().is_some_and(|a| !a.running)
    });
    let at = printer.with_state(|s| s.autotune.clone()).unwrap();
    let (kp, ki, kd) = at.result.expect("a result");
    assert!(kp > 0.0 && ki > 0.0 && kd > 0.0, "{at:?}");
    assert_eq!(at.error, None);
    // (The event follows the state change: wait for it rather than look.)
    let finished = wait_for_event(&events, "AutotuneFinished", |e| match *e {
        Event::AutotuneFinished { heater, kp, ki, kd } => Some((heater, kp, ki, kd)),
        _ => None,
    });
    assert_eq!(finished, (Heater::Hotend(0), kp, ki, kd));
    // U1 applied the values; the settings refresh shows them.
    wait_idle(&printer);
    let pid = printer.with_state(|s| s.settings.hotend_pid(0)).unwrap();
    assert!(
        (pid.0 - kp).abs() < 0.01 && (pid.2 - kd).abs() < 0.01,
        "{pid:?} vs {kp} {kd}"
    );
    let sent: Vec<String> = console(&printer)
        .into_iter()
        .filter(|e| e.kind == sprint_host::ConsoleKind::Sent)
        .map(|e| e.text)
        .collect();
    assert!(sent.iter().any(|c| c == "M303 E0 S200 C3 U1"), "{sent:?}");
}

#[test]
fn failed_autotune_is_reported() {
    let sim = SimConfig {
        heated_bed: false,
        ..fast_sim(100.0)
    };
    let printer = connect(sim, options());
    let events = printer.subscribe();
    printer.start_pid_autotune(Heater::Bed, 60.0, 3);
    wait_for(&printer, "autotune to fail", |s| {
        s.autotune
            .as_ref()
            .is_some_and(|a| !a.running && a.error.is_some())
    });
    wait_for_event(&events, "AutotuneFailed", |e| {
        matches!(e, Event::AutotuneFailed(_)).then_some(())
    });
}

#[test]
fn probing_reports_a_bed_mesh() {
    let printer = connect(fast_sim(100.0), options());
    let events = printer.subscribe();
    printer.probe_bed_mesh();
    wait_for(&printer, "a mesh", |s| s.mesh.is_some());
    wait_idle(&printer);
    let mesh = printer.with_state(|s| s.mesh.clone()).unwrap();
    assert_eq!((mesh.rows(), mesh.cols()), (5, 5));
    assert!(mesh.range().is_some_and(|r| r > 0.0), "a warped bed");
    wait_for_event(&events, "the mesh", |e| {
        matches!(e, Event::Mesh(_)).then_some(())
    });
    let control = printer.sim_control().unwrap();
    let got = control.received_commands();
    let g28 = got.iter().position(|c| c == "G28").unwrap();
    let g29 = got.iter().position(|c| c == "G29").unwrap();
    assert!(g28 < g29);
    // Homed now: probing again skips G28, and the mesh can be re-read.
    control.clear_received();
    printer.probe_bed_mesh();
    printer.request_bed_mesh();
    wait_idle(&printer);
    assert_eq!(control.received_commands(), ["G29", "M420 V", "M420 V"]);
}

/// Start an autotune on a real-time printer (it heats for minutes) and
/// wait until the printer runs it.
fn autotune_running(printer: &sprint_host::Printer) {
    printer.start_pid_autotune(Heater::Hotend(0), 200.0, 5);
    let control = printer.sim_control().unwrap();
    wait_for(printer, "the M303", |s| {
        s.autotune.as_ref().is_some_and(|a| a.running)
            && control
                .received_commands()
                .iter()
                .any(|c| c.starts_with("M303"))
    });
}

/// The autotune ended with an error mentioning `why`, announced with an
/// event.
fn ended_with(
    printer: &sprint_host::Printer,
    events: &crossbeam_channel::Receiver<Event>,
    why: &str,
) {
    let message = wait_for_event(events, "AutotuneFailed", |e| match e {
        Event::AutotuneFailed(m) => Some(m.clone()),
        _ => None,
    });
    assert!(message.contains(why), "{message}");
    let tune = printer.with_state(|s| s.autotune.clone()).unwrap();
    assert!(!tune.running, "{tune:?}");
    assert_eq!(tune.error.as_deref(), Some(message.as_str()));
    assert_eq!(tune.result, None);
}

#[test]
fn an_autotune_cut_short_by_a_halt_ends() {
    let printer = connect(fast_sim(1.0), options());
    autotune_running(&printer);
    let events = printer.subscribe();
    printer.emergency_stop();
    ended_with(&printer, &events, "halted");
    // Only once: releasing the M303 does not report it again.
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        !events
            .try_iter()
            .any(|e| matches!(e, Event::AutotuneFailed(_)))
    );
}

#[test]
fn an_autotune_cut_short_by_a_board_reset_ends() {
    let printer = connect(fast_sim(1.0), options());
    autotune_running(&printer);
    let events = printer.subscribe();
    printer.reset_board().unwrap();
    ended_with(&printer, &events, "reset");
}

#[test]
fn an_autotune_cut_short_by_a_disconnect_ends() {
    let printer = connect(fast_sim(1.0), options());
    autotune_running(&printer);
    let events = printer.subscribe();
    printer.disconnect();
    ended_with(&printer, &events, "disconnected");
}

#[test]
fn an_autotune_is_not_started_while_halted() {
    let printer = connect(fast_sim(20.0), options());
    printer.emergency_stop();
    wait_for(&printer, "halted", |s| s.halted);
    let events = printer.subscribe();
    printer.start_pid_autotune(Heater::Bed, 60.0, 3);
    ended_with(&printer, &events, "halted");
    let tune = printer.with_state(|s| s.autotune.clone()).unwrap();
    assert_eq!(
        (tune.heater, tune.target, tune.cycles),
        (Heater::Bed, 60.0, 3)
    );
    wait_idle(&printer);
    let sent_m303 = || {
        console(&printer)
            .iter()
            .any(|e| e.kind == sprint_host::ConsoleKind::Sent && e.text.starts_with("M303"))
    };
    assert!(!sent_m303(), "no M303 for a halted printer");
    // Typed by hand it goes out, but fails the same way at once.
    printer.send("M303 E0 S200 C3");
    ended_with(&printer, &events, "halted");
    assert!(sent_m303());
}
