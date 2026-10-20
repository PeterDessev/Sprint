//! One configuration command as reported by M503 or edited by the user.

use serde::{Deserialize, Serialize};

use super::catalog::setting_def;
use crate::nan::same_f64;

/// One configuration command as reported by M503 (or edited by the user),
/// e.g. `M92 X80 Y80 Z400 E93`.
///
/// Parameters without a value (`M569 S1 X Y Z`, `M593 X F40`, `M149 C`) are
/// flags and stored as `f64::NAN`; [`to_gcode`](Self::to_gcode) prints them
/// as the bare letter. Equality treats two flags as equal (unlike IEEE NaN).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingCommand {
    /// Normalised code: uppercase, no leading zeros (`"M92"`, `"G21"`).
    pub code: String,
    /// Parameters in their original order. Letters are uppercase.
    #[serde(deserialize_with = "crate::nan::de_params")]
    pub params: Vec<(char, f64)>,
    /// The text this command was parsed from (trimmed, `echo:` prefix
    /// removed, comment kept). Not updated by [`set`](Self::set).
    pub raw: String,
}

/// The comment (and key suffix) that marks a stealth-mode setting.
const STEALTH: &str = "stealth";

/// Format a parameter value with at most six decimals and no trailing
/// zeros: `80.0` -> "80", `0.150` -> "0.15", `-0.0` -> "0".
pub(crate) fn format_value(v: f64) -> String {
    let mut s = format!("{v:.6}");
    if s.contains('.') {
        let trimmed = s.trim_end_matches('0').trim_end_matches('.').len();
        s.truncate(trimmed);
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// Parse one parameter token: a single letter (flag), or one or more
/// letter+number pairs (`X80`, `X80Y80`). Returns false when the token is
/// not of that form (e.g. a word like `Hello` or a quoted string).
/// The G29 forms M503 prints for a leveling mesh, with nothing else on the
/// line: `S3 I<i> J<j> Z<z>` (Marlin 1.1: `S3 X<i> Y<j> Z<z>`), `S4 Z<z>`
/// and bilinear `W I<i> J<j> Z<z>`. Every other G29 runs a probe or a
/// leveling routine.
fn is_mesh_point(params: &[(char, f64)]) -> bool {
    let value = |letter: char| params.iter().find(|p| p.0 == letter).map(|p| p.1);
    let number = |letter: char| value(letter).is_some_and(f64::is_finite);
    let only = |allowed: &[char]| params.iter().all(|p| allowed.contains(&p.0));
    if !number('Z') {
        return false;
    }
    let mode = value('S');
    if mode == Some(3.0) {
        (only(&['S', 'I', 'J', 'Z']) && number('I') && number('J'))
            || (only(&['S', 'X', 'Y', 'Z']) && number('X') && number('Y'))
    } else if mode == Some(4.0) {
        only(&['S', 'Z'])
    } else {
        mode.is_none()
            && value('W').is_some_and(f64::is_nan)
            && only(&['W', 'I', 'J', 'Z'])
            && number('I')
            && number('J')
    }
}

fn parse_param_token(tok: &str, out: &mut Vec<(char, f64)>) -> bool {
    let bytes = tok.as_bytes();
    if bytes.len() == 1 {
        return match bytes[0] {
            b if b.is_ascii_alphabetic() => {
                out.push((char::from(b.to_ascii_uppercase()), f64::NAN));
                true
            }
            _ => false,
        };
    }
    let mut i = 0;
    while i < bytes.len() {
        let letter = bytes[i];
        if !letter.is_ascii_alphabetic() {
            return false;
        }
        i += 1;
        let start = i;
        if matches!(bytes.get(i), Some(b'-' | b'+')) {
            i += 1;
        }
        while matches!(bytes.get(i), Some(b) if b.is_ascii_digit() || *b == b'.') {
            i += 1;
        }
        // All bytes consumed so far are ASCII, so this slice is valid UTF-8.
        let Some(number) = tok.get(start..i) else {
            return false;
        };
        match number.parse::<f64>() {
            Ok(v) if v.is_finite() => out.push((char::from(letter.to_ascii_uppercase()), v)),
            _ => return false,
        }
    }
    true
}

/// Remove `( ... )` comments.
fn strip_paren_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

impl SettingCommand {
    /// Build a command from a code and parameters (`raw` is set to the
    /// resulting G-code).
    pub fn new(code: &str, params: &[(char, f64)]) -> SettingCommand {
        let code = super::catalog::normalize_code(code)
            .unwrap_or_else(|| code.trim().to_ascii_uppercase());
        let params = params
            .iter()
            .map(|&(l, v)| (l.to_ascii_uppercase(), v))
            .collect();
        let mut cmd = SettingCommand {
            code,
            params,
            raw: String::new(),
        };
        cmd.raw = cmd.to_gcode();
        cmd
    }

    /// Parse a setting line such as `"M92 X80.00 Y80.00 Z400.00 E93.00"`,
    /// `"echo:  M301 E1 P22.2 I1.08 D114"`, `"M569 S1 X Y Z"`,
    /// `"M420 S0 Z10.00 ; Leveling OFF"` or `"G21 ; (mm)"`.
    ///
    /// An `echo:` prefix and `;` / `( )` comments are ignored. The code must be
    /// a `G` or `M` number; every parameter must be a letter followed by a
    /// number, or a bare letter (flag). Anything else (quoted strings, words)
    /// yields `None`. `G29` is a setting only in the mesh-point forms M503
    /// prints (`G29 S3 I0 J2 Z0.01`, `G29 S3 X1 Y1 Z0.01`, `G29 S4 Z0`,
    /// `G29 W I1 J0 Z0.1`); any other G29 probes the bed and yields `None`.
    pub fn parse(text: &str) -> Option<SettingCommand> {
        let cmd = SettingCommand::parse_any(text)?;
        // G29 probes the bed; only the forms M503 prints set a value.
        (cmd.code != "G29" || is_mesh_point(&cmd.params)).then_some(cmd)
    }

    /// [`SettingCommand::parse`] without the G29 check: also reads a
    /// settings key spelled as G-code (`"g29 w i1 j0"`).
    pub(crate) fn parse_any(text: &str) -> Option<SettingCommand> {
        let mut t = text.trim();
        if t.len() >= 5 && t.is_char_boundary(5) && t[..5].eq_ignore_ascii_case("echo:") {
            t = t[5..].trim();
        }
        let raw = t.to_string();
        let body = t.split(';').next().unwrap_or("");
        let body = strip_paren_comments(body);
        let body = body.trim();

        // Code: letter + digits (+ .digits), possibly with parameters glued on.
        let bytes = body.as_bytes();
        let letter = bytes.first()?.to_ascii_uppercase();
        if letter != b'G' && letter != b'M' {
            return None;
        }
        let mut end = 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == 1 {
            return None;
        }
        if bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
            end += 1;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
        }
        let code = super::catalog::normalize_code(&body[..end])?;

        let mut params = Vec::new();
        for tok in body[end..].split_whitespace() {
            if !parse_param_token(tok, &mut params) {
                return None;
            }
        }
        Some(SettingCommand { code, params, raw })
    }

    /// Value of a parameter (case-insensitive). Flags return `Some(NaN)`.
    pub fn get(&self, letter: char) -> Option<f64> {
        let letter = letter.to_ascii_uppercase();
        self.params
            .iter()
            .find(|(l, _)| *l == letter)
            .map(|&(_, v)| v)
    }

    /// True when the parameter is present (with or without a value).
    pub fn has(&self, letter: char) -> bool {
        self.get(letter).is_some()
    }

    /// Set a parameter, replacing an existing one in place or appending it.
    /// Use `f64::NAN` to set a flag.
    pub fn set(&mut self, letter: char, value: f64) {
        let letter = letter.to_ascii_uppercase();
        match self.params.iter_mut().find(|(l, _)| *l == letter) {
            Some(slot) => slot.1 = value,
            None => self.params.push((letter, value)),
        }
    }

    /// Remove a parameter, returning its value.
    pub fn remove(&mut self, letter: char) -> Option<f64> {
        let letter = letter.to_ascii_uppercase();
        let pos = self.params.iter().position(|(l, _)| *l == letter)?;
        Some(self.params.remove(pos).1)
    }

    /// Identity within a settings report: code plus the selector params
    /// present (from the catalog: E/T/I/P/S/W, or the axis flag of M593),
    /// e.g. `"M301"`, `"M301 E1"`, `"M906 T0"`, `"M145 S1"`, `"M593 X"`,
    /// `"G29 S3 I0 J2"` (one mesh point).
    ///
    /// Two reports of the same command that mean different things get
    /// different keys, so a [`PrinterSettings`](crate::PrinterSettings)
    /// keeps both:
    /// * Prusa's stealth-mode limits ([`is_stealth`](Self::is_stealth))
    ///   end in `" stealth"` (`"M203 stealth"`), next to the normal-mode
    ///   `"M203"`.
    /// * Marlin 1.1 reports the filament diameter (`M200 D1.75`) and then,
    ///   when volumetric extrusion is off, `M200 D0`, which switches it off
    ///   without changing the diameter. That line's key ends in `" D0"`.
    pub fn key(&self) -> String {
        let mut key = self.code.clone();
        if let Some(def) = setting_def(&self.code) {
            for &sel in def.selectors {
                if let Some(v) = self.get(sel) {
                    key.push(' ');
                    key.push(sel);
                    if v.is_finite() {
                        key.push_str(&format_value(v));
                    }
                }
            }
        }
        if self.is_volumetric_off() {
            key.push_str(" D0");
        }
        if self.is_stealth() {
            key.push(' ');
            key.push_str(STEALTH);
        }
        key
    }

    /// `M200 D0` (optionally with `T`): Marlin's way to switch volumetric
    /// extrusion off, keeping the filament diameter.
    fn is_volumetric_off(&self) -> bool {
        self.code == "M200"
            && self.get('D') == Some(0.0)
            && self.params.iter().all(|&(l, _)| l == 'D' || l == 'T')
    }

    /// True for a stealth-mode (silent) motion limit. Prusa MK3 firmware
    /// reports M201 and M203 twice in M503, under "normal" and "stealth"
    /// headings; the reply parser marks the stealth copy with a `; stealth`
    /// comment in [`raw`](Self::raw) (see [`mark_stealth`](Self::mark_stealth)).
    /// Prusa firmware applies M201/M203 to the mode the printer is in.
    pub fn is_stealth(&self) -> bool {
        self.raw
            .split(';')
            .skip(1)
            .any(|comment| comment.trim().eq_ignore_ascii_case(STEALTH))
    }

    /// Mark this command as a stealth-mode setting (a `; stealth` comment
    /// in [`raw`](Self::raw)); its [`key`](Self::key) then ends in
    /// `" stealth"`.
    pub fn mark_stealth(&mut self) {
        if !self.is_stealth() {
            self.raw.push_str(" ; ");
            self.raw.push_str(STEALTH);
        }
    }

    /// The firmware reports this setting, but [`to_gcode`](Self::to_gcode)
    /// would heat or move the printer (Marlin's Autotemp is reported as
    /// `M104 S<min> B<max> F<factor>`, and `M104 S` sets the hotend
    /// temperature): show it, never send it. See
    /// [`is_report_only`](crate::is_report_only).
    pub fn is_report_only(&self) -> bool {
        super::catalog::is_report_only(&self.code)
    }

    /// G-code that applies this setting: `"M92 X80 Y80 Z400 E93"` (trailing
    /// zeros trimmed, original parameter order kept, flags as bare letters).
    pub fn to_gcode(&self) -> String {
        let mut out = self.code.clone();
        for &(letter, value) in &self.params {
            out.push(' ');
            out.push(letter);
            if value.is_finite() {
                out.push_str(&format_value(value));
            }
        }
        out
    }
}

impl PartialEq for SettingCommand {
    /// Field-wise equality where flags (NaN values) equal each other, so a
    /// command with flags equals its own clone.
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
            && self.raw == other.raw
            && self.params.len() == other.params.len()
            && self
                .params
                .iter()
                .zip(&other.params)
                .all(|(a, b)| a.0 == b.0 && same_f64(a.1, b.1))
    }
}

impl std::fmt::Display for SettingCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_gcode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> SettingCommand {
        SettingCommand::parse(s).unwrap_or_else(|| panic!("failed to parse {s:?}"))
    }

    #[test]
    fn format_values() {
        assert_eq!(format_value(80.0), "80");
        assert_eq!(format_value(0.15), "0.15");
        assert_eq!(format_value(22.20), "22.2");
        assert_eq!(format_value(-0.0), "0");
        assert_eq!(format_value(-1.5), "-1.5");
        assert_eq!(format_value(0.1 + 0.2), "0.3");
        assert_eq!(format_value(0.0000001), "0");
        assert_eq!(format_value(1e12), "1000000000000");
    }

    #[test]
    fn parses_marlin_lines() {
        let c = parse("echo:  M92 X80.00 Y80.00 Z400.00 E93.00");
        assert_eq!(c.code, "M92");
        assert_eq!(
            c.params,
            vec![('X', 80.0), ('Y', 80.0), ('Z', 400.0), ('E', 93.0)]
        );
        assert_eq!(c.raw, "M92 X80.00 Y80.00 Z400.00 E93.00");
        assert_eq!(c.to_gcode(), "M92 X80 Y80 Z400 E93");
        assert_eq!(c.key(), "M92");

        let c = parse("M420 S0 Z10.00 ; Leveling OFF");
        assert_eq!(c.to_gcode(), "M420 S0 Z10");
        assert_eq!(c.raw, "M420 S0 Z10.00 ; Leveling OFF");

        let c = parse("echo:  G21    ; Units in mm (mm)");
        assert_eq!(c.code, "G21");
        assert!(c.params.is_empty());
        assert_eq!(c.to_gcode(), "G21");

        let c = parse("echo:  M149 C ; Units in Celsius");
        assert!(c.get('C').unwrap().is_nan());
        assert_eq!(c.to_gcode(), "M149 C");
    }

    #[test]
    fn selectors_and_flags() {
        let c = parse("M301 E1 P22.20 I1.08 D114.00");
        assert_eq!(c.key(), "M301 E1");
        assert_eq!(c.to_gcode(), "M301 E1 P22.2 I1.08 D114");

        assert_eq!(parse("M145 S0 H185 B45 F255").key(), "M145 S0");
        assert_eq!(parse("M906 T0 E650").key(), "M906 T0");
        assert_eq!(parse("M906 I1 X800").key(), "M906 I1");
        assert_eq!(parse("M906 X580 Y580 Z580").key(), "M906");

        let c = parse("M593 X F40 D0.15");
        assert_eq!(c.key(), "M593 X");
        assert_eq!(c.to_gcode(), "M593 X F40 D0.15");
        assert_eq!(parse("M593 Y F40.00 D0.15").key(), "M593 Y");

        let c = parse("M569 S1 X Y Z");
        assert_eq!(c.key(), "M569");
        assert!(c.has('X') && c.get('Y').unwrap().is_nan());
        assert_eq!(c.to_gcode(), "M569 S1 X Y Z");
        assert_eq!(parse("M569 S1 T0 E").key(), "M569 T0");
        assert_eq!(parse("M569 S1 I1 X Y").key(), "M569 I1");

        // T of M204 is the travel acceleration, not a selector.
        assert_eq!(parse("M204 P500.00 R1000.00 T500.00").key(), "M204");
        // Unknown codes: the code alone.
        assert_eq!(parse("M7777 S1").key(), "M7777");
    }

    #[test]
    fn rejects_non_settings() {
        for s in [
            "",
            "echo:",
            "; Steps per unit:",
            "echo:; Steps per unit:",
            "M117 Hello world",
            "M550 P\"Duet\"",
            "M92 X80 Y",
            "M",
            "X80",
            "T0",
            "M92 X1.2.3",
            "M92 X-",
            "M92 X80 *",
            "Unknown command",
        ] {
            // "M92 X80 Y" is valid (Y flag); everything else must fail.
            if s == "M92 X80 Y" {
                assert!(SettingCommand::parse(s).is_some());
            } else {
                assert!(SettingCommand::parse(s).is_none(), "{s:?}");
            }
        }
    }

    #[test]
    fn g29_is_a_setting_only_as_a_mesh_point() {
        // The catalog knows G29 for the mesh points M503 prints, but G29 is
        // also the command that probes the bed: an imported file or a stray
        // reply line must not turn "G29" into a setting that, applied,
        // starts probing.
        for s in [
            "G29 S3 I0 J2 Z0.0125",
            "echo:  G29 S3 X1 Y1 Z0.2",
            "G29 S4 Z-0.05",
            "G29 W I1 J0 Z0.12",
            "g29 w i1 j0 z0.12",
        ] {
            assert!(SettingCommand::parse(s).is_some(), "{s:?}");
        }
        for s in [
            "G29",
            "G29 P1",
            "G29 S1",
            "G29 S2",
            "G29 S5",
            "G29 T",
            "G29 S3 I0 J2",
            "G29 S3 I0 Z0.1",
            "G29 W Z0.1",
            "G29 S3 I0 J0 Z0.1 P1",
            "G29 S4",
        ] {
            assert!(SettingCommand::parse(s).is_none(), "{s:?}");
        }
    }

    #[test]
    fn compact_and_lowercase() {
        let c = parse("m92x80y80");
        assert_eq!(c.code, "M92");
        assert_eq!(c.params, vec![('X', 80.0), ('Y', 80.0)]);
        let c = parse("M092 x80 y-1.5 z+2 e.5");
        assert_eq!(c.code, "M92");
        assert_eq!(
            c.params,
            vec![('X', 80.0), ('Y', -1.5), ('Z', 2.0), ('E', 0.5)]
        );
        let c = parse("G29.1 S1");
        assert_eq!(c.code, "G29.1");
    }

    #[test]
    fn flags_compare_equal() {
        // Regression: flags are NaN, and derived (IEEE) equality made a
        // command with flags unequal to its own clone.
        let c = parse("M569 S1 X Y Z");
        assert_eq!(c, c.clone());
        assert_eq!(c, parse("M569 S1 X Y Z"));
        assert_ne!(c, parse("M569 S1 X Y"));
        assert_ne!(c, parse("M569 S0 X Y Z"));
        let mut valued = c.clone();
        valued.set('X', 1.0);
        assert_ne!(c, valued);
        let settings: crate::PrinterSettings =
            [c.clone(), parse("M593 X F40")].into_iter().collect();
        assert_eq!(settings, settings.clone());
    }

    #[test]
    fn set_get_remove_round_trip() {
        let mut c = parse("M92 X80 Y80 Z400 E93");
        c.set('e', 95.5);
        c.set('T', 1.0);
        assert_eq!(c.get('E'), Some(95.5));
        assert_eq!(c.to_gcode(), "M92 X80 Y80 Z400 E95.5 T1");
        assert_eq!(c.remove('T'), Some(1.0));
        assert_eq!(c.remove('T'), None);
        let reparsed = parse(&c.to_gcode());
        assert_eq!(reparsed.params, c.params);
        assert_eq!(c.raw, "M92 X80 Y80 Z400 E93");

        let built = SettingCommand::new("m851", &[('x', -40.0), ('Y', -10.0), ('Z', -1.25)]);
        assert_eq!(built.code, "M851");
        assert_eq!(built.raw, "M851 X-40 Y-10 Z-1.25");
        assert_eq!(built.to_string(), "M851 X-40 Y-10 Z-1.25");
    }
}
