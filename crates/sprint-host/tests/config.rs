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
    assert_eq!(AppConfig::load_or_default(&path), AppConfig::default());
    // The user's bytes survive in the backup, so saving the defaults later
    // does not destroy them.
    assert_eq!(
        fs::read(dir.path().join("config.toml.bak")).unwrap(),
        garbage
    );
    assert!(!path.exists());
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
