//! Where the terminal interfaces keep their configuration and history.
//!
//! The configuration file is [`AppConfig::path`] unless the `SPRINT_CONFIG`
//! environment variable names another file (a portable setup, or a test
//! that must not touch the user's configuration); the GUI honours the same
//! variable, so every front end edits the same file. The shell's history
//! sits next to it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sprint_host::{AppConfig, HostError};

use crate::actions::find_profile;

/// Environment variable that overrides the configuration file.
pub const CONFIG_ENV: &str = "SPRINT_CONFIG";

/// File name of the shell history, next to the configuration file.
pub const HISTORY_FILE: &str = "shell-history.txt";

/// The configuration file in use: `$SPRINT_CONFIG` when set (and not
/// blank), else [`AppConfig::path`].
pub fn config_path() -> PathBuf {
    path_from(std::env::var_os(CONFIG_ENV))
}

fn path_from(value: Option<OsString>) -> PathBuf {
    value
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map_or_else(AppConfig::path, PathBuf::from)
}

/// Load the configuration from [`config_path`]: defaults when the file is
/// missing, and a corrupt file is kept as `<name>.bak` (see
/// [`AppConfig::load_or_default`]).
pub fn load_config() -> (AppConfig, PathBuf) {
    let path = config_path();
    (AppConfig::load_or_default(&path), path)
}

/// Add `file` to the recent files of the configuration at `path` and save
/// it, starting from what the file holds now: the GUI or a hand edit may
/// have changed it since it was loaded, and writing the copy loaded at
/// startup would undo that. Without a file, `current` is written. A file
/// that cannot be read or parsed is left alone (an error). Returns the
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
/// `current` is edited and written. A file that cannot be read or parsed
/// is left alone (an error). Returns the configuration saved.
pub fn update(
    path: &Path,
    current: &AppConfig,
    edit: impl FnOnce(&mut AppConfig),
) -> Result<AppConfig, HostError> {
    let mut config = match AppConfig::load_from(path) {
        Ok(on_disk) => on_disk,
        Err(HostError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => current.clone(),
        Err(e) => return Err(e),
    };
    edit(&mut config);
    config.save_to(path)?;
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
/// `path` as it is now, and save it; without a path (tests) `current` is
/// changed in memory. The file's own active profile is kept unless `edit`
/// changes it. Returns the configuration saved and the profile's index in
/// it, or why it could not be saved (the file cannot be read or written,
/// or no longer has the profile).
pub fn edit_profile(
    path: Option<&Path>,
    current: &AppConfig,
    name: &str,
    edit: impl FnOnce(&mut AppConfig, usize),
) -> Result<(AppConfig, usize), String> {
    let mut config = match path {
        Some(path) => match AppConfig::load_from(path) {
            Ok(on_disk) => on_disk,
            Err(HostError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => current.clone(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        },
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

    #[test]
    fn recent_files_are_added_to_the_file_as_it_is_now() {
        let scratch = Scratch::new("recent");
        let path = scratch.file();
        // Loaded at startup, before someone (the GUI, a hand edit) added a
        // macro to the file.
        let loaded = AppConfig::default();
        let mut edited = AppConfig::default();
        edited.macros.push(Macro {
            name: "Added elsewhere".into(),
            gcode: "M117 hi".into(),
            color: None,
            confirm: false,
        });
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
        // A file that does not parse is left alone (not replaced by
        // defaults).
        std::fs::write(&path, "not = [valid").expect("write");
        assert!(save_recent(&path, PathBuf::from("c.gcode"), &loaded).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "not = [valid"
        );
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
    fn the_environment_overrides_the_path() {
        assert_eq!(
            path_from(Some(OsString::from("x/my.toml"))),
            PathBuf::from("x/my.toml")
        );
        assert_eq!(path_from(Some(OsString::from("  "))), AppConfig::path());
        assert_eq!(path_from(None), AppConfig::path());
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
