//! `SPRINT_CONFIG` redirects every load and save (its own test binary: it
//! changes the process environment).

mod common;

use std::fs;

use common::TempDir;
use sprint_host::{AppConfig, CONFIG_ENV, ConnectTarget, PrinterProfile};

#[test]
fn sprint_config_redirects_load_and_save() {
    let dir = TempDir::new("config-env");
    let scratch = dir.path().join("scratch.toml");
    let config = AppConfig {
        profiles: vec![PrinterProfile {
            name: "Scratch profile".into(),
            connection: ConnectTarget::parse("sim").unwrap(),
            auto_connect: true,
            ..PrinterProfile::default()
        }],
        ..AppConfig::default()
    };
    config.save_to(&scratch).unwrap();

    // SAFETY: this binary has no other test, so no other thread reads or
    // writes the environment meanwhile.
    unsafe { std::env::set_var(CONFIG_ENV, &scratch) };
    assert_eq!(AppConfig::path(), scratch);
    assert_ne!(AppConfig::default_path(), scratch);
    // What the GUI's desktop options and the TUI load.
    let loaded = AppConfig::load();
    assert_eq!(loaded.active().name, "Scratch profile");

    let mut changed = loaded;
    changed.console_max_lines = 4321;
    changed.save().unwrap();
    assert_eq!(
        AppConfig::load_from(&scratch).unwrap().console_max_lines,
        4321
    );

    // Blank means unset.
    // SAFETY: as above.
    unsafe { std::env::set_var(CONFIG_ENV, "   ") };
    assert_eq!(AppConfig::path(), AppConfig::default_path());
    // SAFETY: as above.
    unsafe { std::env::remove_var(CONFIG_ENV) };
    assert_eq!(AppConfig::path(), AppConfig::default_path());
    assert!(fs::metadata(&scratch).is_ok());
}
