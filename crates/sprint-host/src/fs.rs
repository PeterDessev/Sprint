//! File helpers for front ends: [`write_atomic`] saves a file so that a
//! crash or a full disk never leaves it half-written (exports, saved
//! G-code, reports). The configuration is saved through it too.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `bytes` to `path` atomically: they go to a temporary file next to
/// it (same directory, so the same file system), are flushed to disk, and
/// the temporary file is renamed over `path`. Readers see the old file or
/// the new one, never a mix, and a failure leaves the old file as it was
/// (the temporary file is removed). Missing parent directories are
/// created.
///
/// Concurrent writers of the same path each use their own temporary file;
/// the last rename wins.
///
/// ```
/// let dir = std::env::temp_dir().join(format!("sprint-doc-{}", std::process::id()));
/// let path = dir.join("exports").join("settings.gcode");
/// sprint_host::fs::write_atomic(&path, "M92 X80 Y80\n")?;
/// assert_eq!(std::fs::read_to_string(&path)?, "M92 X80 Y80\n");
/// # std::fs::remove_dir_all(&dir)?;
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// # Errors
///
/// `path` without a file name ([`io::ErrorKind::InvalidInput`]), and any
/// error creating the directories, writing the temporary file or renaming
/// it (for example when `path` is a directory or a file another program
/// holds open on Windows).
pub fn write_atomic(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let path = path.as_ref();
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} does not name a file", path.display()),
        ));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let tmp = temporary_sibling(path);
    let result = (|| -> io::Result<()> {
        // No live writer uses this name (process id and call number): a
        // file by that name was left by a process that crashed, and the
        // id came round again. Overwrite it.
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes.as_ref())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `<name>.<pid>-<n>.tmp` next to `path`: unique per process and call.
fn temporary_sibling(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static N: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "sprint-host-fs-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn entries(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_replaces_and_creates_directories() {
        let dir = Scratch::new("write");
        let path = dir.0.join("a").join("b").join("out.gcode");
        write_atomic(&path, b"G28\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"G28\n");
        write_atomic(&path, "M84\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "M84\n");
        write_atomic(&path, []).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"");
        // No temporary file is left behind.
        assert_eq!(dir.entries(), ["a"]);
        let inner: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(inner, ["out.gcode"]);
    }

    #[test]
    fn a_failure_keeps_the_old_file_and_cleans_up() {
        let dir = Scratch::new("fail");
        // A directory cannot be replaced by a file.
        let target = dir.0.join("taken");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside.txt"), "keep").unwrap();
        assert!(write_atomic(&target, b"x").is_err());
        assert_eq!(
            fs::read_to_string(target.join("inside.txt")).unwrap(),
            "keep"
        );
        assert_eq!(dir.entries(), ["taken"]);
        // Paths without a file name are refused.
        let err = write_atomic(Path::new(".."), b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // A parent that is a file cannot hold the file.
        fs::write(dir.0.join("plain"), "file").unwrap();
        assert!(write_atomic(dir.0.join("plain").join("x.txt"), b"x").is_err());
        assert_eq!(fs::read_to_string(dir.0.join("plain")).unwrap(), "file");
    }

    #[test]
    fn a_temporary_file_left_by_a_crash_is_no_obstacle() {
        // A process that crashed mid-write left its temporary files, and
        // this one got the same process id (Windows reuses them soon).
        let dir = Scratch::new("stale");
        let path = dir.0.join("settings.toml");
        for n in 0..500 {
            fs::write(
                dir.0
                    .join(format!("settings.toml.{}-{n}.tmp", std::process::id())),
                "half a fi",
            )
            .unwrap();
        }
        write_atomic(&path, "[ok]\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "[ok]\n");
    }

    #[test]
    fn concurrent_writers_never_mix() {
        let dir = Scratch::new("race");
        let path = dir.0.join("shared.txt");
        let contents: Vec<String> = (0..8)
            .map(|i| format!("writer {i}\n").repeat(2000))
            .collect();
        std::thread::scope(|scope| {
            for c in &contents {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..5 {
                        // On Windows a rename over a file another rename
                        // is replacing can be refused; that is an error,
                        // never a mixed file.
                        let _ = write_atomic(path, c);
                    }
                });
            }
        });
        let got = fs::read_to_string(&path).unwrap();
        assert!(contents.contains(&got), "a mixed file");
        assert_eq!(dir.entries(), ["shared.txt"]);
    }
}
