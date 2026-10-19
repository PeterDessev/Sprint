//! Writing the user's files safely.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `contents` to `path` without ever leaving a half-written file
/// there: into a temporary file next to it first, flushed to disk, then
/// renamed over `path` in one step (as the configuration is saved). A
/// failure on the way (a full disk, a crash) keeps the previous file, a
/// settings backup or an export, intact.
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let tmp = temp_sibling(path);
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents.as_ref())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A hidden temporary name next to `path`, unique within this process.
fn temp_sibling(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_else(|| "sprint".as_ref()));
    name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sprint-gui-fsutil-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn writes_and_replaces_without_leftovers() {
        let dir = temp_dir("replace");
        let path = dir.join("backup.gcode");
        write_atomic(&path, "M92 E93\n").expect("first write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "M92 E93\n");
        write_atomic(&path, b"M92 E415\n").expect("second write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "M92 E415\n");
        assert_eq!(files_in(&dir), ["backup.gcode"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_write_leaves_what_was_there() {
        let dir = temp_dir("fail");
        // A directory where the file should go: the rename fails.
        let path = dir.join("export.txt");
        std::fs::create_dir_all(path.join("inside")).expect("dir");
        assert!(write_atomic(&path, "text").is_err());
        assert!(path.join("inside").is_dir());
        assert_eq!(files_in(&dir), ["export.txt"]);
        // A missing directory: nothing is created.
        assert!(write_atomic(&dir.join("missing").join("x.txt"), "text").is_err());
        assert_eq!(files_in(&dir), ["export.txt"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
