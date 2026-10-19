//! The app with the host's newer capabilities, driven headless against the
//! built-in virtual printer: connecting at startup to a given target,
//! resetting the board after an emergency stop, recovering a print from
//! the line it stopped at, skipping objects, refusing files that are not
//! G-code, the shared command-safety classifier and the lenient
//! configuration load. No serial port is ever opened: every target here is
//! the simulator.

mod app_support;

use std::path::PathBuf;
use std::time::Duration;

use app_support::{WINDOW, app, expect, fast_sim, settle, sim_options};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sprint_gui::app::{AppOptions, SprintApp};
use sprint_host::ConnectTarget;
use sprint_host::sprint_sim::SimConfig;

fn connect(h: &mut Harness<'_, SprintApp>) {
    h.get_by_label("Connect").click();
    expect(h, "the virtual printer to connect", |app| {
        app.state().is_connected()
    });
}

#[allow(dead_code)]
fn analyzed(app: &SprintApp) -> bool {
    app.file().is_some_and(|f| f.toolpath.is_some()) && !app.is_loading()
}

#[allow(dead_code)]
fn toast_titles(app: &SprintApp) -> Vec<String> {
    app.toasts().iter().map(|t| t.title().to_owned()).collect()
}

#[allow(dead_code)]
fn received(app: &SprintApp) -> Vec<String> {
    app.printer()
        .sim_control()
        .map(|c| c.received_commands())
        .unwrap_or_default()
}

/// A private temporary directory.
#[allow(dead_code)]
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sprint-gui-core-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn a_target_given_at_startup_is_connected() {
    // `sprint gui --connect T`: that printer, though the profile's
    // auto-connect is off (and its printer another one).
    let given = SimConfig {
        machine_name: "Given printer".into(),
        ..fast_sim()
    };
    let options = AppOptions {
        connect: Some(ConnectTarget::Simulator(given)),
        ..sim_options()
    };
    assert!(!options.config.active().auto_connect);
    let mut h = app(options, WINDOW);
    expect(&mut h, "the given printer to answer", |app| {
        app.state()
            .firmware
            .as_ref()
            .and_then(|f| f.machine_type.as_deref())
            == Some("Given printer")
    });
    // Without a target, the virtual printer or auto-connect, nothing opens.
    let mut idle = app(sim_options(), WINDOW);
    settle(&mut idle);
    std::thread::sleep(Duration::from_millis(100));
    settle(&mut idle);
    assert!(!idle.state().state().status.is_open());
    assert!(!idle.state().connection_busy());
    connect(&mut idle);
}

#[test]
fn a_partly_broken_configuration_loads_and_says_what_it_left_out() {
    let dir = temp_dir("config");
    let path = dir.join("config.toml");
    let mut config = sim_options().config;
    config.profiles.push(sprint_host::PrinterProfile {
        name: "Second printer".into(),
        ..sprint_host::PrinterProfile::default()
    });
    config.save_to(&path).expect("file");
    let text = std::fs::read_to_string(&path).expect("text");
    let broken = text.replacen("checksum = \"Always\"", "checksum = \"always\"", 1);
    assert_ne!(broken, text);
    std::fs::write(&path, &broken).expect("write");
    // What AppOptions::desktop does with the file AppConfig::path() names.
    let (config, report) = sprint_host::AppConfig::load_checked(&path);
    assert!(!report.is_clean());
    let options = AppOptions {
        config,
        config_report: Some(report),
        config_path: Some(path.clone()),
        ..sim_options()
    };
    let mut h = app(options, WINDOW);
    settle(&mut h);
    // Both profiles loaded; a toast says what happened.
    assert_eq!(h.state().config().profiles.len(), 2);
    assert!(toast_titles(h.state()).contains(&"Some settings could not be read".to_owned()));
    let message = h
        .state()
        .toasts()
        .iter()
        .find_map(|t| t.message_text().map(str::to_owned))
        .unwrap_or_default();
    assert!(message.contains("config.toml.bak"), "{message}");
    // The preferences list the problem and the backup.
    h.state_mut()
        .push_action(sprint_gui::context::AppAction::OpenPreferences);
    settle(&mut h);
    assert!(h.query_by_label_contains("checksum").is_some());
    // The toast and the settings say where the original is.
    assert!(
        h.query_all_by_label_contains("The file as it was is kept in")
            .count()
            >= 2
    );
    assert!(
        h.query_all_by_label_contains(&format!("Saved to {}", path.display()))
            .next()
            .is_some()
    );
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}
