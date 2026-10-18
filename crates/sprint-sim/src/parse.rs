//! Minimal G-code command parser (independent of `sprint-gcode` on purpose).

/// Commands whose remainder is a free-text argument rather than parameters.
const STRING_ARG_M: &[u16] = &[23, 28, 30, 32, 33, 117, 118, 928];

/// One parsed command.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Command<'a> {
    /// `b'G'`, `b'M'`, `b'T'`, or 0 when the text has no command word.
    pub letter: u8,
    /// Command number (`1` for G1, tool index for T).
    pub code: u16,
    /// Sub-code after a dot (G29.1 -> Some(1)).
    pub sub: Option<u8>,
    /// The whole command text, as received (cleaned).
    pub text: &'a str,
    /// Remainder after the command word (trimmed). The free-text argument
    /// for M23/M28/M30/M32/M33/M117/M118.
    pub args: &'a str,
    params: [Param; 26],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Param {
    seen: bool,
    value: Option<f64>,
}

/// Scan a number (`-12.5`, `.5`, `+3`) at the start of `s`; returns the
/// value and the number of bytes consumed.
fn scan_number(s: &str) -> Option<(f64, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'-' | b'+')) {
        i += 1;
    }
    let digits_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - digits_start;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        digits += i - frac_start;
    }
    if digits == 0 {
        return None;
    }
    let v: f64 = s[..i].parse().ok()?;
    v.is_finite().then_some((v, i))
}

impl<'a> Command<'a> {
    /// Parse a cleaned command (no line number, checksum or `;` comment).
    pub(crate) fn parse(text: &'a str) -> Command<'a> {
        let t = text.trim();
        let mut cmd = Command {
            letter: 0,
            code: 0,
            sub: None,
            text: t,
            args: "",
            params: [Param::default(); 26],
        };
        let b = t.as_bytes();
        let letter = b.first().map(u8::to_ascii_uppercase);
        let rest = match letter {
            Some(l @ (b'G' | b'M' | b'T')) if b.get(1).is_some_and(u8::is_ascii_digit) => {
                let mut i = 1;
                let mut code: u32 = 0;
                while i < b.len() && b[i].is_ascii_digit() {
                    code = (code * 10 + u32::from(b[i] - b'0')).min(65_535);
                    i += 1;
                }
                cmd.letter = l;
                cmd.code = code as u16;
                if l != b'T' && i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                    let mut sub: u32 = 0;
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        sub = (sub * 10 + u32::from(b[i] - b'0')).min(255);
                        i += 1;
                    }
                    cmd.sub = Some(sub as u8);
                }
                &t[i..]
            }
            _ => t,
        };
        cmd.args = rest.trim();
        if cmd.letter == b'M' && STRING_ARG_M.contains(&cmd.code) {
            // M118 still takes leading A1/E1/Pn flags; handled by the command.
            return cmd;
        }
        if cmd.letter != 0 {
            cmd.parse_params(rest);
        }
        cmd
    }

    fn parse_params(&mut self, s: &str) {
        let mut i = 0;
        let b = s.as_bytes();
        while i < b.len() {
            let c = b[i];
            if c == b'(' {
                // Parenthesised comment: skip to the closing parenthesis.
                while i < b.len() && b[i] != b')' {
                    i += 1;
                }
                i += 1;
                continue;
            }
            if c.is_ascii_alphabetic() {
                let idx = usize::from(c.to_ascii_uppercase() - b'A');
                i += 1;
                let value = s.get(i..).and_then(scan_number).map(|(v, used)| {
                    i += used;
                    v
                });
                self.params[idx] = Param { seen: true, value };
                continue;
            }
            i += 1;
        }
    }

    /// True for the given letter and code without a sub-code.
    pub(crate) fn is(&self, letter: u8, code: u16) -> bool {
        self.letter == letter && self.code == code
    }

    fn param(&self, letter: char) -> Param {
        let up = letter.to_ascii_uppercase();
        if up.is_ascii_uppercase() {
            self.params[(up as u8 - b'A') as usize]
        } else {
            Param::default()
        }
    }

    /// Whether the parameter letter appears (with or without a value).
    pub(crate) fn seen(&self, letter: char) -> bool {
        self.param(letter).seen
    }

    /// The parameter's numeric value, if present.
    pub(crate) fn value(&self, letter: char) -> Option<f64> {
        self.param(letter).value
    }

    /// The parameter's value truncated to an integer, if present.
    pub(crate) fn int(&self, letter: char) -> Option<i64> {
        self.value(letter).map(|v| v.clamp(-9.0e15, 9.0e15) as i64)
    }

    /// Marlin `boolval`: a bare letter or a non-zero value is true.
    pub(crate) fn flag(&self, letter: char) -> bool {
        let p = self.param(letter);
        p.seen && p.value.is_none_or(|v| v != 0.0)
    }

    /// Whether any parameter at all was given.
    pub(crate) fn has_params(&self) -> bool {
        self.params.iter().any(|p| p.seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_move() {
        let c = Command::parse("G1 X10 Y-2.5 E.5 F3000");
        assert!(c.is(b'G', 1));
        assert_eq!(c.value('X'), Some(10.0));
        assert_eq!(c.value('Y'), Some(-2.5));
        assert_eq!(c.value('E'), Some(0.5));
        assert_eq!(c.value('F'), Some(3000.0));
        assert!(!c.seen('Z'));
    }

    #[test]
    fn parses_compact_and_lowercase() {
        let c = Command::parse("g1x10y20");
        assert!(c.is(b'G', 1));
        assert_eq!(c.value('X'), Some(10.0));
        assert_eq!(c.value('y'), Some(20.0));
        let c = Command::parse("G28 X Y");
        assert!(c.seen('X') && c.seen('Y') && !c.seen('Z'));
        assert_eq!(c.value('X'), None);
        assert!(c.flag('X'));
    }

    #[test]
    fn sub_codes_and_tools() {
        let c = Command::parse("G29.1 P2");
        assert_eq!((c.letter, c.code, c.sub), (b'G', 29, Some(1)));
        let c = Command::parse("T1");
        assert!(c.is(b'T', 1));
        let c = Command::parse("M999999999");
        assert_eq!(c.code, 65_535);
    }

    #[test]
    fn string_args_are_kept() {
        let c = Command::parse("M117 Hello  X10 World");
        assert!(c.is(b'M', 117));
        assert_eq!(c.args, "Hello  X10 World");
        assert!(!c.seen('X'));
        let c = Command::parse("M23 cube~1.gco");
        assert_eq!(c.args, "cube~1.gco");
        let c = Command::parse("M28cube.gco");
        assert_eq!(c.args, "cube.gco");
    }

    #[test]
    fn non_commands() {
        let c = Command::parse("X10 Y20");
        assert_eq!(c.letter, 0);
        let c = Command::parse("SET_FAN_SPEED SPEED=1");
        assert_eq!(c.letter, 0);
        let c = Command::parse("G");
        assert_eq!(c.letter, 0);
        let c = Command::parse("");
        assert_eq!(c.letter, 0);
    }

    #[test]
    fn paren_comments_and_garbage() {
        let c = Command::parse("G1 X5 (move to Y9) Z1");
        assert_eq!(c.value('X'), Some(5.0));
        assert!(!c.seen('Y'));
        assert_eq!(c.value('Z'), Some(1.0));
        let c = Command::parse("G1 X- Y. Z1e5 \u{e9}\u{1F600}");
        assert!(c.seen('X') && c.value('X').is_none());
        assert!(c.value('Y').is_none());
        assert_eq!(c.value('Z'), Some(1.0));
        assert!(c.seen('E'));
    }

    #[test]
    fn int_and_flag() {
        let c = Command::parse("M106 P1 S127.9 A0");
        assert_eq!(c.int('P'), Some(1));
        assert_eq!(c.int('S'), Some(127));
        assert!(!c.flag('A'));
        assert!(c.has_params());
        assert!(!Command::parse("M105").has_params());
    }
}
