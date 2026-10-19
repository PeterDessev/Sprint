//! File-name completion for `load` (shell) and the open dialog (TUI).

use std::path::{MAIN_SEPARATOR, Path, PathBuf};

/// Most completion candidates listed.
pub const MAX_PATH_CANDIDATES: usize = 500;

/// File extensions treated as G-code.
pub const GCODE_EXTENSIONS: &[&str] = &["gcode", "gco", "g", "gc", "nc", "ngc", "bgcode"];

/// One completion of a path being typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathCandidate {
    /// The entry's file name (with a trailing separator for directories).
    pub name: String,
    /// The whole input with this entry completed.
    pub completion: String,
    /// A directory.
    pub is_dir: bool,
    /// File size in bytes (files only).
    pub size: Option<u64>,
}

/// Whether `name` has a G-code extension (case-insensitive).
pub fn is_gcode_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| GCODE_EXTENSIONS.iter().any(|g| e.eq_ignore_ascii_case(g)))
}

fn is_separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// The user's home directory, from the environment.
fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `input` as a path: a leading `~` is the home directory, a relative path
/// is relative to `base`.
pub fn resolve(input: &str, base: &Path) -> PathBuf {
    let expanded = match input.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(is_separator) => match home_dir() {
            Some(home) => home.join(rest.trim_start_matches(is_separator)),
            None => PathBuf::from(input),
        },
        _ => PathBuf::from(input),
    };
    if expanded.is_absolute() || expanded.has_root() {
        expanded
    } else {
        base.join(expanded)
    }
}

/// Entries of the directory named by `input` (up to its last separator)
/// whose names start with what follows it, case-insensitively. Directories
/// come first, then files, each sorted by name; hidden entries are listed
/// only when the typed name starts with a dot. With `gcode_only`, files
/// without a G-code extension are left out. Unreadable directories give
/// nothing.
pub fn complete_path(input: &str, base: &Path, gcode_only: bool) -> Vec<PathCandidate> {
    let split = input
        .char_indices()
        .rfind(|(_, c)| is_separator(*c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    // A bare drive ("C:") lists that drive's current directory.
    let split = if split == 0 && cfg!(windows) && input.len() >= 2 && input.as_bytes()[1] == b':' {
        2
    } else {
        split
    };
    let (dir_text, prefix) = input.split_at(split);
    let dir = if dir_text.is_empty() {
        base.to_path_buf()
    } else {
        resolve(dir_text, base)
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let prefix_lower = prefix.to_lowercase();
    let show_hidden = prefix.starts_with('.');
    let mut out: Vec<PathCandidate> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if !name.to_lowercase().starts_with(&prefix_lower) {
                return None;
            }
            if name.starts_with('.') && !show_hidden {
                return None;
            }
            // Follows symlinks, so a link to a directory completes as one.
            let meta = std::fs::metadata(entry.path()).ok();
            let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
            if !is_dir && gcode_only && !is_gcode_name(&name) {
                return None;
            }
            let shown = if is_dir {
                format!("{name}{MAIN_SEPARATOR}")
            } else {
                name
            };
            Some(PathCandidate {
                completion: format!("{dir_text}{shown}"),
                name: shown,
                is_dir,
                size: meta.filter(|m| m.is_file()).map(|m| m.len()),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out.truncate(MAX_PATH_CANDIDATES);
    out
}

/// The longest common prefix of the candidates' completions (compared
/// case-insensitively, taken from the first candidate), for Tab.
pub fn common_completion(candidates: &[PathCandidate]) -> Option<String> {
    let first = candidates.first()?;
    let mut end = first.completion.len();
    for other in &candidates[1..] {
        let common = first
            .completion
            .char_indices()
            .zip(other.completion.chars())
            .take_while(|((_, a), b)| a.to_lowercase().eq(b.to_lowercase()))
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8());
        end = end.min(common);
    }
    first.completion.get(..end).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let path =
                std::env::temp_dir().join(format!("sprint-tui-paths-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("prints")).expect("dir");
            std::fs::create_dir_all(path.join("Projects")).expect("dir");
            std::fs::create_dir_all(path.join(".hidden")).expect("dir");
            for (name, text) in [
                ("cube.gcode", "G28\n"),
                ("cube-v2.GCODE", "G28\nG1 X1\n"),
                ("notes.txt", "hello"),
                ("prints/vase.gco", "G28"),
            ] {
                std::fs::write(path.join(name), text).expect("file");
            }
            Dir(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names(c: &[PathCandidate]) -> Vec<&str> {
        c.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn lists_directories_first_then_files() {
        let dir = Dir::new("list");
        let all = complete_path("", &dir.0, false);
        let sep = MAIN_SEPARATOR;
        assert_eq!(
            names(&all),
            [
                format!("prints{sep}").as_str(),
                &format!("Projects{sep}"),
                "cube-v2.GCODE",
                "cube.gcode",
                "notes.txt"
            ]
        );
        assert!(all[0].is_dir && all[0].size.is_none());
        assert_eq!(all[3].size, Some(4));
        let gcode = complete_path("", &dir.0, true);
        assert_eq!(gcode.len(), 4, "{gcode:?}");
        // Prefixes match case-insensitively; hidden entries need a dot.
        assert_eq!(
            names(&complete_path("CU", &dir.0, false)),
            ["cube-v2.GCODE", "cube.gcode"]
        );
        assert_eq!(complete_path(".h", &dir.0, false).len(), 1);
        assert!(complete_path("zzz", &dir.0, false).is_empty());
    }

    #[test]
    fn completes_inside_directories() {
        let dir = Dir::new("nested");
        let found = complete_path("prints/v", &dir.0, false);
        assert_eq!(names(&found), ["vase.gco"]);
        assert_eq!(found[0].completion, "prints/vase.gco");
        let absolute = format!("{}/prints/", dir.0.display());
        let found = complete_path(&absolute, Path::new("."), false);
        assert_eq!(found[0].completion, format!("{absolute}vase.gco"));
        assert!(complete_path("no-such-dir/x", &dir.0, false).is_empty());
    }

    #[test]
    fn common_prefix_extends_the_input() {
        let dir = Dir::new("common");
        let found = complete_path("c", &dir.0, false);
        assert_eq!(common_completion(&found).as_deref(), Some("cube"));
        let found = complete_path("p", &dir.0, false);
        assert_eq!(common_completion(&found).as_deref(), Some("pr"));
        assert_eq!(common_completion(&[]), None);
    }

    #[test]
    fn gcode_names() {
        for name in ["a.gcode", "B.GCO", "x.g", "part.bgcode", "dir/y.nc"] {
            assert!(is_gcode_name(name), "{name}");
        }
        for name in ["a.txt", "gcode", "a.gcode.bak", ""] {
            assert!(!is_gcode_name(name), "{name}");
        }
    }

    #[test]
    fn resolves_relative_and_home_paths() {
        let base = Path::new("base");
        assert_eq!(resolve("a.gcode", base), Path::new("base").join("a.gcode"));
        if let Some(home) = home_dir() {
            assert_eq!(resolve("~/x.gcode", base), home.join("x.gcode"));
            assert_eq!(resolve("~", base), home);
        }
        assert_eq!(resolve("~user", base), base.join("~user"));
    }
}
