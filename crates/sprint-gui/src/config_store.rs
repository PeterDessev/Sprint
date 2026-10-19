//! Debounced, off-thread saving of the [`AppConfig`].
//!
//! Every change marks the configuration dirty; it is written
//! [`ConfigSaver::DEBOUNCE`] after the last change, on a worker thread (one
//! save at a time, so two writes never race on the temporary file). On
//! exit [`ConfigSaver::flush`] writes whatever is still pending.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use sprint_host::AppConfig;

/// Saves the configuration (see the module docs).
#[derive(Debug)]
pub struct ConfigSaver {
    path: Option<PathBuf>,
    changed_at: Option<Instant>,
    worker: Option<Receiver<Result<(), String>>>,
    saves: u32,
}

impl ConfigSaver {
    /// Quiet time after the last change before saving.
    pub const DEBOUNCE: Duration = Duration::from_millis(800);

    /// A saver writing to `path`; `None` keeps the configuration in
    /// memory only (tests, demos).
    pub fn new(path: Option<PathBuf>) -> ConfigSaver {
        ConfigSaver {
            path,
            changed_at: None,
            worker: None,
            saves: 0,
        }
    }

    /// Where the configuration is saved.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Note a change at `now`.
    pub fn touch(&mut self, now: Instant) {
        if self.path.is_some() {
            self.changed_at = Some(now);
        }
    }

    /// True while a change waits to be written or a save runs.
    pub fn is_pending(&self) -> bool {
        self.changed_at.is_some() || self.worker.is_some()
    }

    /// Completed saves (successful or not).
    pub fn saves(&self) -> u32 {
        self.saves
    }

    /// Start a save once the debounce has passed and collect a finished
    /// one. Returns the error of a failed save.
    pub fn tick(&mut self, config: &AppConfig, now: Instant) -> Option<String> {
        let mut error = None;
        if let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(result) => {
                    self.worker = None;
                    self.saves += 1;
                    error = result.err();
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.saves += 1;
                    error = Some("the save was interrupted".to_owned());
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return None,
            }
        }
        let (Some(path), Some(changed)) = (&self.path, self.changed_at) else {
            return error;
        };
        if now.saturating_duration_since(changed) < Self::DEBOUNCE {
            return error;
        }
        self.changed_at = None;
        let (tx, rx) = crossbeam_channel::bounded(1);
        let snapshot = config.clone();
        let target = path.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-config".to_owned())
            .spawn(move || {
                let _ = tx.send(snapshot.save_to(&target).map_err(|e| e.to_string()));
            });
        match spawned {
            Ok(_) => self.worker = Some(rx),
            Err(_) => {
                // No thread: save here (a small file).
                self.saves += 1;
                if let Err(e) = config.save_to(path) {
                    error = Some(e.to_string());
                }
            }
        }
        error
    }

    /// Write pending changes now (waits for a running save first).
    pub fn flush(&mut self, config: &AppConfig) -> Result<(), String> {
        if let Some(worker) = self.worker.take() {
            let _ = worker.recv_timeout(Duration::from_secs(5));
            self.saves += 1;
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        if self.changed_at.take().is_none() {
            return Ok(());
        }
        self.saves += 1;
        config.save_to(path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sprint-gui-config-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("config.toml")
    }

    fn wait_idle(saver: &mut ConfigSaver, config: &AppConfig) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut error = None;
        while saver.is_pending() && Instant::now() < deadline {
            error = error.or(saver.tick(config, Instant::now() + ConfigSaver::DEBOUNCE));
            std::thread::sleep(Duration::from_millis(2));
        }
        error
    }

    #[test]
    fn saves_after_the_debounce_on_a_worker() {
        let path = temp_path("debounce");
        let mut saver = ConfigSaver::new(Some(path.clone()));
        let config = AppConfig {
            console_max_lines: 1234,
            ..AppConfig::default()
        };
        let t0 = Instant::now();
        saver.touch(t0);
        assert!(saver.is_pending());
        // Too early: nothing happens.
        assert_eq!(saver.tick(&config, t0), None);
        assert!(!path.exists());
        assert_eq!(wait_idle(&mut saver, &config), None);
        assert_eq!(saver.saves(), 1);
        let loaded = AppConfig::load_from(&path).expect("saved");
        assert_eq!(loaded.console_max_lines, 1234);
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn flush_writes_pending_changes_and_memory_mode_never_writes() {
        let path = temp_path("flush");
        let mut saver = ConfigSaver::new(Some(path.clone()));
        let config = AppConfig::default();
        assert_eq!(saver.flush(&config), Ok(()), "nothing pending");
        assert!(!path.exists());
        saver.touch(Instant::now());
        assert_eq!(saver.flush(&config), Ok(()));
        assert!(path.exists());
        assert!(!saver.is_pending());
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));

        let mut memory = ConfigSaver::new(None);
        memory.touch(Instant::now());
        assert!(!memory.is_pending());
        assert_eq!(memory.tick(&config, Instant::now()), None);
        assert_eq!(memory.flush(&config), Ok(()));
        assert_eq!(memory.saves(), 0);
    }

    #[test]
    fn failures_are_reported() {
        // A directory where the file should be: the rename fails.
        let path = temp_path("fail");
        std::fs::create_dir_all(path.join("blocker")).expect("dir");
        let mut saver = ConfigSaver::new(Some(path.clone()));
        saver.touch(Instant::now());
        let error = wait_idle(&mut saver, &AppConfig::default());
        assert!(error.is_some());
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }
}
