//! Where the terminal interfaces keep their configuration and history.
//!
//! The configuration file is [`AppConfig::path`] unless the `SPRINT_CONFIG`
//! environment variable names another file (a portable setup, or a test
//! that must not touch the user's configuration). The shell's history sits
//! next to it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sprint_host::AppConfig;

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

/// The shell history file for a configuration file.
pub fn history_path(config: &Path) -> PathBuf {
    config
        .parent()
        .map_or_else(|| PathBuf::from(HISTORY_FILE), |dir| dir.join(HISTORY_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

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
