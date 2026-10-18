//! Line framing: `N<n> <command>*<checksum>` splitting, comment removal and
//! the emergency-parser prefix check.

/// One received line split into its framing parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Framed<'a> {
    /// The `N` word at the start of the line, if any (strtol semantics:
    /// garbage after `N` reads as 0).
    pub number: Option<i64>,
    /// Command text without the `N` word, checksum and comment, trimmed.
    pub command: &'a str,
    /// Checksum value transmitted after the last `*`, if any (compare it
    /// with [`raw_checksum`] of the received bytes).
    pub checksum: Option<i64>,
}

/// Remove a `;` comment (Marlin drops everything after `;`, even inside an
/// M117 message).
pub(crate) fn strip_comment(line: &str) -> &str {
    match line.find(';') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Parse a C `strtol`-style integer prefix: optional whitespace, optional
/// sign, digits. Returns 0 when there are no digits (like strtol).
pub(crate) fn strtol(text: &str) -> i64 {
    let t = text.trim_start();
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut value: i64 = 0;
    for b in digits.bytes() {
        if !b.is_ascii_digit() {
            break;
        }
        value = value.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    if neg { -value } else { value }
}

/// Split a raw line (no line terminator) into its framing parts. Returns
/// `None` for lines that are empty after removing the comment.
pub(crate) fn frame(raw: &str) -> Option<Framed<'_>> {
    let line = strip_comment(raw).trim_start();
    if line.trim().is_empty() {
        return None;
    }
    let (body, checksum) = match line.rfind('*') {
        Some(star) => (&line[..star], Some(strtol(&line[star + 1..]))),
        None => (line, None),
    };
    let bytes = body.as_bytes();
    let (number, command) = if matches!(bytes.first(), Some(b'N' | b'n')) {
        let rest = &body[1..];
        let digits_end = rest
            .char_indices()
            .find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
            .map_or(rest.len(), |(i, _)| i);
        (Some(strtol(&rest[..digits_end])), rest[digits_end..].trim())
    } else {
        (None, body.trim())
    };
    Some(Framed {
        number,
        command,
        checksum,
    })
}

/// The checksum Marlin computes for a raw received line: the XOR of the
/// bytes before the last `*` of the line with its `;` comment removed and
/// leading whitespace skipped. Works on the raw bytes, so it matches the
/// host's checksum even where the line is not valid UTF-8.
pub(crate) fn raw_checksum(raw: &[u8]) -> u8 {
    let line = raw
        .iter()
        .position(|&b| b == b';')
        .map_or(raw, |i| &raw[..i]);
    let start = line
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(line.len());
    let line = &line[start..];
    line.iter()
        .rposition(|&b| b == b'*')
        .map_or(0, |star| line[..star].iter().fold(0u8, |acc, b| acc ^ b))
}

/// Emergency commands recognised on receipt (Marlin EMERGENCY_PARSER).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Emergency {
    /// M108: break out of heating / user waits.
    M108,
    /// M112: kill.
    M112,
    /// M410: quickstop.
    M410,
    /// M876 S<digit>: host prompt response.
    M876(i64),
}

/// Check a raw received line for an emergency command. Like Marlin's
/// emergency parser this accepts an optional `N<n>` prefix and ignores the
/// checksum.
pub(crate) fn emergency(raw: &str) -> Option<Emergency> {
    let framed = frame(raw)?;
    let cmd = framed.command;
    let bytes = cmd.as_bytes();
    if bytes.len() < 4 || !(bytes[0] == b'M' || bytes[0] == b'm') {
        return None;
    }
    let digits_end = bytes[1..]
        .iter()
        .position(|b| !b.is_ascii_digit())
        .map_or(bytes.len(), |p| p + 1);
    let code = &cmd[1..digits_end];
    let rest = &cmd[digits_end..];
    // The command word must end here (no "M1120", no "M112.5").
    if rest.starts_with(|c: char| !c.is_whitespace()) && !rest.is_empty() {
        // Allow parameters glued to M876 ("M876S1").
        if !(code == "876" && rest.starts_with(['S', 's'])) {
            return None;
        }
    }
    match code {
        "108" => Some(Emergency::M108),
        "112" => Some(Emergency::M112),
        "410" => Some(Emergency::M410),
        "876" => {
            // Marlin's state machine: spaces, 'S', spaces, then one digit
            // is the response; anything else (e.g. OctoPrint's "M876 P1")
            // is not a prompt response.
            let r = rest.trim_start();
            let after_s = r.strip_prefix(['S', 's'])?.trim_start();
            let digit = after_s.bytes().next().filter(u8::is_ascii_digit)?;
            Some(Emergency::M876(i64::from(digit - b'0')))
        }
        _ => None,
    }
}

/// XOR checksum of a payload (for tests).
#[cfg(test)]
pub(crate) fn checksum(payload: &str) -> u8 {
    payload.bytes().fold(0u8, |acc, b| acc ^ b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_numbered_line() {
        let payload = "N12 G1 X10";
        let raw = format!("{payload}*{}", checksum(payload));
        let f = frame(&raw).unwrap();
        assert_eq!(f.number, Some(12));
        assert_eq!(f.command, "G1 X10");
        assert_eq!(f.checksum, Some(i64::from(checksum(payload))));
        assert_eq!(raw_checksum(raw.as_bytes()), checksum(payload));
    }

    #[test]
    fn frames_bare_and_comment_lines() {
        assert_eq!(frame(""), None);
        assert_eq!(frame("   ; only a comment"), None);
        let f = frame("  G28   ; home ").unwrap();
        assert_eq!(f.number, None);
        assert_eq!(f.command, "G28");
        assert_eq!(f.checksum, None);
        let f = frame("M117 Hello  World").unwrap();
        assert_eq!(f.command, "M117 Hello  World");
    }

    #[test]
    fn raw_checksum_matches_frame_and_covers_invalid_utf8() {
        for (raw, payload) in [
            ("N12 G1 X10*7", "N12 G1 X10"),
            ("  N3 M105 *1 ; c*9", "N3 M105 "),
            ("M117 *a*b", "M117 *a"),
            ("G28", ""),
        ] {
            assert_eq!(raw_checksum(raw.as_bytes()), checksum(payload), "{raw}");
        }
        let latin1: &[u8] = b"N1 M117 Caf\xe9*0";
        let expected = b"N1 M117 Caf\xe9".iter().fold(0u8, |a, b| a ^ b);
        assert_eq!(raw_checksum(latin1), expected);
        assert_eq!(raw_checksum(b""), 0);
        assert_eq!(raw_checksum(b"   ;*5"), 0);
    }

    #[test]
    fn checksum_after_comment_is_lost() {
        let f = frame("N3 G1 X1 ;c*99").unwrap();
        assert_eq!(f.checksum, None);
        assert_eq!(f.number, Some(3));
    }

    #[test]
    fn garbage_numbers_follow_strtol() {
        let f = frame("Nx G1*5").unwrap();
        assert_eq!(f.number, Some(0));
        assert_eq!(f.command, "x G1");
        let f = frame("N7 M105*abc").unwrap();
        assert_eq!(f.checksum, Some(0));
        assert_eq!(strtol("  -12abc"), -12);
        assert_eq!(strtol("99999999999999999999999"), i64::MAX);
        assert_eq!(strtol(""), 0);
    }

    #[test]
    fn multibyte_text_does_not_panic() {
        let f = frame("N1 M117 h\u{e9}llo \u{1F600}*12").unwrap();
        assert_eq!(f.command, "M117 h\u{e9}llo \u{1F600}");
        assert!(frame("\u{1F600}").is_some());
        assert_eq!(emergency("\u{1F600}\u{1F600}"), None);
    }

    #[test]
    fn emergency_detection() {
        assert_eq!(emergency("M112"), Some(Emergency::M112));
        assert_eq!(emergency("N5 M112*33"), Some(Emergency::M112));
        assert_eq!(emergency("m108"), Some(Emergency::M108));
        assert_eq!(emergency("M410 ; stop"), Some(Emergency::M410));
        assert_eq!(emergency("M876 S1"), Some(Emergency::M876(1)));
        assert_eq!(emergency("M876S0"), Some(Emergency::M876(0)));
        // Marlin's parser needs S and a digit; the reason is that digit.
        assert_eq!(emergency("M876"), None);
        assert_eq!(emergency("M876 P1"), None);
        assert_eq!(emergency("M876 S"), None);
        assert_eq!(emergency("M876 P1 S0"), None);
        assert_eq!(emergency("M876 S 1"), Some(Emergency::M876(1)));
        assert_eq!(emergency("M876 S12"), Some(Emergency::M876(1)));
        assert_eq!(emergency("M1120"), None);
        assert_eq!(emergency("M11"), None);
        assert_eq!(emergency("G112"), None);
        assert_eq!(emergency("M104 S200"), None);
    }
}
