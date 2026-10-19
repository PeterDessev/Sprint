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
/// byte-order mark is dropped, so the text can be shorter or longer than the
/// source: [`GcodeFile::byte_len`] and [`GcodeFile::byte_offset`] index the
/// text, [`GcodeFile::source_len`] and [`GcodeFile::source_offset`] the
/// source bytes (sizes and progress shown to the user).
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
    /// Size of the source in bytes (on disk), BOM and invalid bytes included.
    source_len: u64,
    /// Maps text offsets to source offsets: from `text_start` on, the source
    /// offset is `text offset + delta` (until the next entry). Empty when
    /// both are the same; more than one entry only when lossy decoding
    /// changed lengths.
    offset_map: Vec<OffsetShift>,
}

/// One entry of [`GcodeFile`]'s text-to-source offset map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OffsetShift {
    text_start: usize,
    delta: i64,
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
            source_len: 0,
            offset_map: Vec::new(),
        }
    }
}

impl fmt::Debug for GcodeFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GcodeFile")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("lines", &self.len())
            .field("bytes", &self.source_len)
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

    /// Build from text already in memory (its UTF-8 bytes are the source).
    pub fn from_text(name: impl Into<String>, text: impl Into<String>) -> GcodeFile {
        let text = text.into();
        let content = sniff_content(text.as_bytes(), text.len() as u64);
        let source_len = text.len() as u64;
        GcodeFile::build(name.into(), text, content, source_len, Vec::new())
    }

    /// Build from raw bytes (lossy UTF-8).
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> GcodeFile {
        let content = sniff_content(&bytes, bytes.len() as u64);
        let source_len = bytes.len() as u64;
        let (text, shifts) = match String::from_utf8(bytes) {
            Ok(s) => (s, Vec::new()),
            Err(e) => decode_lossy(e.as_bytes()),
        };
        GcodeFile::build(name.into(), text, content, source_len, shifts)
    }

    /// `shifts` maps offsets of `text` (before the BOM is dropped) to source
    /// offsets where lossy decoding changed lengths.
    fn build(
        name: String,
        mut text: String,
        content: ContentKind,
        source_len: u64,
        shifts: Vec<OffsetShift>,
    ) -> GcodeFile {
        let mut bom = 0usize;
        if text.starts_with('\u{FEFF}') {
            bom = '\u{FEFF}'.len_utf8();
            text.drain(..bom);
        }
        // Dropping the BOM moves the text, not the source.
        let bom_delta = bom as i64;
        let mut offset_map = Vec::new();
        if bom > 0 || !shifts.is_empty() {
            offset_map.push(OffsetShift {
                text_start: 0,
                delta: bom_delta,
            });
            offset_map.extend(shifts.into_iter().map(|s| OffsetShift {
                text_start: s.text_start.saturating_sub(bom),
                delta: s.delta + bom_delta,
            }));
        }
        let (starts, command_count) = index_lines(&text);
        GcodeFile {
            name,
            path: None,
            text,
            starts,
            command_count,
            content,
            source_len,
            offset_map,
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

    /// Size of the decoded text in bytes (what [`GcodeFile::text`] holds).
    /// For the size of the file as stored, use [`GcodeFile::source_len`].
    pub fn byte_len(&self) -> usize {
        self.text.len()
    }

    /// Offset in [`GcodeFile::text`] of the start of line `index`
    /// (`byte_len()` when out of range). For progress through the file as
    /// stored, use [`GcodeFile::source_offset`].
    pub fn byte_offset(&self, index: usize) -> usize {
        self.starts.get(index).copied().unwrap_or(self.text.len())
    }

    /// Size of the source in bytes: the file on disk (or the bytes or text
    /// it was built from), including a byte-order mark and invalid UTF-8
    /// that the text drops or replaces. Report this as the file size.
    pub fn source_len(&self) -> u64 {
        self.source_len
    }

    /// Offset in the source bytes of the start of line `index`
    /// (`source_len()` when out of range): the bytes done once streaming
    /// has reached line `index`.
    /// A byte-order mark counts as part of the first line, so line 0 starts
    /// at 0.
    pub fn source_offset(&self, index: usize) -> u64 {
        if index >= self.len() {
            return self.source_len;
        }
        if index == 0 {
            return 0;
        }
        let text_offset = self.byte_offset(index);
        let n = self
            .offset_map
            .partition_point(|s| s.text_start <= text_offset);
        let delta = n
            .checked_sub(1)
            .and_then(|i| self.offset_map.get(i))
            .map_or(0, |s| s.delta);
        let offset = i64::try_from(text_offset)
            .unwrap_or(i64::MAX)
            .saturating_add(delta);
        u64::try_from(offset).unwrap_or(0).min(self.source_len)
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

/// Lossy UTF-8 decoding (each maximal invalid sequence becomes one U+FFFD,
/// as with `String::from_utf8_lossy`) that records where text offsets stop
/// matching source offsets.
///
/// Offsets are only mapped at line starts, so a shift is kept only when a
/// line break follows it before the next one: binary data, with an invalid
/// sequence every byte or two, gets at most one entry per line.
fn decode_lossy(bytes: &[u8]) -> (String, Vec<OffsetShift>) {
    let mut text = String::with_capacity(bytes.len() + 16);
    let mut shifts: Vec<OffsetShift> = Vec::new();
    let mut source = 0usize;
    for chunk in bytes.utf8_chunks() {
        let valid = chunk.valid();
        text.push_str(valid);
        source += valid.len();
        let invalid = chunk.invalid();
        if !invalid.is_empty() {
            text.push(char::REPLACEMENT_CHARACTER);
            source += invalid.len();
            let shift = OffsetShift {
                text_start: text.len(),
                delta: source as i64 - text.len() as i64,
            };
            // No line starts between the last shift and this one: it is
            // never looked up.
            match shifts.last_mut() {
                Some(last) if !valid.contains(['\n', '\r']) => *last = shift,
                _ => shifts.push(shift),
            }
        }
    }
    (text, shifts)
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
    fn sizes_and_offsets_count_source_bytes() {
        // Audit A41: a BOM and invalid UTF-8 changed the reported size.
        let mut bytes = b"\xEF\xBB\xBFG28\n".to_vec();
        bytes.extend_from_slice(b"M117 caf\xE9 \xFF\xFE!\r\n");
        bytes.extend_from_slice(b"; \xC3\n\nG1 X1\n");
        let f = GcodeFile::from_bytes("x", bytes.clone());
        assert_eq!(f.source_len(), bytes.len() as u64);
        assert_ne!(f.byte_len(), bytes.len());
        assert_eq!(f.len(), 5);
        // Every line starts where it starts on disk.
        let mut expected = vec![0u64];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' && i + 1 < bytes.len() {
                expected.push(i as u64 + 1);
            }
        }
        let offsets: Vec<u64> = (0..f.len()).map(|i| f.source_offset(i)).collect();
        assert_eq!(offsets, expected);
        assert_eq!(f.source_offset(f.len()), f.source_len());
        assert_eq!(f.source_offset(usize::MAX), f.source_len());
        assert!(format!("{f:?}").contains(&format!("bytes: {}", bytes.len())));
        // Text offsets still index the decoded text.
        assert_eq!(f.text().get(f.byte_offset(4)..), Some("G1 X1\n"));
        // Valid UTF-8 without BOM: both the same.
        let text = "G28\n; caf\u{e9}\nG1 X1";
        let f = GcodeFile::from_bytes("y", text.as_bytes().to_vec());
        assert_eq!(f.source_len(), text.len() as u64);
        assert_eq!(f.source_len(), f.byte_len() as u64);
        for i in 0..=f.len() {
            assert_eq!(f.source_offset(i), f.byte_offset(i) as u64);
        }
        // A BOM only: the first line starts at 0, the others 3 bytes later.
        let f = GcodeFile::from_text("z", "\u{FEFF}G28\nG1 X1\n");
        assert_eq!((f.source_len(), f.byte_len()), (13, 10));
        assert_eq!((f.source_offset(0), f.source_offset(1)), (0, 7));
        assert_eq!(f.byte_offset(1), 4);
        // Only invalid bytes.
        let f = GcodeFile::from_bytes("w", vec![0xFF; 10]);
        assert_eq!((f.source_len(), f.len()), (10, 1));
        assert_eq!(GcodeFile::default().source_len(), 0);
        assert_eq!(GcodeFile::default().source_offset(0), 0);
    }

    #[test]
    fn offset_map_grows_with_lines_not_invalid_bytes() {
        // Binary data has an invalid sequence every byte or two; one map
        // entry each (16 bytes) made a large binary file cost several
        // times its size again. Offsets are only looked up at line starts,
        // so one entry per line is enough.
        let mut bytes = Vec::new();
        for line in 0..50u8 {
            for _ in 0..2_000 {
                bytes.extend_from_slice(&[0xFF, b'a', 0xC3]);
            }
            bytes.push(if line % 2 == 0 { b'\n' } else { b'\r' });
            bytes.extend_from_slice(b"G1 X1 \xE9\r\n");
        }
        let f = GcodeFile::from_bytes("bin", bytes.clone());
        assert_eq!(f.len(), 100);
        assert!(f.offset_map.len() <= f.len() + 1, "{}", f.offset_map.len());
        // Still exact at every line start.
        let mut expected = vec![0u64];
        for (i, pair) in bytes.windows(2).enumerate() {
            let starts_line = match pair {
                [b'\n', _] => true,
                [b'\r', next] => *next != b'\n',
                _ => false,
            };
            if starts_line {
                expected.push(i as u64 + 1);
            }
        }
        let offsets: Vec<u64> = (0..f.len()).map(|i| f.source_offset(i)).collect();
        assert_eq!(offsets, expected);
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
            assert_eq!(f.source_len(), 12);
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
