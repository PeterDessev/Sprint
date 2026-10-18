//! Position reports (M114 / M154 auto-report).

use super::PositionReport;

/// A whitespace token, split into `KEY:value` when it has a colon.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok<'a> {
    Pair(&'a str, Option<&'a str>),
    Word(&'a str),
}

/// Tokenise `X:0.00 Y: 1.00 Count 0 0 0`, joining a value separated from its
/// key by a space ("X: 0.00") unless the next token is itself a pair.
fn tokens(line: &str) -> Vec<Tok<'_>> {
    let raw: Vec<&str> = line.split_whitespace().collect();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let tok = raw[i];
        i += 1;
        match tok.split_once(':') {
            Some((key, "")) => {
                let value = match raw.get(i) {
                    Some(next) if !next.contains(':') && next.parse::<f64>().is_ok() => {
                        i += 1;
                        Some(*next)
                    }
                    _ => None,
                };
                out.push(Tok::Pair(key, value));
            }
            Some((key, value)) => out.push(Tok::Pair(key, Some(value))),
            None => out.push(Tok::Word(tok)),
        }
    }
    out
}

fn number(s: Option<&str>) -> Option<f32> {
    s?.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Parse a position report:
///
/// * Marlin `X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0`
/// * Prusa `X:0.00 Y:0.00 Z:0.15 E:0.00 Count X: 0.00 Y:-3.00 Z:0.15 E:0.00`
/// * Klipper `X:0.000 Y:0.000 Z:0.000 E:0.000`
/// * RRF `X:0.000 Y:0.000 Z:0.000 E:0.000 E0:-0.0 Count 0 0 0 Machine 0.000 0.000 0.000 Bed comp 0.000`
/// * Smoothieware / Repetier `C: X:0.0000 Y:0.0000 Z:0.0000 E:0.0000`
///
/// The line must start with `X:` (after an optional empty `C:`) and report
/// numeric X, Y and Z, so probe results such as `Bed X: 110.00 Y: 110.00
/// Z: 0.42` and other lines containing `X:` are not matched. `E` defaults to
/// 0 when missing. `stepper_counts` is set only when every value after
/// `Count` is an integer (Prusa reports millimetres there).
pub fn parse_position(line: &str) -> Option<PositionReport> {
    let toks = tokens(line);
    let mut iter = toks.iter().peekable();
    if let Some(Tok::Pair("C", None)) = iter.peek() {
        iter.next();
    }
    match iter.peek() {
        Some(Tok::Pair("X", Some(_))) => {}
        _ => return None,
    }

    let (mut x, mut y, mut z, mut e) = (None, None, None, None);
    let mut saw_count = false;
    for tok in iter.by_ref() {
        match *tok {
            Tok::Pair(key, value) => {
                let slot = match key {
                    "X" => &mut x,
                    "Y" => &mut y,
                    "Z" => &mut z,
                    "E" => &mut e,
                    // Extra axes (A, B, U...) and per-extruder E0:/E1:.
                    _ => continue,
                };
                if slot.is_none() {
                    *slot = Some(number(value)?);
                }
            }
            Tok::Word(w) if w.eq_ignore_ascii_case("Count") => {
                saw_count = true;
                break;
            }
            Tok::Word(_) => return None,
        }
    }

    let mut stepper_counts = None;
    if saw_count {
        let mut counts = Vec::new();
        let mut all_integers = true;
        for tok in iter {
            let text = match *tok {
                Tok::Pair(_, Some(v)) => v,
                Tok::Pair(_, None) => continue,
                Tok::Word(w) if w.parse::<f64>().is_ok() => w,
                Tok::Word(_) => break, // "Machine", "Bed comp", ...
            };
            match text.parse::<i64>() {
                Ok(n) => counts.push(n),
                Err(_) => all_integers = false,
            }
        }
        if all_integers && !counts.is_empty() {
            stepper_counts = Some(counts);
        }
    }

    Some(PositionReport {
        x: x?,
        y: y?,
        z: z?,
        e: e.unwrap_or(0.0),
        stepper_counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: f32, y: f32, z: f32, e: f32, counts: Option<Vec<i64>>) -> PositionReport {
        PositionReport {
            x,
            y,
            z,
            e,
            stepper_counts: counts,
        }
    }

    #[test]
    fn firmware_formats() {
        assert_eq!(
            parse_position("X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0"),
            Some(pos(0.0, 0.0, 0.0, 0.0, Some(vec![0, 0, 0])))
        );
        assert_eq!(
            parse_position("X:10.00 Y:20.50 Z:0.30 E:1.25 Count X:800 Y:1640 Z:120"),
            Some(pos(10.0, 20.5, 0.3, 1.25, Some(vec![800, 1640, 120])))
        );
        assert_eq!(
            parse_position("X:0.00 Y:0.00 Z:0.15 E:0.00 Count X: 0.00 Y:-3.00 Z:0.15 E:0.00"),
            Some(pos(0.0, 0.0, 0.15, 0.0, None))
        );
        assert_eq!(
            parse_position("X:0.000 Y:0.000 Z:0.000 E:0.000"),
            Some(pos(0.0, 0.0, 0.0, 0.0, None))
        );
        assert_eq!(
            parse_position(
                "X:1.000 Y:2.000 Z:3.000 E:0.000 E0:-0.0 Count 80 160 1200 Machine 1.000 2.000 3.000 Bed comp 0.000"
            ),
            Some(pos(1.0, 2.0, 3.0, 0.0, Some(vec![80, 160, 1200])))
        );
        assert_eq!(
            parse_position("C: X:5.0000 Y:6.0000 Z:7.0000 E:8.0000"),
            Some(pos(5.0, 6.0, 7.0, 8.0, None))
        );
        // CoreXY counts and extra axes.
        assert_eq!(
            parse_position("X:1.00 Y:2.00 Z:3.00 A:0.00 E:4.00 Count A:800 B:1600 Z:120"),
            Some(pos(1.0, 2.0, 3.0, 4.0, Some(vec![800, 1600, 120])))
        );
        // No E reported.
        assert_eq!(
            parse_position("X:1 Y:2 Z:3"),
            Some(pos(1.0, 2.0, 3.0, 0.0, None))
        );
    }

    #[test]
    fn does_not_misfire() {
        for line in [
            "",
            "Bed X: 110.00 Y: 110.00 Z: 0.42",
            "echo:Home offset X:0.00",
            "Stepper: X:800 Y:800 Z:120 E:0",
            "Logical: X:0.00 Y:0.00 Z:0.00",
            "X:0.00 Y:0.00",
            "X:abc Y:0 Z:0",
            "X:0 Y:0 Z:nope",
            "X:1 Y:2 junk Z:3",
            "T:210.0 /210.0",
            "x_min: open",
            "X:",
        ] {
            assert_eq!(parse_position(line), None, "{line:?}");
        }
    }
}
