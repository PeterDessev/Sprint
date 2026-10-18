//! SD card / media replies (M20-M33).

use super::text::{strip_prefix_ci, tokens_with_offsets, unquote};
use super::{Reply, SdFile, SdReply};

fn u64_digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Strip one trailing period ("x.gco." -> "x.gco").
fn strip_period(s: &str) -> &str {
    let s = s.trim();
    s.strip_suffix('.').unwrap_or(s).trim()
}

/// True for the start of a file listing.
pub(crate) fn is_list_begin(line: &str) -> bool {
    line.eq_ignore_ascii_case("Begin file list")
}

/// True for the end of a file listing.
pub(crate) fn is_list_end(line: &str) -> bool {
    line.eq_ignore_ascii_case("End file list")
}

/// Parse an SD status message (with the `echo:` prefix already removed).
/// "open failed, File: x.gco." is reported as a non-fatal [`Reply::Error`].
pub(crate) fn parse_sd_message(body: &str) -> Option<Reply> {
    let sd = |r: SdReply| Some(Reply::SdCard(r));
    let lower = body.to_ascii_lowercase();

    if is_list_begin(body) {
        return sd(SdReply::ListBegin);
    }
    if is_list_end(body) {
        return sd(SdReply::ListEnd);
    }
    if lower.starts_with("sd card ok") || lower == "media ok" {
        return sd(SdReply::CardOk);
    }
    if lower.starts_with("no sd card") || lower.starts_with("no media") {
        return sd(SdReply::NoCard);
    }
    if lower.contains("init fail")
        || lower.starts_with("volume.init failed")
        || lower.starts_with("openroot failed")
        || lower.starts_with("workdir open failed")
    {
        return sd(SdReply::CardFailed(body.to_string()));
    }
    if let Some(rest) = strip_prefix_ci(body, "File opened:") {
        // "File opened: NAME Size: 12345" (the name may contain spaces).
        let (name, size) = match rest.rfind(" Size:") {
            Some(i) => (&rest[..i], u64_digits(rest[i + 6..].trim()).unwrap_or(0)),
            None => (rest, 0),
        };
        return sd(SdReply::Opened {
            name: name.trim().to_string(),
            size,
        });
    }
    for prefix in ["Now fresh file:", "Now doing file:"] {
        if let Some(name) = strip_prefix_ci(body, prefix) {
            return sd(SdReply::Opened {
                name: name.trim().to_string(),
                size: 0,
            });
        }
    }
    if lower.starts_with("file selected") {
        return sd(SdReply::Selected);
    }
    if let Some(rest) = strip_prefix_ci(body, "SD printing byte") {
        let (done, total) = rest.trim().split_once('/')?;
        return sd(SdReply::Progress {
            done: u64_digits(done.trim())?,
            total: u64_digits(total.trim())?,
        });
    }
    if lower.starts_with("not sd printing") {
        return sd(SdReply::NotPrinting);
    }
    if lower.starts_with("done printing file") {
        return sd(SdReply::DonePrinting);
    }
    if let Some(name) = strip_prefix_ci(body, "Writing to file:") {
        return sd(SdReply::WritingTo(name.trim().to_string()));
    }
    if lower.starts_with("done saving file") {
        return sd(SdReply::Saved);
    }
    if let Some(name) = strip_prefix_ci(body, "File deleted:") {
        return sd(SdReply::Deleted(strip_period(name).to_string()));
    }
    if let Some(rest) = strip_prefix_ci(body, "Deletion failed") {
        let name = rest
            .trim_start_matches(',')
            .trim()
            .strip_prefix("File:")
            .unwrap_or(rest);
        return sd(SdReply::DeleteFailed(strip_period(name).to_string()));
    }
    if lower.starts_with("open failed") {
        return Some(Reply::Error {
            message: body.to_string(),
            fatal: false,
        });
    }
    None
}

/// Parse one entry of an M20 listing: `NAME [SIZE] [0xTIMESTAMP] [LONG NAME]`,
/// e.g. "CUBE~1.GCO 12345 cube_long_name.gcode" or "/SUBDIR/FILE.GCO 123".
/// When the second word is not a size, the name itself contains spaces:
/// Klipper's `my part.gcode 1234` (size last) or RRF's bare `my part.gcode`.
pub(crate) fn parse_sd_file(line: &str) -> Option<SdFile> {
    let line = line.trim();
    let toks = tokens_with_offsets(line);
    let &(_, name) = toks.first()?;
    if name.contains(':') {
        return None;
    }
    let mut next = 1;
    let size = toks.get(1).and_then(|&(_, t)| u64_digits(t));
    if size.is_none() && toks.len() > 1 {
        // FAT names never contain ':', and status lines usually do.
        if line.contains(':') {
            return None;
        }
        let (name, size) = match toks.last() {
            Some(&(offset, last)) if u64_digits(last).is_some() => {
                (line[..offset].trim(), u64_digits(last))
            }
            _ => (line, None),
        };
        return Some(SdFile {
            name: name.to_string(),
            long_name: None,
            size,
        });
    }
    if size.is_some() {
        next = 2;
        // EXTENDED_M20 timestamp ("M20 T"): 0x5A3B1C2D.
        if let Some(&(_, t)) = toks.get(2)
            && let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))
            && !hex.is_empty()
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            next = 3;
        }
    }
    let long_name = toks
        .get(next)
        .map(|&(offset, _)| unquote(line[offset..].trim()).to_string())
        .filter(|s| !s.is_empty());
    Some(SdFile {
        name: name.to_string(),
        long_name,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(body: &str) -> Option<SdReply> {
        match parse_sd_message(body) {
            Some(Reply::SdCard(r)) => Some(r),
            _ => None,
        }
    }

    #[test]
    fn messages() {
        assert_eq!(msg("Begin file list"), Some(SdReply::ListBegin));
        assert_eq!(msg("End file list"), Some(SdReply::ListEnd));
        assert_eq!(msg("SD card ok"), Some(SdReply::CardOk));
        assert_eq!(
            msg("SD init fail"),
            Some(SdReply::CardFailed("SD init fail".into()))
        );
        assert_eq!(msg("No SD card"), Some(SdReply::NoCard));
        assert_eq!(msg("No media"), Some(SdReply::NoCard));
        assert_eq!(
            msg("File opened: x.gco Size: 12345"),
            Some(SdReply::Opened {
                name: "x.gco".into(),
                size: 12345
            })
        );
        assert_eq!(
            msg("File opened: my part.gco Size: 7"),
            Some(SdReply::Opened {
                name: "my part.gco".into(),
                size: 7
            })
        );
        assert_eq!(
            msg("Now fresh file: x.gco"),
            Some(SdReply::Opened {
                name: "x.gco".into(),
                size: 0
            })
        );
        assert_eq!(
            msg("Now doing file: x.gco"),
            Some(SdReply::Opened {
                name: "x.gco".into(),
                size: 0
            })
        );
        assert_eq!(msg("File selected"), Some(SdReply::Selected));
        assert_eq!(
            msg("SD printing byte 1234/5678"),
            Some(SdReply::Progress {
                done: 1234,
                total: 5678
            })
        );
        assert_eq!(msg("SD printing byte 1234/x"), None);
        assert_eq!(msg("Not SD printing"), Some(SdReply::NotPrinting));
        assert_eq!(msg("Not SD printing."), Some(SdReply::NotPrinting));
        assert_eq!(msg("Done printing file"), Some(SdReply::DonePrinting));
        assert_eq!(
            msg("Writing to file: x.gco"),
            Some(SdReply::WritingTo("x.gco".into()))
        );
        assert_eq!(msg("Done saving file."), Some(SdReply::Saved));
        assert_eq!(
            msg("File deleted:x.gco"),
            Some(SdReply::Deleted("x.gco".into()))
        );
        assert_eq!(
            msg("Deletion failed, File: x.gco."),
            Some(SdReply::DeleteFailed("x.gco".into()))
        );
        assert_eq!(
            parse_sd_message("open failed, File: x.gco."),
            Some(Reply::Error {
                message: "open failed, File: x.gco.".into(),
                fatal: false
            })
        );
        assert_eq!(parse_sd_message("hello"), None);
    }

    #[test]
    fn file_entries() {
        let f = |name: &str, size: Option<u64>, long: Option<&str>| SdFile {
            name: name.into(),
            size,
            long_name: long.map(Into::into),
        };
        assert_eq!(
            parse_sd_file("CUBE~1.GCO 12345"),
            Some(f("CUBE~1.GCO", Some(12345), None))
        );
        assert_eq!(
            parse_sd_file("CUBE~1.GCO 12345 cube_long_name.gcode"),
            Some(f("CUBE~1.GCO", Some(12345), Some("cube_long_name.gcode")))
        );
        assert_eq!(
            parse_sd_file("BENCHY~1.GCO 99 3D Benchy  final.gcode"),
            Some(f("BENCHY~1.GCO", Some(99), Some("3D Benchy  final.gcode")))
        );
        assert_eq!(
            parse_sd_file("/SUBDIR/FILE.GCO 123"),
            Some(f("/SUBDIR/FILE.GCO", Some(123), None))
        );
        assert_eq!(
            parse_sd_file("CUBE~1.GCO 12345 0x5A3B1C2D \"cube.gcode\""),
            Some(f("CUBE~1.GCO", Some(12345), Some("cube.gcode")))
        );
        assert_eq!(parse_sd_file("OLD.GCO"), Some(f("OLD.GCO", None, None)));
        assert_eq!(parse_sd_file(""), None);
        assert_eq!(parse_sd_file("T:20.0 /0.0"), None);
        // Names with spaces: Klipper (size last) and RRF (no size).
        assert_eq!(
            parse_sd_file("my part.gcode 1234"),
            Some(f("my part.gcode", Some(1234), None))
        );
        assert_eq!(
            parse_sd_file("my  part.gcode"),
            Some(f("my  part.gcode", None, None))
        );
        assert_eq!(parse_sd_file("Mesh X,Y: 3,2"), None);
        assert_eq!(parse_sd_file("x:open y:open z:TRIGGERED"), None);
    }
}
