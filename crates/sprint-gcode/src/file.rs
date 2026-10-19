//! In-memory G-code file: one text buffer plus line start offsets.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::content::{ContentKind, sniff_content};
use crate::parse::{clean_line, has_command};

/// A loaded G-code file with cheap random access to its lines.
///
/// The whole text lives in one `String`; lines are located through a table
/// of start offsets built in a single pass at load time. `\n`, `\r\n` and
/// lone `\r` line endings are all recognised, and a final line without a
/// newline is kept. Invalid UTF-8 is replaced (lossy decoding) and a leading
/// byte-order mark is dropped.
///
/// Any file loads, whatever it contains; [`GcodeFile::content_kind`] says
/// whether it actually is G-code (front ends refuse to stream 3D models,
/// binary data and other text).
#[derive(Clone)]
pub struct GcodeFile {
    name: String,
    path: Option<PathBuf>,
    text: String,
    /// Start offset of every line plus a final sentinel (`text.len()`).
    starts: Vec<usize>,
    command_count: usize,
    /// Sniffed from the source bytes at load time.
    content: ContentKind,
}

impl Default for GcodeFile {
    fn default() -> Self {
        GcodeFile {
            name: String::new(),
            path: None,
            text: String::new(),
            starts: vec![0],
            command_count: 0,
            content: ContentKind::Empty,
        }
    }
}

impl fmt::Debug for GcodeFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GcodeFile")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("lines", &self.len())
            .field("bytes", &self.text.len())
            .field("commands", &self.command_count)
            .field("content", &self.content)
            .finish()
    }
}

impl GcodeFile {
    /// Read a file from disk (lossy UTF-8). The display name is the file
    /// name component of `path`.
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<GcodeFile> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let mut file = GcodeFile::from_bytes(name, bytes);
        file.path = Some(path.to_path_buf());
        Ok(file)
    }

    /// Build from text already in memory.
    pub fn from_text(name: impl Into<String>, text: impl Into<String>) -> GcodeFile {
        let text = text.into();
        let content = sniff_content(text.as_bytes(), text.len() as u64);
        GcodeFile::build(name.into(), text, content)
    }

    /// Build from raw bytes (lossy UTF-8).
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> GcodeFile {
        let content = sniff_content(&bytes, bytes.len() as u64);
        let text = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
        GcodeFile::build(name.into(), text, content)
    }

    fn build(name: String, mut text: String, content: ContentKind) -> GcodeFile {
        if text.starts_with('\u{FEFF}') {
            text.drain(..'\u{FEFF}'.len_utf8());
        }
        let (starts, command_count) = index_lines(&text);
        GcodeFile {
            name,
            path: None,
            text,
            starts,
            command_count,
            content,
        }
    }

    /// What the file contains, sniffed from its first bytes when it was
    /// loaded (see [`sniff_content`]): G-code, a 3D model (ASCII or binary
    /// STL, 3MF, OBJ, STEP), binary data or other text. Only
    /// [`ContentKind::Gcode`] should be streamed to a printer.
    pub fn content_kind(&self) -> ContentKind {
        self.content
    }

    /// File name for display.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Path the file was loaded from, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The whole text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of lines.
    pub fn len(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    /// True when the file has no lines.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Raw line `index` without its line ending; `""` when out of range.
    pub fn line(&self, index: usize) -> &str {
        let (Some(&start), Some(&next)) = (self.starts.get(index), self.starts.get(index + 1))
        else {
            return "";
        };
        // Line content never contains `\r` or `\n` (both end a line), so
        // stripping them from the end removes exactly the line ending.
        let bytes = self.text.as_bytes();
        let mut end = next;
        while end > start && matches!(bytes.get(end - 1), Some(b'\n' | b'\r')) {
            end -= 1;
        }
        self.text.get(start..end).unwrap_or("")
    }

    /// The command to send for line `index` (`clean_line(line(index))`).
    pub fn command(&self, index: usize) -> Option<Cow<'_, str>> {
        clean_line(self.line(index))
    }

    /// Size of the text in bytes.
    pub fn byte_len(&self) -> usize {
        self.text.len()
    }

    /// Byte offset of the start of line `index` (`byte_len()` when out of
    /// range).
    pub fn byte_offset(&self, index: usize) -> usize {
        self.starts.get(index).copied().unwrap_or(self.text.len())
    }

    /// Index of the line containing byte `offset` (clamped to the last line).
    pub fn line_at_offset(&self, offset: usize) -> usize {
        let n = self.starts.partition_point(|&s| s <= offset);
        n.saturating_sub(1).min(self.len().saturating_sub(1))
    }

    /// All lines, without line endings.
    pub fn lines(&self) -> impl Iterator<Item = &str> + '_ {
        (0..self.len()).map(move |i| self.line(i))
    }

    /// Number of lines that produce a command (precomputed at load).
    pub fn command_count(&self) -> usize {
        self.command_count
    }
}

/// Single pass over the text: line starts (plus sentinel) and the number of
/// lines that carry a command.
fn index_lines(text: &str) -> (Vec<usize>, usize) {
    let bytes = text.as_bytes();
    let n = bytes.len();
    // Typical slicer output averages 20-30 bytes per line.
    let mut starts = Vec::with_capacity(n / 24 + 2);
    let mut commands = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;
    while start < n {
        // Find the end of this line.
        while i < n && bytes[i] != b'\n' && bytes[i] != b'\r' {
            i += 1;
        }
        let content_end = i;
        if i < n {
            if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                i += 2;
            } else {
                i += 1;
            }
        }
        starts.push(start);
        if let Some(line) = text.get(start..content_end)
            && has_command(line)
        {
            commands += 1;
        }
        start = i;
    }
    starts.push(n);
    (starts, commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(f: &GcodeFile) -> Vec<&str> {
        f.lines().collect()
    }

    #[test]
    fn line_endings() {
        let f = GcodeFile::from_text("a", "G1 X1\nG1 X2\r\nG1 X3\rG1 X4");
        assert_eq!(lines(&f), vec!["G1 X1", "G1 X2", "G1 X3", "G1 X4"]);
        assert_eq!(f.len(), 4);
        assert_eq!(f.command_count(), 4);
        let f = GcodeFile::from_text("a", "a\n\nb\n");
        assert_eq!(lines(&f), vec!["a", "", "b"]);
        let f = GcodeFile::from_text("a", "a\r\r\nb\n\r");
        assert_eq!(lines(&f), vec!["a", "", "b", ""]);
        let f = GcodeFile::from_text("a", "\n");
        assert_eq!(lines(&f), vec![""]);
        let f = GcodeFile::from_text("a", "\r\n\r\n");
        assert_eq!(lines(&f), vec!["", ""]);
    }

    #[test]
    fn empty_file() {
        let f = GcodeFile::from_text("empty", "");
        assert!(f.is_empty());
        assert_eq!(f.len(), 0);
        assert_eq!(f.line(0), "");
        assert_eq!(f.command(0), None);
        assert_eq!(f.byte_offset(0), 0);
        assert_eq!(f.command_count(), 0);
        assert_eq!(f.lines().count(), 0);
        assert_eq!(f.line_at_offset(10), 0);
    }

    #[test]
    fn offsets_and_commands() {
        let text = "; header\nG28 ; home\n\nM117 Hi\r\nG1 X1";
        let f = GcodeFile::from_text("t.gcode", text);
        assert_eq!(f.name(), "t.gcode");
        assert_eq!(f.path(), None);
        assert_eq!(f.byte_len(), text.len());
        assert_eq!(f.byte_offset(1), 9);
        assert_eq!(f.byte_offset(99), text.len());
        assert_eq!(f.command(0), None);
        assert_eq!(f.command(1).as_deref(), Some("G28"));
        assert_eq!(f.command(3).as_deref(), Some("M117 Hi"));
        assert_eq!(f.command(4).as_deref(), Some("G1 X1"));
        assert_eq!(f.command_count(), 3);
        assert_eq!(f.line(100), "");
        assert_eq!(f.line_at_offset(0), 0);
        assert_eq!(f.line_at_offset(9), 1);
        assert_eq!(f.line_at_offset(12), 1);
        assert_eq!(f.line_at_offset(text.len() + 5), 4);
        assert_eq!(f.text(), text);
    }

    #[test]
    fn lossy_utf8_and_bom() {
        let mut bytes = b"\xEF\xBB\xBFG28\n".to_vec();
        bytes.extend_from_slice(b"M117 caf\xE9\nG1 X1\n");
        let f = GcodeFile::from_bytes("x", bytes);
        assert_eq!(f.line(0), "G28");
        assert_eq!(f.line(1), "M117 caf\u{FFFD}");
        assert_eq!(f.command_count(), 3);
    }

    #[test]
    fn load_from_disk() {
        let dir = std::env::temp_dir().join(format!("sprint-gcode-file-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("load-test.gcode");
        let written = std::fs::write(&path, "G28\r\nG1 X5\r\n");
        assert!(written.is_ok());
        let f = GcodeFile::load(&path);
        assert!(f.is_ok());
        if let Ok(f) = f {
            assert_eq!(f.name(), "load-test.gcode");
            assert_eq!(f.path(), Some(path.as_path()));
            assert_eq!(lines(&f), vec!["G28", "G1 X5"]);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        assert!(GcodeFile::load(dir.join("missing.gcode")).is_err());
    }

    #[test]
    fn content_is_sniffed_at_load() {
        // Audit A15: an ASCII STL loads (any text does) but is not G-code,
        // although every line yields a "command".
        let stl = "solid cube\nfacet normal 0 0 -1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 1 1 0\nendloop\nendfacet\nendsolid cube\n";
        let f = GcodeFile::from_text("part.stl", stl);
        assert_eq!(f.command_count(), 9);
        assert_eq!(f.content_kind(), ContentKind::AsciiStl);
        assert!(!f.content_kind().is_gcode());
        let f = GcodeFile::from_bytes("part.stl", stl.as_bytes().to_vec());
        assert_eq!(f.content_kind(), ContentKind::AsciiStl);
        // Sniffed from the raw bytes, before lossy decoding.
        let mut stl = vec![0x80u8; 80];
        stl.extend_from_slice(&1u32.to_le_bytes());
        stl.extend_from_slice(&[0xC1; 50]);
        assert_eq!(
            GcodeFile::from_bytes("b.stl", stl).content_kind(),
            ContentKind::BinaryStl
        );
        assert_eq!(
            GcodeFile::from_text("t.gcode", "\u{FEFF}G28\nG1 X1\n").content_kind(),
            ContentKind::Gcode
        );
        assert_eq!(
            GcodeFile::from_text("e", "").content_kind(),
            ContentKind::Empty
        );
        assert_eq!(GcodeFile::default().content_kind(), ContentKind::Empty);
        assert!(format!("{:?}", GcodeFile::default()).contains("Empty"));
    }

    #[test]
    fn debug_is_compact() {
        let f = GcodeFile::from_text("big", "G1 X1\n".repeat(1000));
        let s = format!("{f:?}");
        assert!(s.contains("lines: 1000"));
        assert!(s.len() < 200);
    }
}
