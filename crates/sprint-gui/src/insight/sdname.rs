//! 8.3 file names for SD card uploads.
//!
//! Marlin creates files with `M28 <name>`; FAT 8.3 names (up to eight
//! characters, a dot and up to three) work on every firmware, long names
//! only on some. Names are upper-cased, as the card stores them.

/// Characters allowed in a name besides `A`–`Z` and `0`–`9`. A conservative
/// subset of what FAT accepts: no spaces and nothing a G-code parser could
/// take for a comment, checksum or parameter (`;`, `*`, `(`).
const EXTRA_CHARS: &[char] = &['_', '-', '~', '!', '#', '$', '%', '&', '@', '^'];
/// Default extension of uploaded G-code files.
pub const DEFAULT_EXTENSION: &str = "GCO";

/// Why a name is not a valid 8.3 name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// Nothing before the dot.
    Empty,
    /// More than eight characters before the dot.
    BaseTooLong,
    /// More than three characters after the dot.
    ExtensionTooLong,
    /// More than one dot.
    TooManyDots,
    /// A character that is not allowed.
    InvalidChar(char),
}

impl NameError {
    /// A sentence for the user.
    pub fn message(&self) -> String {
        match self {
            NameError::Empty => "Enter a file name.".to_owned(),
            NameError::BaseTooLong => "Use at most 8 characters before the dot.".to_owned(),
            NameError::ExtensionTooLong => "Use at most 3 characters after the dot.".to_owned(),
            NameError::TooManyDots => "Use a single dot before the extension.".to_owned(),
            NameError::InvalidChar(' ') => "Spaces are not allowed.".to_owned(),
            NameError::InvalidChar(c) => format!("\u{201c}{c}\u{201d} is not allowed."),
        }
    }
}

fn allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || EXTRA_CHARS.contains(&c)
}

/// Check an 8.3 name typed by the user and return it upper-cased, without
/// surrounding whitespace.
pub fn validate(name: &str) -> Result<String, NameError> {
    let name = name.trim();
    if let Some(c) = name.chars().find(|&c| c != '.' && !allowed(c)) {
        return Err(NameError::InvalidChar(c));
    }
    let mut parts = name.split('.');
    let base = parts.next().unwrap_or("");
    let ext = parts.next();
    if parts.next().is_some() {
        return Err(NameError::TooManyDots);
    }
    if base.is_empty() {
        return Err(NameError::Empty);
    }
    if base.chars().count() > 8 {
        return Err(NameError::BaseTooLong);
    }
    if ext.is_some_and(|e| e.chars().count() > 3) {
        return Err(NameError::ExtensionTooLong);
    }
    Ok(name.to_ascii_uppercase())
}

/// Suggest an 8.3 name for a file name of any length: the stem upper-cased
/// with spaces and dots turned into `_`, other unusable characters dropped,
/// cut to eight characters; the extension's first three usable characters
/// (`GCO` for `.gcode`, or when there is none).
///
/// `taken` are names already on the card (compared case-insensitively); a
/// clash is resolved the way Windows does it (`BENCHY~1.GCO`,
/// `BENCHY~2.GCO`, ...).
pub fn suggest(file_name: &str, taken: &[&str]) -> String {
    let file_name = file_name.trim();
    // Only the file name, not a path.
    let file_name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (file_name, ""),
    };
    let clean = |s: &str, keep_separators: bool| -> String {
        s.chars()
            .filter_map(|c| {
                let c = c.to_ascii_uppercase();
                if allowed(c) {
                    Some(c)
                } else if keep_separators && (c == ' ' || c == '.') {
                    Some('_')
                } else {
                    None
                }
            })
            .collect()
    };
    let mut base: String = clean(stem, true);
    // Runs of underscores from separators read badly.
    while base.contains("__") {
        base = base.replace("__", "_");
    }
    let base = base.trim_matches('_').to_owned();
    let base = if base.is_empty() {
        "PRINT".to_owned()
    } else {
        base
    };
    let ext: String = clean(ext, false).chars().take(3).collect();
    let ext = if ext.is_empty() {
        DEFAULT_EXTENSION.to_owned()
    } else {
        ext
    };
    let is_taken = |candidate: &str| {
        taken
            .iter()
            .any(|t| t.trim().eq_ignore_ascii_case(candidate))
    };
    let short: String = base.chars().take(8).collect();
    let candidate = format!("{short}.{ext}");
    if !is_taken(&candidate) && base.chars().count() <= 8 {
        return candidate;
    }
    for n in 1..=999_999u32 {
        let suffix = format!("~{n}");
        let keep = 8usize.saturating_sub(suffix.len());
        let stem: String = base.chars().take(keep).collect();
        let candidate = format!("{stem}{suffix}.{ext}");
        if !is_taken(&candidate) {
            return candidate;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_are_upper_cased() {
        assert_eq!(validate("cube.gco").as_deref(), Ok("CUBE.GCO"));
        assert_eq!(validate("  BENCHY~1.G ").as_deref(), Ok("BENCHY~1.G"));
        assert_eq!(validate("NOEXT").as_deref(), Ok("NOEXT"));
        assert_eq!(validate("A.").as_deref(), Ok("A."));
    }

    #[test]
    fn invalid_names_are_explained() {
        assert_eq!(validate(""), Err(NameError::Empty));
        assert_eq!(validate(".gco"), Err(NameError::Empty));
        assert_eq!(validate("toolongname.gco"), Err(NameError::BaseTooLong));
        assert_eq!(validate("cube.gcode"), Err(NameError::ExtensionTooLong));
        assert_eq!(validate("a.b.c"), Err(NameError::TooManyDots));
        assert_eq!(validate("my cube.gco"), Err(NameError::InvalidChar(' ')));
        assert_eq!(validate("cube;1.gco"), Err(NameError::InvalidChar(';')));
        assert_eq!(validate("cübe.gco"), Err(NameError::InvalidChar('ü')));
        assert_eq!(
            NameError::InvalidChar(' ').message(),
            "Spaces are not allowed."
        );
        assert!(NameError::InvalidChar('*').message().contains('*'));
    }

    #[test]
    fn suggestions_shorten_long_names() {
        assert_eq!(suggest("cube.gcode", &[]), "CUBE.GCO");
        assert_eq!(suggest("benchy_0.2mm_PLA.gcode", &[]), "BENCHY~1.GCO");
        assert_eq!(suggest("first layer test.gcode", &[]), "FIRST_~1.GCO");
        assert_eq!(suggest("C:\\prints\\part.g", &[]), "PART.G");
        assert_eq!(suggest("/home/me/part", &[]), "PART.GCO");
        assert_eq!(suggest("über teil.gcode", &[]), "BER_TEIL.GCO");
        assert_eq!(suggest("???.gcode", &[]), "PRINT.GCO");
        assert_eq!(suggest("", &[]), "PRINT.GCO");
        assert_eq!(suggest(".gcode", &[]), "GCODE.GCO");
        for name in [
            "cube.gcode",
            "benchy_0.2mm_PLA.gcode",
            "über teil.gcode",
            "",
        ] {
            assert!(validate(&suggest(name, &[])).is_ok(), "{name}");
        }
    }

    #[test]
    fn suggestions_avoid_existing_files() {
        assert_eq!(suggest("cube.gcode", &["CUBE.GCO"]), "CUBE~1.GCO");
        assert_eq!(
            suggest("cube.gcode", &["cube.gco", "CUBE~1.GCO"]),
            "CUBE~2.GCO"
        );
        assert_eq!(
            suggest("benchy_0.2mm_PLA.gcode", &["BENCHY~1.GCO"]),
            "BENCHY~2.GCO"
        );
    }
}
