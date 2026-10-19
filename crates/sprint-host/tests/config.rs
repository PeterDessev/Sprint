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

    let (config, report) = AppConfig::load_checked(&path);
    assert_eq!(config, AppConfig::default());
    assert!(!report.is_clean());
    let backup = dir.path().join("config.toml.bak");
    assert_eq!(report.backup.as_deref(), Some(backup.as_path()));
    assert_eq!(fs::read_to_string(&backup).unwrap(), garbage);
    assert!(path.exists(), "the file stays until it is saved over");
    // Loading it again reuses the identical backup.
    assert_eq!(
        AppConfig::load_checked(&path).1.backup,
        Some(backup.clone())
    );

    // A second broken file never replaces the older backup.
    fs::write(&path, "console_max_lines = \"many\"").unwrap();
    let (config, report) = AppConfig::load_checked(&path);
    assert_eq!(config, AppConfig::default());
    assert_eq!(fs::read_to_string(&backup).unwrap(), garbage);
    let second = dir.path().join("config.toml.1.bak");
    assert_eq!(report.backup.as_deref(), Some(second.as_path()));
    assert_eq!(
        fs::read_to_string(&second).unwrap(),
        "console_max_lines = \"many\""
    );
}

#[test]
fn one_bad_value_keeps_every_profile_and_macro() {
    // A lowercase enum, a decimal comma or a value from a newer version
    // used to discard the whole file; then the defaults were saved and a
    // second mistake overwrote the only backup.
    let dir = TempDir::new("config-lenient");
    let path = dir.path().join("config.toml");
    let mut config = AppConfig {
        profiles: vec![
            PrinterProfile {
                name: "My real printer".into(),
                connection: ConnectTarget::parse("COM7@250000").unwrap(),
                max_z: 300.0,
                ..PrinterProfile::default()
            },
            PrinterProfile {
                name: "Second printer".into(),
                ..PrinterProfile::default()
            },
        ],
        ..AppConfig::default()
    };
    config.macros.push(sprint_host::Macro {
        name: "Purge".into(),
        gcode: "G1 E20 F300".into(),
        color: Some([10, 20, 30]),
        confirm: true,
    });
    config.save_to(&path).unwrap();
    let original = fs::read_to_string(&path).unwrap();
    let edited = original.replacen("checksum = \"Always\"", "checksum = \"always\"", 1);
    assert_ne!(edited, original);
    fs::write(&path, &edited).unwrap();

    let (loaded, report) = AppConfig::load_checked(&path);
    let names: Vec<&str> = loaded.profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["My real printer", "Second printer"]);
    assert_eq!(loaded.profiles[0].max_z, 300.0);
    assert_eq!(loaded.profiles[0].connection, config.profiles[0].connection);
    assert_eq!(loaded.profiles[0].options.checksum, ChecksumMode::Always);
    assert_eq!(loaded.macros, config.macros);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].starts_with("profiles[0].options.checksum: ignored \"always\""),
        "{:?}",
        report.warnings
    );
    let backup = report.backup.clone().expect("a backup");
    assert_eq!(fs::read_to_string(&backup).unwrap(), edited);

    // The repaired configuration is saved; a second typo later (a decimal
    // comma) loses only that line and keeps the first backup.
    loaded.save_to(&path).unwrap();
    let saved = fs::read_to_string(&path).unwrap();
    fs::write(&path, saved.replacen("max_z = 300.0", "max_z = 300,0", 1)).unwrap();
    let (again, report) = AppConfig::load_checked(&path);
    assert_eq!(again.profiles.len(), 2);
    assert_eq!(again.profiles[0].max_z, 250.0, "the broken line fell back");
    assert_eq!(again.profiles[0].name, "My real printer");
    assert!(
        report.warnings[0].starts_with("line "),
        "{:?}",
        report.warnings
    );
    assert_eq!(
        fs::read_to_string(&backup).unwrap(),
        edited,
        "older backups stay"
    );
    assert_ne!(report.backup, Some(backup));

    // A connection kind from a newer version falls back to the simulator
    // rather than losing the profile.
    let future = saved.replacen(
        "[profiles.connection.Serial]",
        "[profiles.connection.Bluetooth]",
        1,
    );
    assert_ne!(future, saved);
    fs::write(&path, future).unwrap();
    let (future, report) = AppConfig::load_checked(&path);
    assert_eq!(future.profiles[0].name, "My real printer");
    assert!(future.profiles[0].connection.is_simulator());
    assert!(
        report.warnings[0].contains("profiles[0].connection"),
        "{:?}",
        report.warnings
    );
}

#[test]
fn a_file_that_is_not_utf8_is_backed_up_too() {
    // A truncated write or disk corruption, not text at all.
    let dir = TempDir::new("config-binary");
    let path = dir.path().join("config.toml");
    let garbage: &[u8] = b"console_max_lines = 900\n\xff\xfe\x00\x81 binary junk";
    fs::write(&path, garbage).unwrap();
    assert!(matches!(
        AppConfig::load_from(&path),
        Err(HostError::Config(_))
    ));
    // What can be read is kept; the binary junk is dropped.
    let (config, report) = AppConfig::load_checked(&path);
    assert_eq!(config.console_max_lines, 900);
    assert!(
        report.warnings[0].contains("not UTF-8"),
        "{:?}",
        report.warnings
    );
    // The user's bytes survive in the backup, so saving later does not
    // destroy them.
    assert_eq!(
        fs::read(dir.path().join("config.toml.bak")).unwrap(),
        garbage
    );
    assert_eq!(AppConfig::load_or_default(&path).console_max_lines, 900);
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
fn unedited_old_defaults_are_upgraded() {
    // Earlier defaults switched off only the active hotend; profiles and
    // macros that still hold them exactly get the new defaults, edited
    // ones are kept.
    let dir = TempDir::new("config-upgrade");
    let path = dir.path().join("config.toml");
    let old_cancel = "M104 S0\nM140 S0\nM107\nG91\nG1 Z10 F600\nG90\nG28 X Y\nM84\n";
    let mut config = AppConfig::default();
    config.profiles[0].cancel_script = old_cancel.into();
    config.profiles.push(PrinterProfile {
        name: "Edited".into(),
        cancel_script: format!("{old_cancel}M117 bye\n"),
        ..PrinterProfile::default()
    });
    for m in &mut config.macros {
        if m.name == "Cool down" {
            m.gcode = "M104 S0\nM140 S0\nM107".into();
        }
    }
    config.save_to(&path).unwrap();
    let loaded = AppConfig::load_from(&path).unwrap();
    assert_eq!(
        loaded.profiles[0].cancel_script,
        sprint_host::DEFAULT_CANCEL_SCRIPT
    );
    assert!(loaded.profiles[0].cancel_script.contains("{heaters_off}"));
    assert_eq!(
        loaded.profiles[1].cancel_script,
        format!("{old_cancel}M117 bye\n")
    );
    let cool = loaded
        .macros
        .iter()
        .find(|m| m.name == "Cool down")
        .unwrap();
    assert_eq!(cool.gcode, "{heaters_off}\nM107");
    assert_eq!(loaded.macros, AppConfig::default().macros);
}

#[test]
fn default_path_is_under_a_sprint_directory() {
    let path = AppConfig::default_path();
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

#[test]
fn documented_items_are_reexported_at_the_crate_root() {
    // ARCHITECTURE.md: every public item is also available at the root.
    assert_eq!(sprint_host::MAX_RECENT_FILES, 12);
    assert_eq!(sprint_host::DEFAULT_CONSOLE_LINES, 5000);
    assert_eq!(sprint_host::MAX_IN_FLIGHT_LIMIT, 32);
    assert_eq!(sprint_host::EVENT_QUEUE_LIMIT, 65_536);
    assert_eq!(sprint_host::TEMP_HISTORY_LEN, 1800);
    assert_eq!(sprint_host::TEMP_HISTORY_SECS, 1800.0);
    assert_eq!(sprint_host::FIRMWARE_MESSAGES_LEN, 50);
    assert!(sprint_host::DEFAULT_PAUSE_SCRIPT.contains("M400"));
    assert!(sprint_host::DEFAULT_RESUME_SCRIPT.contains("G92 E{e}"));
    assert!(sprint_host::DEFAULT_CANCEL_SCRIPT.contains("M84"));
    assert!(sprint_host::DEFAULT_CANCEL_SCRIPT.contains("{heaters_off}"));
    assert_eq!(sprint_host::heaters_off(1, false, false), ["M104 S0"]);
    assert!(sprint_host::ConfigReport::default().is_clean());
    assert_eq!(sprint_host::format_number(1.50, 3), "1.5");
    assert_eq!(sprint_host::Console::new(3).capacity(), 3);
    assert!(sprint_host::is_printer_vendor(0x1A86));
    assert!(!sprint_host::PRINTER_USB_VENDORS.is_empty());
    assert!(sprint_host::READ_TIMEOUT.as_millis() <= 100);
    let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "x");
    assert!(sprint_host::is_no_data(&timed_out));
    let (_, _, handle): (_, _, sprint_host::LinkHandle) =
        sprint_host::Link::new(std::io::empty(), std::io::sink()).split();
    handle.close();
}
