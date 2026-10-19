//! Content sniffing: tells G-code apart from 3D models, archives, binary
//! data and other text, so front ends can refuse to stream a file that is
//! not G-code (an ASCII STL is valid text and every line would otherwise be
//! sent to the printer as an unknown command).

use std::io::Read;
use std::path::Path;

use serde::Serialize;

use crate::parse::parse_line;

/// How much of a file [`sniff_file`] reads and [`sniff_content`] looks at.
pub const SNIFF_BYTES: usize = 64 * 1024;

/// Most non-blank lines examined by the text heuristics.
const SNIFF_LINES: usize = 4096;

/// What a file contains, judged by [`sniff_content`] from its first bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub enum ContentKind {
    /// G-code: most lines are G/M/T commands, parameter words, upper-case
    /// extended commands (Klipper macros) or comments. A file with only
    /// comments is G-code too (there is nothing to send).
    #[default]
    Gcode,
    /// Nothing but whitespace.
    Empty,
    /// ASCII STL model (`solid` ... `facet normal` ... `endsolid`).
    AsciiStl,
    /// Binary STL model (80-byte header, triangle count, 50 bytes per
    /// triangle).
    BinaryStl,
    /// ZIP archive, typically a 3MF project.
    Zip3mf,
    /// Wavefront OBJ model (`v` / `f` statements).
    Obj,
    /// STEP CAD model (ISO 10303-21).
    Step,
    /// Prusa binary G-code (`.bgcode`, magic `GCDE`).
    BinaryGcode,
    /// Other binary data.
    Binary,
    /// Text that does not look like G-code.
    OtherText,
}

impl ContentKind {
    /// True for G-code that can be streamed.
    pub fn is_gcode(self) -> bool {
        self == ContentKind::Gcode
    }

    /// True for 3D models and model archives, which need slicing first.
    pub fn is_model(self) -> bool {
        matches!(
            self,
            ContentKind::AsciiStl
                | ContentKind::BinaryStl
                | ContentKind::Zip3mf
                | ContentKind::Obj
                | ContentKind::Step
        )
    }

    /// Short description for messages ("an ASCII STL model").
    pub fn description(self) -> &'static str {
        match self {
            ContentKind::Gcode => "G-code",
            ContentKind::Empty => "an empty file",
            ContentKind::AsciiStl => "an ASCII STL model",
            ContentKind::BinaryStl => "a binary STL model",
            ContentKind::Zip3mf => "a ZIP archive (3MF project)",
            ContentKind::Obj => "an OBJ model",
            ContentKind::Step => "a STEP model",
            ContentKind::BinaryGcode => "binary G-code (.bgcode)",
            ContentKind::Binary => "a binary file",
            ContentKind::OtherText => "a text file that is not G-code",
        }
    }

    /// Why the file cannot be printed as it is, as a sentence for the user;
    /// `None` for G-code. Front ends refuse to stream such files.
    pub fn problem(self) -> Option<&'static str> {
        Some(match self {
            ContentKind::Gcode => return None,
            ContentKind::Empty => "The file is empty.",
            ContentKind::AsciiStl
            | ContentKind::BinaryStl
            | ContentKind::Zip3mf
            | ContentKind::Obj
            | ContentKind::Step => "This is a 3D model, not G-code: slice it first.",
            ContentKind::BinaryGcode => {
                "This is binary G-code (.bgcode): export plain G-code from the slicer instead."
            }
            ContentKind::Binary => "This is a binary file, not G-code.",
            ContentKind::OtherText => "This text file does not look like G-code.",
        })
    }
}

/// Classify a file from its first bytes (`head`, ideally the first
/// [`SNIFF_BYTES`] or the whole file) and its total size in bytes.
///
/// Checks, in order: emptiness, ZIP and binary G-code magic numbers, the
/// binary STL size equation, binary bytes (NUL or many control/invalid
/// UTF-8 bytes), STEP and ASCII STL headers, OBJ statements, and finally the
/// share of lines that read as G-code. Never panics.
pub fn sniff_content(head: &[u8], total_len: u64) -> ContentKind {
    let head = &head[..head.len().min(SNIFF_BYTES)];
    // The head may end inside a multi-byte character or a line.
    let complete = head.len() as u64 >= total_len;
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    if head.iter().all(u8::is_ascii_whitespace) {
        return if complete {
            ContentKind::Empty
        } else {
            // Only blanks to look at although the file goes on.
            ContentKind::OtherText
        };
    }
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        return ContentKind::Zip3mf;
    }
    if head.starts_with(b"GCDE") {
        return ContentKind::BinaryGcode;
    }
    if is_binary_stl(head, total_len) {
        return ContentKind::BinaryStl;
    }
    let text = String::from_utf8_lossy(head);
    if looks_binary(&text, complete) {
        return ContentKind::Binary;
    }
    let mut lines: Vec<&str> = text.lines().collect();
    if !complete && lines.len() > 1 {
        lines.pop();
    }
    sniff_text(&lines)
}

/// Classify the file at `path` from its first [`SNIFF_BYTES`] and its size,
/// without reading the rest.
pub fn sniff_file(path: impl AsRef<Path>) -> std::io::Result<ContentKind> {
    let file = std::fs::File::open(path)?;
    let total = file.metadata()?.len();
    let mut head = Vec::with_capacity(SNIFF_BYTES.min(usize::try_from(total).unwrap_or(0)));
    file.take(SNIFF_BYTES as u64).read_to_end(&mut head)?;
    Ok(sniff_content(&head, total))
}

/// 80-byte header, little-endian triangle count, 50 bytes per triangle,
/// and nothing else: exactly the file size.
fn is_binary_stl(head: &[u8], total_len: u64) -> bool {
    let Some(count) = head.get(80..84) else {
        return false;
    };
    let count = u64::from(u32::from_le_bytes([count[0], count[1], count[2], count[3]]));
    84 + 50 * count == total_len
}

/// NUL characters, or more than a quarter control characters and invalid
/// UTF-8 (text in a legacy encoding such as Latin-1 has far fewer).
fn looks_binary(text: &str, complete: bool) -> bool {
    let mut total = 0usize;
    let mut bad = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        // A replacement character at the very end of a truncated head is a
        // multi-byte character that was cut, not bad data.
        if c == '\u{FFFD}' && chars.peek().is_none() && !complete {
            break;
        }
        total += 1;
        match c {
            '\0' => return true,
            '\t' | '\n' | '\r' | '\x0c' => {}
            '\u{FFFD}' => bad += 1,
            c if c.is_control() => bad += 1,
            _ => {}
        }
    }
    bad * 4 > total
}

/// Line classes counted by [`sniff_text`].
#[derive(Default)]
struct Tally {
    comments: usize,
    gcode: usize,
    obj: usize,
    obj_vertices: usize,
    obj_faces: usize,
    foreign: usize,
}

fn sniff_text(lines: &[&str]) -> ContentKind {
    let mut nonblank = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(SNIFF_LINES);
    let Some(first) = nonblank.clone().next() else {
        return ContentKind::Empty;
    };
    if first.starts_with("ISO-10303-21") {
        return ContentKind::Step;
    }
    if starts_with_word(first, "solid")
        && nonblank
            .clone()
            .skip(1)
            .take(64)
            .any(|l| starts_with_word(l, "facet") || starts_with_word(l, "endsolid"))
    {
        return ContentKind::AsciiStl;
    }
    let mut tally = Tally::default();
    for line in nonblank.by_ref() {
        classify_line(line, &mut tally);
    }
    let statements = tally.gcode + tally.foreign;
    if tally.obj_vertices > 0 && tally.obj_faces > 0 && tally.obj * 5 >= statements * 4 {
        return ContentKind::Obj;
    }
    if tally.gcode == 0 {
        return if tally.foreign == 0 && tally.comments > 0 {
            ContentKind::Gcode
        } else {
            ContentKind::OtherText
        };
    }
    if tally.gcode >= tally.foreign {
        ContentKind::Gcode
    } else {
        ContentKind::OtherText
    }
}

/// True when `line` starts with `word` (ASCII case-insensitive) followed by
/// whitespace or the end of the line.
fn starts_with_word(line: &str, word: &str) -> bool {
    line.get(..word.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(word))
        && line[word.len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

/// RepRapFirmware meta-command keywords (conditional G-code).
const RRF_META: [&str; 11] = [
    "if", "elif", "else", "while", "break", "continue", "var", "global", "set", "echo", "abort",
];

/// OBJ statements.
const OBJ_STATEMENTS: [&str; 11] = [
    "v", "vn", "vt", "vp", "f", "l", "o", "g", "s", "mtllib", "usemtl",
];

fn classify_line(line: &str, tally: &mut Tally) {
    let first_word = line.split_whitespace().next().unwrap_or("");
    if OBJ_STATEMENTS.contains(&first_word) {
        tally.obj += 1;
        match first_word {
            "v" => tally.obj_vertices += 1,
            "f" => tally.obj_faces += 1,
            _ => {}
        }
    } else if first_word.starts_with('#') {
        // OBJ (and shell, Python, ...) comments: not G-code.
        tally.obj += 1;
    }
    if line.starts_with([';', '(', '%']) {
        tally.comments += 1;
        return;
    }
    let parsed = parse_line(line);
    let gcode = match (parsed.code, parsed.text) {
        (Some(code), _) => matches!(code.letter, b'G' | b'M' | b'T'),
        // Parameter words only ("X10 Y20": a modal move).
        (None, _) if !parsed.words.is_empty() => true,
        (None, Some(text)) => is_extended_command(text),
        // An N word or checksum only.
        (None, None) => parsed.line_number.is_some(),
    };
    if gcode {
        tally.gcode += 1;
    } else {
        tally.foreign += 1;
    }
}

/// Upper-case extended commands (Klipper `PRINT_START`, `SET_FAN_SPEED
/// SPEED=1`), host commands (`@pause`) and RepRapFirmware meta commands.
fn is_extended_command(text: &str) -> bool {
    let name = text.split_whitespace().next().unwrap_or("");
    if name.starts_with('@') && name.len() > 1 {
        return true;
    }
    if RRF_META.contains(&name) {
        return true;
    }
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_uppercase())
        && name.len() >= 2
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff(text: &str) -> ContentKind {
        sniff_content(text.as_bytes(), text.len() as u64)
    }

    const ASCII_STL: &str = "solid cube\n  facet normal 0 0 -1\n    outer loop\n      vertex 0 0 0\n      vertex 1 0 0\n      vertex 1 1 0\n    endloop\n  endfacet\nendsolid cube\n";

    #[test]
    fn ascii_stl_is_a_model() {
        // The audit's example: every line is "valid" text for clean_line.
        let kind = sniff(ASCII_STL);
        assert_eq!(kind, ContentKind::AsciiStl);
        assert!(kind.is_model() && !kind.is_gcode());
        assert_eq!(
            kind.problem(),
            Some("This is a 3D model, not G-code: slice it first.")
        );
        assert_eq!(
            sniff("SOLID x\r\nfacet normal 0 0 1\r\n"),
            ContentKind::AsciiStl
        );
        // A G-code comment that happens to start with "solid".
        assert_eq!(sniff("; solid infill\nG1 X1\n"), ContentKind::Gcode);
    }

    #[test]
    fn binary_stl_by_size() {
        let mut stl = b"solid header written by a CAD tool".to_vec();
        stl.resize(80, b' ');
        stl.extend_from_slice(&2u32.to_le_bytes());
        stl.extend(std::iter::repeat_n(0x41u8, 100));
        assert_eq!(
            sniff_content(&stl, stl.len() as u64),
            ContentKind::BinaryStl
        );
        // One byte more and it is not an STL (but still binary-free text).
        assert_ne!(
            sniff_content(&stl, stl.len() as u64 + 1),
            ContentKind::BinaryStl
        );
    }

    #[test]
    fn archives_and_binary() {
        assert_eq!(
            sniff_content(b"PK\x03\x04\x14\x00rest", 100),
            ContentKind::Zip3mf
        );
        assert_eq!(
            sniff_content(b"GCDE\x01\x00\x00\x00", 1000),
            ContentKind::BinaryGcode
        );
        assert_eq!(sniff_content(b"G1 X1\0\0\0", 8), ContentKind::Binary);
        // Binary without NUL bytes.
        let noise: Vec<u8> = (0..2000u32).map(|i| (i * 37 % 250 + 1) as u8).collect();
        assert_eq!(sniff_content(&noise, 2000), ContentKind::Binary);
        assert!(ContentKind::Binary.problem().is_some());
    }

    #[test]
    fn text_models() {
        assert_eq!(
            sniff("ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('x'),'2;1');\n"),
            ContentKind::Step
        );
        let obj = "# Blender\nmtllib cube.mtl\no Cube\nv 1 1 -1\nv 1 -1 -1\nv 1 1 1\nvn 0 1 0\ns off\nf 1/1/1 2/2/1 3/3/1\n";
        assert_eq!(sniff(obj), ContentKind::Obj);
    }

    #[test]
    fn gcode_flavours() {
        assert_eq!(
            sniff("G28\nG1 X10 Y10 F3000\nM104 S200\n"),
            ContentKind::Gcode
        );
        assert_eq!(sniff("g28\nn5 g1 x1*12\n"), ContentKind::Gcode);
        // Klipper macros, host commands, RRF meta commands, modal words.
        assert_eq!(
            sniff("PRINT_START BED=60\nSET_FAN_SPEED FAN=part SPEED=1\n@pause\nX10 Y20\n"),
            ContentKind::Gcode
        );
        assert_eq!(
            sniff("if move.axes[0].homed\n  G1 X10\nelse\n  G28\n"),
            ContentKind::Gcode
        );
        // Only comments (a slicer header cut off by the sniff limit).
        assert_eq!(
            sniff("; thumbnail begin\n; iVBORw0KGgo\n"),
            ContentKind::Gcode
        );
        // Latin-1 in a comment is not binary.
        let mut latin = b"; caf\xE9\nG28\nG1 X1\n".to_vec();
        latin.extend_from_slice(b"M117 \xE9t\xE9\n");
        assert_eq!(
            sniff_content(&latin, latin.len() as u64),
            ContentKind::Gcode
        );
        // A BOM is skipped.
        assert_eq!(sniff("\u{FEFF}G28\n"), ContentKind::Gcode);
    }

    #[test]
    fn other_text_and_empty() {
        assert_eq!(sniff(""), ContentKind::Empty);
        assert_eq!(sniff(" \n\r\n\t"), ContentKind::Empty);
        assert_eq!(sniff("\u{FEFF}"), ContentKind::Empty);
        assert_eq!(
            sniff("# Notes\n\nPrint this at 210 degrees.\nThen sand it.\n"),
            ContentKind::OtherText
        );
        assert_eq!(sniff("a,b,c\n1,2,3\n4,5,6\n"), ContentKind::OtherText);
        assert_eq!(
            sniff("M117 hello\nthis is text\nand more text\n"),
            ContentKind::OtherText
        );
        assert_eq!(
            ContentKind::OtherText.description(),
            "a text file that is not G-code"
        );
    }

    #[test]
    fn truncated_heads() {
        // A head cut inside a multi-byte character and inside a line.
        let mut text = "G1 X1 ; café\n".repeat(10);
        text.push_str("G1 X2 ; é");
        let bytes = text.as_bytes();
        let cut = &bytes[..bytes.len() - 1];
        assert_eq!(sniff_content(cut, 1_000_000), ContentKind::Gcode);
        // Head longer than the sniff window.
        let big = "G1 X1 Y1\n".repeat(20_000);
        assert_eq!(sniff(&big), ContentKind::Gcode);
    }

    #[test]
    fn sniff_file_reads_only_the_head() {
        let dir = std::env::temp_dir().join(format!("sprint-gcode-sniff-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("part.stl");
        assert!(std::fs::write(&path, ASCII_STL).is_ok());
        assert_eq!(sniff_file(&path).ok(), Some(ContentKind::AsciiStl));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        assert!(sniff_file(dir.join("missing.gcode")).is_err());
    }

    #[test]
    fn never_panics_on_odd_input() {
        for bytes in [
            &b""[..],
            b"\xEF\xBB",
            b"\xFF\xFE\xFD",
            b"PK",
            b"solid",
            b"solid\n",
            b"\n\n\n;",
            b"(((",
        ] {
            let _ = sniff_content(bytes, bytes.len() as u64);
            let _ = sniff_content(bytes, u64::MAX);
            let _ = sniff_content(bytes, 0);
        }
    }
}
