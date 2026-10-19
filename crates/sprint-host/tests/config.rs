//! Configuration files on disk: round trip, atomic save, missing and
//! corrupt files.

mod common;

use std::fs;
use std::path::PathBuf;

use common::TempDir;
use sprint_host::{AppConfig, ChecksumMode, ConnectTarget, HostError, PrinterProfile};

#[test]
fn save_and_load_round_trip() {
    let dir = TempDir::new("config-round-trip");
    let path = dir.path().join("nested").join("config.toml");
    let mut config = AppConfig::default();
    config.profiles.push(PrinterProfile {
        name: "Workshop".into(),
        connection: ConnectTarget::parse("COM7@250000").unwrap(),
        ..PrinterProfile::default()
    });
    config.active_profile = 1;
    config.active_mut().options.checksum = ChecksumMode::Never;
    config.push_recent(PathBuf::from("cube.gcode"));
    config.console_max_lines = 12_345;

    config.save_to(&path).unwrap();
    assert!(path.exists());
    let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "config.toml")
        .collect();
    assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");

    let loaded = AppConfig::load_from(&path).unwrap();
    assert_eq!(loaded, config);
    assert_eq!(loaded.active().name, "Workshop");

    // Saving again replaces the file atomically.
    let mut changed = loaded.clone();
    changed.console_max_lines = 777;
    changed.save_to(&path).unwrap();
    assert_eq!(AppConfig::load_from(&path).unwrap().console_max_lines, 777);
}

#[test]
fn missing_file_gives_defaults() {
    let dir = TempDir::new("config-missing");
    let path = dir.path().join("config.toml");
    assert!(matches!(AppConfig::load_from(&path), Err(HostError::Io(_))));
    assert_eq!(AppConfig::load_or_default(&path), AppConfig::default());
    assert!(!path.exists(), "loading must not create the file");
}

#[test]
fn corrupt_file_is_backed_up_and_defaults_are_used() {
    let dir = TempDir::new("config-corrupt");
    let path = dir.path().join("config.toml");
    let garbage = "profiles = [[[ this is not toml\n\u{0}\u{1}";
    fs::write(&path, garbage).unwrap();
    assert!(matches!(
        AppConfig::load_from(&path),
        Err(HostError::Config(_))
    ));

    let config = AppConfig::load_or_default(&path);
    assert_eq!(config, AppConfig::default());
    let backup = dir.path().join("config.toml.bak");
    assert_eq!(fs::read_to_string(&backup).unwrap(), garbage);
    assert!(!path.exists(), "the corrupt file was moved aside");

    // A second corrupt file replaces the older backup.
    fs::write(&path, "console_max_lines = \"many\"").unwrap();
    assert_eq!(AppConfig::load_or_default(&path), AppConfig::default());
    assert_eq!(
        fs::read_to_string(&backup).unwrap(),
        "console_max_lines = \"many\""
    );
}

#[test]
fn wrong_types_and_out_of_range_values_are_repaired_or_rejected() {
    let dir = TempDir::new("config-repair");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "active_profile = 40\nconsole_max_lines = 0\nprofiles = []\n[[macros]]\nname = \"m\"\ngcode = \"G28\"\n",
    )
    .unwrap();
    let c = AppConfig::load_from(&path).unwrap();
    assert_eq!(c.profiles.len(), 1);
    assert_eq!(c.active_profile, 0);
    assert_eq!(c.console_max_lines, 5000);
    assert_eq!(c.macros.len(), 1);
    assert!(!c.macros[0].confirm);
}

#[test]
fn default_path_is_under_a_sprint_directory() {
    let path = AppConfig::path();
    let fallback = path.as_os_str() == "sprint-config.toml";
    assert!(
        fallback
            || (path.file_name().is_some_and(|n| n == "config.toml")
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|n| n == "Sprint")),
        "{}",
        path.display()
    );
}
