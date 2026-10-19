//! Where the terminal interfaces keep their configuration and history.
//!
//! The configuration file is [`AppConfig::path`]: the file `SPRINT_CONFIG`
//! names (a portable setup, or a test that must not touch the user's
//! configuration), else the platform location; every front end, the GUI
//! included, uses the same file. A file that does not fully parse loads
//! leniently ([`AppConfig::load_checked`]): what is broken is left out and
//! reported, and the file as it was is kept as a backup. The shell's
//! history sits next to it.

use std::path::{Path, PathBuf};

use sprint_host::{AppConfig, ConfigReport, HostError};

use crate::actions::find_profile;

/// Environment variable that names another configuration file (see
/// [`AppConfig::path`]).
pub use sprint_host::CONFIG_ENV;

/// File name of the shell history, next to the configuration file.
pub const HISTORY_FILE: &str = "shell-history.txt";

/// The configuration file in use: [`AppConfig::path`] (`$SPRINT_CONFIG`
/// when set and not blank).
pub fn config_path() -> PathBuf {
    AppConfig::path()
}

/// Load the configuration from [`config_path`]: defaults when the file is
/// missing; from a file that does not fully parse, everything that is
/// valid, with the report of what was left out (show it with
/// [`report_messages`]), the file as it was kept as a backup.
pub fn load_config() -> (AppConfig, PathBuf, ConfigReport) {
    let path = config_path();
    let (config, report) = AppConfig::load_checked(&path);
    (config, path, report)
}

/// What the user should read about loading the configuration at `path`:
/// one message per value or line left out, then where the original file
/// is kept. Empty when it loaded as written.
pub fn report_messages(path: &Path, report: &ConfigReport) -> Vec<String> {
    let mut out: Vec<String> = report
        .warnings
        .iter()
        .map(|w| format!("Configuration {}: {w}", path.display()))
        .collect();
    if let Some(backup) = &report.backup {
        out.push(format!(
            "The configuration file as it was is kept in {}",
            backup.display()
        ));
    }
    out
}

/// Add `file` to the recent files of the configuration at `path` and save
/// it, starting from what the file holds now: the GUI or a hand edit may
/// have changed it since it was loaded, and writing the copy loaded at
/// startup would undo that. Without a file, `current` is written. A file
/// that does not fully parse is read leniently (see [`on_disk`]), so one
/// broken value does not stop recent files from being saved. Returns the
/// configuration saved.
pub fn save_recent(
    path: &Path,
    file: PathBuf,
    current: &AppConfig,
) -> Result<AppConfig, HostError> {
    update(path, current, |config| config.push_recent(file))
}

/// Change the configuration at `path` with `edit` and save it, starting
/// from what the file holds now (see [`save_recent`]); without a file,
/// `current` is edited and written. Returns the configuration saved.
pub fn update(
    path: &Path,
    current: &AppConfig,
    edit: impl FnOnce(&mut AppConfig),
) -> Result<AppConfig, HostError> {
    let mut config = on_disk(path, current)?;
    edit(&mut config);
    config.save_to(path)?;
    Ok(config)
}

/// The configuration the file at `path` holds now, to be changed and saved
/// again: `current` when there is no file. A file that does not fully
/// parse gives everything valid in it ([`AppConfig::load_checked`], which
/// keeps the original as a backup first). A file that cannot be read, or
/// whose original could not be backed up, is an error: saving over it
/// would lose it.
pub fn on_disk(path: &Path, current: &AppConfig) -> Result<AppConfig, HostError> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(current.clone()),
        Err(e) => return Err(HostError::Io(e)),
        Ok(meta) if meta.is_dir() => {
            return Err(HostError::Config(format!(
                "{} is a directory, not a configuration file",
                path.display()
            )));
        }
        Ok(_) => {}
    }
    let (config, report) = AppConfig::load_checked(path);
    if !report.is_clean() && report.backup.is_none() {
        return Err(HostError::Config(format!(
            "{}: {}",
            path.display(),
            report.warnings.join("; ")
        )));
    }
    Ok(config)
}

/// [`update`] the configuration file at `path`, or change `current` in
/// memory without one (tests). Errors are messages for the user.
pub fn edit_config(
    path: Option<&Path>,
    current: &AppConfig,
    edit: impl FnOnce(&mut AppConfig),
) -> Result<AppConfig, String> {
    match path {
        Some(path) => {
            update(path, current, edit).map_err(|e| format!("cannot save {}: {e}", path.display()))
        }
        None => {
            let mut config = current.clone();
            edit(&mut config);
            Ok(config)
        }
    }
}

/// Use the profile called `name` (see [`find_profile`]) for this session
/// instead of the active one; the file is not changed. Nothing happens
/// without a name.
pub fn select_profile(config: &mut AppConfig, name: Option<&str>) -> Result<(), String> {
    if let Some(name) = name {
        config.active_profile = find_profile(&config.profiles, name)?;
    }
    Ok(())
}

/// Change the profile called `name` with `edit` (which gets the
/// configuration and the profile's index) in the configuration file at
/// `path` as it is now ([`on_disk`]), and save it; without a path (tests)
/// `current` is changed in memory. The file's own active profile is kept
/// unless `edit` changes it. Returns the configuration saved and the
/// profile's index in it, or why it could not be saved (the file cannot be
/// read or written, or no longer has the profile).
pub fn edit_profile(
    path: Option<&Path>,
    current: &AppConfig,
    name: &str,
    edit: impl FnOnce(&mut AppConfig, usize),
) -> Result<(AppConfig, usize), String> {
    let mut config = match path {
        Some(path) => {
            on_disk(path, current).map_err(|e| format!("cannot read {}: {e}", path.display()))?
        }
        None => current.clone(),
    };
    let Some(index) = config.profiles.iter().position(|p| p.name == name) else {
        return Err(format!(
            "the configuration file has no profile called {name} any more"
        ));
    };
    edit(&mut config, index);
    if let Some(path) = path {
        config
            .save_to(path)
            .map_err(|e| format!("cannot save {}: {e}", path.display()))?;
    }
    Ok((config, index))
}

/// The shell history file for a configuration file.
pub fn history_path(config: &Path) -> PathBuf {
    config
        .parent()
        .map_or_else(|| PathBuf::from(HISTORY_FILE), |dir| dir.join(HISTORY_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::Macro;

    /// A scratch configuration file, removed with its directory on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let dir = std::env::temp_dir()
                .join(format!("sprint-tui-config-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("config.toml")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn named_macro(name: &str) -> Macro {
        Macro {
            name: name.into(),
            gcode: "M117 hi".into(),
            color: None,
            confirm: false,
        }
    }

    #[test]
    fn recent_files_are_added_to_the_file_as_it_is_now() {
        let scratch = Scratch::new("recent");
        let path = scratch.file();
        // Loaded at startup, before someone (the GUI, a hand edit) added a
        // macro to the file.
        let loaded = AppConfig::default();
        let mut edited = AppConfig::default();
        edited.macros.push(named_macro("Added elsewhere"));
        edited.save_to(&path).expect("save");
        let saved = save_recent(&path, PathBuf::from("a.gcode"), &loaded).expect("saved");
        let on_disk = AppConfig::load_from(&path).expect("load");
        assert_eq!(on_disk, saved);
        assert!(on_disk.macros.iter().any(|m| m.name == "Added elsewhere"));
        assert_eq!(on_disk.recent_files, [PathBuf::from("a.gcode")]);
        // No file yet: the configuration in memory is written.
        std::fs::remove_file(&path).expect("remove");
        let saved = save_recent(&path, PathBuf::from("b.gcode"), &loaded).expect("saved");
        assert_eq!(saved.recent_files, [PathBuf::from("b.gcode")]);
        assert!(path.exists());
    }

    #[test]
    fn a_partly_broken_file_still_takes_recent_files() {
        let scratch = Scratch::new("broken");
        let path = scratch.file();
        let mut good = AppConfig::default();
        good.macros.push(named_macro("Kept"));
        good.save_to(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        // A decimal comma (a hand edit) breaks one line.
        let broken = text.replacen("console_max_lines = 5000", "console_max_lines = 5,5", 1);
        assert_ne!(broken, text, "the line to break is in the file");
        std::fs::write(&path, &broken).expect("write");
        let saved = save_recent(&path, PathBuf::from("c.gcode"), &AppConfig::default())
            .expect("saved despite the broken line");
        assert_eq!(saved.recent_files, [PathBuf::from("c.gcode")]);
        assert!(saved.macros.iter().any(|m| m.name == "Kept"), "{saved:?}");
        assert_eq!(AppConfig::load_from(&path).expect("now valid"), saved);
        // The file as it was is kept.
        let backup = scratch.0.join("config.toml.bak");
        assert_eq!(std::fs::read_to_string(&backup).expect("backup"), broken);
        // The report names the line and the backup.
        std::fs::write(&path, &broken).expect("write");
        let (_, report) = AppConfig::load_checked(&path);
        let messages = report_messages(&path, &report);
        assert!(
            messages
                .first()
                .is_some_and(|m| m.starts_with("Configuration ") && m.contains("config.toml: ")),
            "{messages:?}"
        );
        assert!(
            messages
                .last()
                .is_some_and(|m| m.ends_with("config.toml.bak")),
            "{messages:?}"
        );
        assert!(report_messages(&path, &ConfigReport::default()).is_empty());
    }

    #[test]
    fn a_configuration_that_cannot_be_read_is_not_replaced() {
        let scratch = Scratch::new("unreadable");
        // A directory where the file should be.
        let path = scratch.file();
        std::fs::create_dir_all(&path).expect("directory");
        let err = save_recent(&path, PathBuf::from("d.gcode"), &AppConfig::default())
            .expect_err("not a file");
        assert!(err.to_string().contains("is a directory"), "{err}");
        assert!(path.is_dir());
        let why = edit_profile(
            Some(&path),
            &AppConfig::default(),
            "Virtual printer",
            |_, _| {},
        )
        .expect_err("not a file");
        assert!(why.starts_with("cannot read"), "{why}");
    }

    #[test]
    fn profiles_are_edited_in_the_file_as_it_is_now() {
        let scratch = Scratch::new("profiles");
        let path = scratch.file();
        let mut loaded = AppConfig::default();
        loaded.profiles.push(sprint_host::PrinterProfile {
            name: "Bench".into(),
            ..sprint_host::PrinterProfile::default()
        });
        // Changed elsewhere since it was loaded.
        let mut edited = loaded.clone();
        edited.console_max_lines = 777;
        edited.save_to(&path).expect("save");
        let (saved, index) = edit_profile(Some(&path), &loaded, "Bench", |config, i| {
            config.active_profile = i;
        })
        .expect("saved");
        assert_eq!(index, 1);
        let on_disk = AppConfig::load_from(&path).expect("load");
        assert_eq!(on_disk, saved);
        assert_eq!(on_disk.active_profile, 1);
        assert_eq!(on_disk.console_max_lines, 777);
        // A profile the file no longer has is an error; nothing is saved.
        let why = edit_profile(Some(&path), &loaded, "Gone", |config, _| {
            config.console_max_lines = 100;
        })
        .expect_err("missing");
        assert!(why.contains("no profile called Gone"), "{why}");
        assert_eq!(AppConfig::load_from(&path).expect("load"), on_disk);
        // Without a file: in memory only.
        let (memory, _) =
            edit_profile(None, &loaded, "Bench", |c, i| c.active_profile = i).expect("memory");
        assert_eq!(memory.active_profile, 1);
        // A session's profile by name; the file is not touched.
        let mut session = loaded.clone();
        select_profile(&mut session, Some("bench")).expect("found");
        assert_eq!(session.active_profile, 1);
        select_profile(&mut session, None).expect("nothing to do");
        assert_eq!(session.active_profile, 1);
        assert!(select_profile(&mut session, Some("nope")).is_err());
    }

    #[test]
    fn the_path_is_the_hosts() {
        assert_eq!(config_path(), AppConfig::path());
        assert_eq!(CONFIG_ENV, "SPRINT_CONFIG");
    }

    #[test]
    fn history_sits_next_to_the_configuration() {
        assert_eq!(
            history_path(Path::new("dir/config.toml")),
            Path::new("dir").join(HISTORY_FILE)
        );
        assert_eq!(history_path(Path::new("")), PathBuf::from(HISTORY_FILE));
    }
}
