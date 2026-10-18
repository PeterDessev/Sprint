//! Line framing: line numbers and checksums as used by RepRap-style firmware
//! (Marlin, Prusa, Klipper, RepRapFirmware, Smoothieware, Repetier).
//!
//! A framed line looks like `N12 G1 X10*98`: the `N` word, a single space,
//! the command, then `*` and the XOR checksum of every byte before the `*`.

/// XOR of all bytes of `payload`.
///
/// For a framed line the payload is `"N<n> <command>"`, i.e. everything that
/// precedes the `*`.
pub fn checksum(payload: &str) -> u8 {
    payload.bytes().fold(0u8, |acc, b| acc ^ b)
}

/// Frame a command with a line number and checksum: `"N{n} {cmd}*{cs}"`,
/// where the checksum covers `"N{n} {cmd}"`.
///
/// Surrounding whitespace (including a trailing `\r`/`\n`) is removed from
/// `command` first so the checksum matches what the firmware receives. The
/// command must not contain `*` itself (firmware treats the first `*` as the
/// start of the checksum); callers send cleaned lines.
pub fn frame_numbered(line_number: u32, command: &str) -> String {
    let command = command.trim();
    let payload = format!("N{line_number} {command}");
    let cs = checksum(&payload);
    format!("{payload}*{cs}")
}

/// Error returned by [`parse_framed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The line does not start with an `N<number>` word.
    #[error("line has no N line number")]
    MissingLineNumber,
    /// The line has no `*<checksum>` suffix.
    #[error("line has no checksum")]
    MissingChecksum,
    /// The checksum field is not a number in 0..=255.
    #[error("checksum field is not a number")]
    InvalidChecksum,
    /// The checksum does not match the payload.
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Checksum computed over the payload.
        expected: u8,
        /// Checksum found on the line.
        actual: u8,
    },
}

/// A framed line split into its parts by [`parse_framed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramedLine<'a> {
    /// The `N` line number.
    pub line_number: u32,
    /// The command between the line number and the `*` (trimmed).
    pub command: &'a str,
    /// The checksum that was transmitted (already verified).
    pub checksum: u8,
}

/// Parse and verify a framed line such as `N12 G1 X10*98` (the inverse of
/// [`frame_numbered`]). Useful for tests and for anything that has to
/// validate host traffic the way firmware does.
///
/// ```
/// use sprint_proto::{FrameError, frame_numbered, parse_framed};
///
/// assert_eq!(frame_numbered(12, "G1 X10"), "N12 G1 X10*98");
/// let line = parse_framed("N12 G1 X10*98").unwrap();
/// assert_eq!((line.line_number, line.command), (12, "G1 X10"));
/// assert!(matches!(
///     parse_framed("N12 G1 X10*85"),
///     Err(FrameError::ChecksumMismatch { expected: 98, actual: 85 })
/// ));
/// ```
pub fn parse_framed(line: &str) -> Result<FramedLine<'_>, FrameError> {
    let line = line.trim();
    let star = line.rfind('*').ok_or(FrameError::MissingChecksum)?;
    let payload = &line[..star];
    let cs_text = line[star + 1..].trim();
    let actual: u8 = cs_text.parse().map_err(|_| FrameError::InvalidChecksum)?;
    let rest = payload
        .strip_prefix('N')
        .or_else(|| payload.strip_prefix('n'))
        .ok_or(FrameError::MissingLineNumber)?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return Err(FrameError::MissingLineNumber);
    }
    let line_number: u32 = rest[..digits]
        .parse()
        .map_err(|_| FrameError::MissingLineNumber)?;
    let expected = checksum(payload);
    if expected != actual {
        return Err(FrameError::ChecksumMismatch { expected, actual });
    }
    Ok(FramedLine {
        line_number,
        command: rest[digits..].trim(),
        checksum: actual,
    })
}

/// Emergency commands that firmware handles immediately (Marlin's emergency
/// parser) and that the host must send outside the queue and flow control.
const EMERGENCY_CODES: [u32; 4] = [112, 108, 410, 876];

/// True for commands that must bypass the send queue and flow control:
/// `M112` (emergency stop), `M108` (break out of a heating/wait loop),
/// `M410` (quick stop) and `M876` (host prompt response).
///
/// Case-insensitive, with or without parameters (`m876 S1`, `M876S1`), and
/// with spaces after the letter (`M 112`, which Marlin accepts too). A
/// leading `N` word and a trailing `*checksum` are ignored.
pub fn is_emergency_command(command: &str) -> bool {
    let mut s = command.trim_start();
    // Skip an optional line-number word.
    if let Some(rest) = s.strip_prefix(['N', 'n']) {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            s = rest[digits..].trim_start();
        }
    }
    let Some(rest) = s.strip_prefix(['M', 'm']) else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 6 {
        return false;
    }
    // A sub-code (M112.1) is a different command.
    if rest[digits..].starts_with('.') {
        return false;
    }
    match rest[..digits].parse::<u32>() {
        Ok(code) => EMERGENCY_CODES.contains(&code),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_of_known_lines() {
        // Values cross-checked against Printrun/OctoPrint output.
        assert_eq!(checksum(""), 0);
        assert_eq!(checksum("N0 M110 N0"), 125);
        assert_eq!(checksum("N1 G28"), 18);
        assert_eq!(checksum("N3 T0"), 57);
        assert_eq!(checksum("N4 G92 E0"), 67);
    }

    #[test]
    fn frame_matches_marlin_format() {
        assert_eq!(frame_numbered(0, "M110 N0"), "N0 M110 N0*125");
        assert_eq!(frame_numbered(1, "G28"), "N1 G28*18");
        assert_eq!(frame_numbered(3, "T0"), "N3 T0*57");
        assert_eq!(frame_numbered(4, " G92 E0\r\n"), "N4 G92 E0*67");
    }

    #[test]
    fn frame_round_trips_through_parse() {
        for (n, cmd) in [
            (0u32, "M105"),
            (12, "G1 X10 Y20 E0.5"),
            (u32::MAX, "M117 Hi"),
        ] {
            let framed = frame_numbered(n, cmd);
            let parsed = parse_framed(&framed).expect("valid frame");
            assert_eq!(parsed.line_number, n);
            assert_eq!(parsed.command, cmd);
            assert_eq!(parsed.checksum, checksum(&format!("N{n} {cmd}")));
        }
    }

    #[test]
    fn parse_framed_rejects_bad_lines() {
        assert_eq!(parse_framed("G28"), Err(FrameError::MissingChecksum));
        assert_eq!(parse_framed("G28*18"), Err(FrameError::MissingLineNumber));
        assert_eq!(parse_framed("N G28*18"), Err(FrameError::MissingLineNumber));
        assert_eq!(parse_framed("N1 G28*xx"), Err(FrameError::InvalidChecksum));
        assert_eq!(parse_framed("N1 G28*999"), Err(FrameError::InvalidChecksum));
        assert_eq!(
            parse_framed("N1 G28*19"),
            Err(FrameError::ChecksumMismatch {
                expected: 18,
                actual: 19
            })
        );
    }

    #[test]
    fn emergency_commands() {
        for cmd in [
            "M112",
            "m112",
            " M112 ",
            "M108",
            "M410",
            "M876",
            "M876 S1",
            "m876s0",
            "M112 ; stop",
            "N5 M112*1",
            "M0112",
            // Marlin skips spaces between the letter and the number.
            "M 112",
            "m  410",
            "N7 M 108",
        ] {
            assert!(is_emergency_command(cmd), "{cmd:?} should be emergency");
        }
        for cmd in [
            "",
            "M",
            "M11",
            "M1120",
            "M1080",
            "M41",
            "G112",
            "M112.1",
            "M105",
            "T0",
            "M8760",
            "SET_FAN_SPEED",
            "M9999999999999",
        ] {
            assert!(
                !is_emergency_command(cmd),
                "{cmd:?} should not be emergency"
            );
        }
    }
}
