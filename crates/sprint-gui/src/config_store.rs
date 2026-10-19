//! Debounced, off-thread saving of the [`AppConfig`].
//!
//! Every change marks the configuration dirty; it is written
//! [`ConfigSaver::DEBOUNCE`] after the last change, on a worker thread (one
//! save at a time, so two writes never race on the temporary file). On
//! exit [`ConfigSaver::flush`] writes whatever is still pending.
//!
//! The file is shared: the TUI, the shell and hand edits change it while
//! the GUI runs. A save therefore never writes the GUI's copy wholesale: it
//! reads the file again and [`merge`]s. What the GUI changed since its last
//! save (or since loading) wins, everything else stays as it is on disk.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use sprint_host::{AppConfig, HostError, MAX_RECENT_FILES};

/// Saves the configuration (see the module docs).
#[derive(Debug)]
pub struct ConfigSaver {
    path: Option<PathBuf>,
    /// The GUI's configuration as of its last save (or as loaded): the
    /// common ancestor of the GUI's copy and the file, for [`merge`].
    base: AppConfig,
    changed_at: Option<Instant>,
    /// The running save and the configuration it writes (the next base).
    worker: Option<(Receiver<Result<(), String>>, AppConfig)>,
    saves: u32,
}

impl ConfigSaver {
    /// Quiet time after the last change before saving.
    pub const DEBOUNCE: Duration = Duration::from_millis(800);

    /// A saver writing to `path` the changes made to `loaded` (the
    /// configuration as read from that file); `None` keeps the
    /// configuration in memory only (tests, demos).
    pub fn new(path: Option<PathBuf>, loaded: &AppConfig) -> ConfigSaver {
        ConfigSaver {
            path,
            base: loaded.clone(),
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
        if let Some((worker, _)) = &self.worker {
            let result = match worker.try_recv() {
                Ok(result) => result,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    Err("the save was interrupted".to_owned())
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return None,
            };
            if let Some((_, written)) = self.worker.take() {
                self.saves += 1;
                match result {
                    Ok(()) => self.base = written,
                    Err(e) => error = Some(e),
                }
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
        let base = self.base.clone();
        let ours = config.clone();
        let target = path.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-config".to_owned())
            .spawn(move || {
                let _ = tx.send(save_merged(&base, &ours, &target));
            });
        match spawned {
            Ok(_) => self.worker = Some((rx, config.clone())),
            Err(_) => {
                // No thread: save here (a small file).
                self.saves += 1;
                match save_merged(&self.base, config, path) {
                    Ok(()) => self.base = config.clone(),
                    Err(e) => error = Some(e),
                }
            }
        }
        error
    }

    /// Write pending changes now (waits for a running save first).
    pub fn flush(&mut self, config: &AppConfig) -> Result<(), String> {
        if let Some((worker, written)) = self.worker.take() {
            if let Ok(Ok(())) = worker.recv_timeout(Duration::from_secs(5)) {
                self.base = written;
            }
            self.saves += 1;
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        if self.changed_at.take().is_none() {
            return Ok(());
        }
        self.saves += 1;
        save_merged(&self.base, config, path)?;
        self.base = config.clone();
        Ok(())
    }
}

/// Write `ours` to `path`, merged (see [`merge`]) with the file as it is
/// now, `base` being the configuration `ours` started from. A missing file
/// gets `ours`; a file that does not parse (a hand edit in progress) is
/// kept as `<name>.bak` first.
fn save_merged(base: &AppConfig, ours: &AppConfig, path: &Path) -> Result<(), String> {
    let merged = match AppConfig::load_from(path) {
        Ok(disk) => merge(base, ours, &disk),
        Err(HostError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => ours.clone(),
        Err(HostError::Config(msg)) => {
            let mut name = path.file_name().unwrap_or_default().to_os_string();
            name.push(".bak");
            let backup = path.with_file_name(name);
            if let Err(e) = std::fs::copy(path, &backup) {
                return Err(format!("{msg} (and it could not be backed up: {e})"));
            }
            log::warn!(
                "the configuration file is invalid ({msg}); kept it as {} and saved Sprint's",
                backup.display()
            );
            ours.clone()
        }
        // Unreadable: writing would replace what cannot be merged.
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    merged.save_to(path).map_err(|e| e.to_string())
}

/// Merge three versions of the configuration: `base`, the one the GUI
/// started from; `ours`, the GUI's now; `disk`, the file's now. What the
/// GUI changed since `base` wins; everything it did not touch comes from
/// `disk`, so the changes the TUI, the shell or a hand edit made meanwhile
/// are kept:
///
/// * profiles and macros merge entry by entry (by name): entries the GUI
///   added or edited are the GUI's, entries it deleted stay deleted, the
///   others are the file's, and entries added to the file are kept;
/// * recent files: the files the GUI opened go first, the ones it removed
///   stay removed, the rest is the file's list;
/// * the active profile and the console size are the GUI's when it changed
///   them, else the file's.
pub fn merge(base: &AppConfig, ours: &AppConfig, disk: &AppConfig) -> AppConfig {
    let mut merged = disk.clone();
    merged.profiles = merge_keyed(&base.profiles, &ours.profiles, &disk.profiles, |p| {
        p.name.as_str()
    });
    if merged.profiles.is_empty() {
        merged.profiles = ours.profiles.clone();
    }
    // The active profile, found by its name in the merged list.
    let active = if ours.active_profile != base.active_profile {
        ours
    } else {
        disk
    };
    let name = active.active().name.as_str();
    merged.active_profile = merged
        .profiles
        .iter()
        .position(|p| p.name == name)
        .unwrap_or(active.active_profile)
        .min(merged.profiles.len().saturating_sub(1));
    merged.macros = merge_keyed(&base.macros, &ours.macros, &disk.macros, |m| {
        m.name.as_str()
    });
    merged.recent_files = merge_recent(&base.recent_files, &ours.recent_files, &disk.recent_files);
    if ours.console_max_lines != base.console_max_lines {
        merged.console_max_lines = ours.console_max_lines;
    }
    merged
}

/// Three-way merge of a list whose entries are named by `key` (see
/// [`merge`]). Lists with duplicate names merge as a whole: the GUI's when
/// it changed it, else the file's.
fn merge_keyed<T: Clone + PartialEq>(
    base: &[T],
    ours: &[T],
    disk: &[T],
    key: impl Fn(&T) -> &str,
) -> Vec<T> {
    if ours == base {
        return disk.to_vec();
    }
    if disk == base {
        return ours.to_vec();
    }
    let unique = |list: &[T]| {
        let mut names: Vec<&str> = list.iter().map(&key).collect();
        names.sort_unstable();
        names.windows(2).all(|w| w[0] != w[1])
    };
    if !(unique(base) && unique(ours) && unique(disk)) {
        return ours.to_vec();
    }
    let find = |list: &[T], name: &str| list.iter().position(|t| key(t) == name);
    let mut out = Vec::with_capacity(ours.len().max(disk.len()));
    for entry in ours {
        let name = key(entry);
        let untouched = find(base, name).is_some_and(|i| base[i] == *entry);
        if !untouched {
            // Added or edited in the GUI.
            out.push(entry.clone());
        } else if let Some(i) = find(disk, name) {
            // The file's version (deleted from the file: gone).
            out.push(disk[i].clone());
        }
    }
    // Added to the file meanwhile (an entry the GUI deleted stays deleted).
    for entry in disk {
        let name = key(entry);
        if find(ours, name).is_none() && find(base, name).is_none() {
            out.push(entry.clone());
        }
    }
    out
}

/// Three-way merge of the recent files, most recent first (see [`merge`]).
fn merge_recent(base: &[PathBuf], ours: &[PathBuf], disk: &[PathBuf]) -> Vec<PathBuf> {
    if ours == base {
        return disk.to_vec();
    }
    // The files the GUI opened since `base` went to the front: the shortest
    // prefix of `ours` after which the list is still in `base`'s order.
    let in_base_order = |rest: &[PathBuf]| {
        let mut remaining = base.iter();
        rest.iter().all(|p| remaining.any(|b| b == p))
    };
    let opened = (0..=ours.len())
        .find(|&k| in_base_order(&ours[k..]))
        .unwrap_or(ours.len());
    let mut out: Vec<PathBuf> = ours[..opened].to_vec();
    for path in disk {
        let removed = base.contains(path) && !ours.contains(path);
        if !removed && !out.contains(path) {
            out.push(path.clone());
        }
    }
    out.truncate(MAX_RECENT_FILES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::{Macro, PrinterProfile};

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
        let mut saver = ConfigSaver::new(Some(path.clone()), &AppConfig::default());
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
        let mut saver = ConfigSaver::new(Some(path.clone()), &AppConfig::default());
        let config = AppConfig::default();
        assert_eq!(saver.flush(&config), Ok(()), "nothing pending");
        assert!(!path.exists());
        saver.touch(Instant::now());
        assert_eq!(saver.flush(&config), Ok(()));
        assert!(path.exists());
        assert!(!saver.is_pending());
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));

        let mut memory = ConfigSaver::new(None, &AppConfig::default());
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
        let mut saver = ConfigSaver::new(Some(path.clone()), &AppConfig::default());
        saver.touch(Instant::now());
        let error = wait_idle(&mut saver, &AppConfig::default());
        assert!(error.is_some());
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    fn macro_named(name: &str, gcode: &str) -> Macro {
        Macro {
            name: name.to_owned(),
            gcode: gcode.to_owned(),
            color: None,
            confirm: false,
        }
    }

    fn names(macros: &[Macro]) -> Vec<&str> {
        macros.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn saves_keep_what_others_wrote_meanwhile() {
        let path = temp_path("shared");
        let loaded = AppConfig::default();
        loaded.save_to(&path).expect("initial file");
        let mut saver = ConfigSaver::new(Some(path.clone()), &loaded);
        // The TUI opens a file and a hand edit adds a macro.
        let mut theirs = AppConfig::load_from(&path).expect("file");
        theirs.push_recent(PathBuf::from("from-the-tui.gcode"));
        theirs.macros.push(macro_named("Hand edit", "M117 hi"));
        theirs.save_to(&path).expect("their save");
        // Meanwhile the GUI opens another file.
        let mut ours = loaded.clone();
        ours.push_recent(PathBuf::from("from-the-gui.gcode"));
        saver.touch(Instant::now());
        assert_eq!(wait_idle(&mut saver, &ours), None);
        let saved = AppConfig::load_from(&path).expect("saved");
        assert_eq!(
            saved.recent_files,
            [
                PathBuf::from("from-the-gui.gcode"),
                PathBuf::from("from-the-tui.gcode")
            ]
        );
        assert_eq!(
            names(&saved.macros),
            ["Home", "Bed mesh", "Cool down", "Motors off", "Hand edit"]
        );
        // The next save starts from what the GUI wrote: a macro the GUI
        // adds now still keeps the hand-made one.
        ours.macros.push(macro_named("Mine", "M117 mine"));
        saver.touch(Instant::now());
        assert_eq!(wait_idle(&mut saver, &ours), None);
        let saved = AppConfig::load_from(&path).expect("saved");
        assert_eq!(
            names(&saved.macros),
            [
                "Home",
                "Bed mesh",
                "Cool down",
                "Motors off",
                "Mine",
                "Hand edit"
            ]
        );
        // On exit too.
        let mut theirs = AppConfig::load_from(&path).expect("file");
        theirs.console_max_lines = 777;
        theirs.save_to(&path).expect("their save");
        ours.push_recent(PathBuf::from("last.gcode"));
        saver.touch(Instant::now());
        assert_eq!(saver.flush(&ours), Ok(()));
        let saved = AppConfig::load_from(&path).expect("saved");
        assert_eq!(saved.console_max_lines, 777);
        assert_eq!(saved.recent_files[0], PathBuf::from("last.gcode"));
        assert_eq!(saved.macros.len(), 6);
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn an_invalid_file_is_backed_up_before_it_is_replaced() {
        let path = temp_path("invalid");
        std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
        std::fs::write(&path, "macros = [ half typed").expect("write");
        let mut saver = ConfigSaver::new(Some(path.clone()), &AppConfig::default());
        saver.touch(Instant::now());
        assert_eq!(saver.flush(&AppConfig::default()), Ok(()));
        let mut backup = path.as_os_str().to_owned();
        backup.push(".bak");
        assert_eq!(
            std::fs::read_to_string(PathBuf::from(backup)).expect("backup"),
            "macros = [ half typed"
        );
        assert!(AppConfig::load_from(&path).is_ok());
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    fn profile(name: &str, bed: f32) -> PrinterProfile {
        PrinterProfile {
            name: name.to_owned(),
            bed_size: [bed, bed],
            ..PrinterProfile::default()
        }
    }

    #[test]
    fn profiles_and_macros_merge_entry_by_entry() {
        let base = AppConfig {
            profiles: vec![profile("A", 200.0), profile("B", 200.0)],
            active_profile: 1,
            macros: vec![macro_named("x", "1"), macro_named("y", "2")],
            ..AppConfig::default()
        };
        // The GUI edits A, deletes macro y and adds z.
        let mut ours = base.clone();
        ours.profiles[0].bed_size = [300.0, 300.0];
        ours.macros = vec![macro_named("x", "1"), macro_named("z", "3")];
        // The file got profile C, an edited B, an edited x and a new w.
        let mut disk = base.clone();
        disk.profiles[1].bed_size = [250.0, 250.0];
        disk.profiles.insert(0, profile("C", 100.0));
        disk.active_profile = 2;
        disk.macros = vec![
            macro_named("x", "edited"),
            macro_named("y", "2"),
            macro_named("w", "4"),
        ];
        let merged = merge(&base, &ours, &disk);
        let profiles: Vec<(&str, f32)> = merged
            .profiles
            .iter()
            .map(|p| (p.name.as_str(), p.bed_size[0]))
            .collect();
        assert_eq!(profiles, [("A", 300.0), ("B", 250.0), ("C", 100.0)]);
        // B stays active, wherever it is now.
        assert_eq!(merged.active().name, "B");
        assert_eq!(names(&merged.macros), ["x", "z", "w"]);
        assert_eq!(merged.macros[0].gcode, "edited");
        // The GUI switched to A: A, found by name.
        ours.active_profile = 0;
        assert_eq!(merge(&base, &ours, &disk).active().name, "A");
        // Untouched by the GUI: the file wins as a whole.
        assert_eq!(merge(&base, &base, &disk), disk);
        // Duplicate names: the GUI's list as a whole.
        let mut dup = ours.clone();
        dup.macros.push(macro_named("x", "again"));
        assert_eq!(merge(&base, &dup, &disk).macros, dup.macros);
    }

    #[test]
    fn recent_files_keep_both_sides() {
        let p = |s: &str| PathBuf::from(s);
        let base = vec![p("a"), p("b"), p("c")];
        // The GUI opened c again, then d; the file gained x.
        let ours = vec![p("d"), p("c"), p("a"), p("b")];
        let disk = vec![p("x"), p("a"), p("b"), p("c")];
        assert_eq!(
            merge_recent(&base, &ours, &disk),
            [p("d"), p("c"), p("x"), p("a"), p("b")]
        );
        // Cleared in the GUI: what the GUI knew is gone, x stays.
        assert_eq!(merge_recent(&base, &[], &disk), [p("x")]);
        // Untouched by the GUI: the file's list.
        assert_eq!(merge_recent(&base, &base, &disk), disk);
        // Never longer than the limit.
        let many: Vec<PathBuf> = (0..30).map(|i| p(&format!("{i}"))).collect();
        assert_eq!(
            merge_recent(&base, &[p("new")], &many).len(),
            MAX_RECENT_FILES
        );
    }
}
