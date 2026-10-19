//! Where the terminal interfaces keep their configuration and history.
//!
//! The configuration file is [`AppConfig::path`] unless the `SPRINT_CONFIG`
//! environment variable names another file (a portable setup, or a test
//! that must not touch the user's configuration). The shell's history sits
//! next to it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sprint_host::{AppConfig, HostError};

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
    let mut config = match AppConfig::load_from(path) {
        Ok(on_disk) => on_disk,
        Err(HostError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => current.clone(),
        Err(e) => return Err(e),
    };
    config.push_recent(file);
    config.save_to(path)?;
    Ok(config)
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
