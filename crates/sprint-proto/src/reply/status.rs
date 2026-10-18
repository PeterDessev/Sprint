//! Endstop (M119), statistics (M78) and print time (M31) replies.

use super::StatsReport;
use super::text::{parse_duration, parse_u32, strip_prefix_ci};

/// Endstop state word: "open" / "TRIGGERED" (Marlin, Klipper), RRF "not
/// stopped" / "at min stop" / "at max stop" / "stopped".
fn endstop_state(word: &str) -> Option<bool> {
    match word.trim().to_ascii_lowercase().as_str() {
        "open" | "not stopped" | "not triggered" => Some(false),
        "triggered" | "hit" | "at min stop" | "at max stop" | "stopped" => Some(true),
        _ => None,
    }
}

fn valid_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ')
}

/// Parse an endstop report line into `(name, triggered)` pairs:
/// Marlin "x_min: open" / "z_probe: TRIGGERED", Klipper
/// "x:open y:open z:TRIGGERED" and RRF "Endstops - X: not stopped, Y: at
/// min stop, Z probe: not stopped".
pub(crate) fn parse_endstops(line: &str) -> Option<Vec<(String, bool)>> {
    if let Some(rest) = strip_prefix_ci(line, "Endstops - ") {
        let mut out = Vec::new();
        for part in rest.split(',') {
            let (name, state) = part.split_once(':')?;
            if !valid_name(name) {
                return None;
            }
            out.push((name.trim().to_string(), endstop_state(state)?));
        }
        return (!out.is_empty()).then_some(out);
    }
    // Marlin: one endstop per line, "name: state".
    if let Some((name, state)) = line.split_once(": ")
        && valid_name(name)
        && !name.contains(' ')
        && let Some(triggered) = endstop_state(state)
    {
        return Some(vec![(name.trim().to_string(), triggered)]);
    }
    // Klipper: "x:open y:open z:TRIGGERED".
    let mut out = Vec::new();
    for tok in line.split_whitespace() {
        let (name, state) = tok.split_once(':')?;
        if !valid_name(name) {
            return None;
        }
        let triggered = match state.to_ascii_lowercase().as_str() {
            "open" => false,
            "triggered" => true,
            _ => return None,
        };
        out.push((name.to_string(), triggered));
    }
    (!out.is_empty()).then_some(out)
}

/// Filament length "12.34m", "1234mm", "12.3 m" in millimetres.
fn filament_mm(text: &str) -> Option<f64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(text.len());
    let (num, unit) = text.split_at(split);
    let value: f64 = num.parse().ok().filter(|v: &f64| v.is_finite())?;
    let factor = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "m" => 1000.0,
        "mm" => 1.0,
        "cm" => 10.0,
        "km" => 1_000_000.0,
        _ => return None,
    };
    Some(value * factor)
}

/// Remove `( ... )` groups (DEBUG_PRINTCOUNTER prints raw seconds in
/// parentheses).
fn strip_parens(s: &str) -> String {
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

/// Parse one M78 line: "Stats: Prints: 12, Finished: 10, Failed: 2",
/// "Stats: Total time: 1d 2h 3m 4s, Longest job: 5h 6m 7s",
/// "Stats: Filament used: 12.34m".
pub(crate) fn parse_stats(body: &str) -> Option<StatsReport> {
    let rest = strip_prefix_ci(body, "Stats:")?;
    let rest = strip_parens(rest);
    let mut report = StatsReport::default();
    let mut any = false;
    for part in rest.split(',') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let key = key.trim().to_ascii_lowercase();
        match key.as_str() {
            "prints" => report.prints = parse_u32(value),
            "finished" => report.finished = parse_u32(value),
            "failed" => report.failed = parse_u32(value),
            "total time" => report.total_time = parse_duration(value),
            "longest job" => report.longest_job = parse_duration(value),
            "filament used" => report.filament_used_mm = filament_mm(value),
            _ => continue,
        }
        any = true;
    }
    any.then_some(report)
}

/// M31: "Print time: 1h 2m 3s" (Marlin) or "12 min, 5 sec" (Prusa).
pub(crate) fn parse_print_time(body: &str) -> Option<u64> {
    if let Some(rest) = strip_prefix_ci(body, "Print time:") {
        return parse_duration(rest);
    }
    let lower = body.to_ascii_lowercase();
    if lower.contains(" min, ") && lower.ends_with(" sec") {
        return parse_duration(body);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn es(pairs: &[(&str, bool)]) -> Option<Vec<(String, bool)>> {
        Some(pairs.iter().map(|&(n, t)| (n.to_string(), t)).collect())
    }

    #[test]
    fn endstops() {
        assert_eq!(parse_endstops("x_min: open"), es(&[("x_min", false)]));
        assert_eq!(
            parse_endstops("z_probe: TRIGGERED"),
            es(&[("z_probe", true)])
        );
        assert_eq!(parse_endstops("filament: open"), es(&[("filament", false)]));
        assert_eq!(parse_endstops("z2_min: TRIGGERED"), es(&[("z2_min", true)]));
        assert_eq!(
            parse_endstops("x:open y:open z:TRIGGERED"),
            es(&[("x", false), ("y", false), ("z", true)])
        );
        assert_eq!(
            parse_endstops(
                "Endstops - X: not stopped, Y: at min stop, Z: not stopped, Z probe: not stopped"
            ),
            es(&[("X", false), ("Y", true), ("Z", false), ("Z probe", false)])
        );
        for line in [
            "Reporting endstop status",
            "Bed X: 110.00 Y: 110.00 Z: 0.42",
            "x_min: maybe",
            "x:open y:2",
            "",
            "SD card ok",
            "1abc: open",
        ] {
            assert_eq!(parse_endstops(line), None, "{line:?}");
        }
    }

    #[test]
    fn stats_lines() {
        assert_eq!(
            parse_stats("Stats: Prints: 12, Finished: 10, Failed: 2"),
            Some(StatsReport {
                prints: Some(12),
                finished: Some(10),
                failed: Some(2),
                ..Default::default()
            })
        );
        assert_eq!(
            parse_stats("Stats: Total time: 1d 2h 3m 4s (93784), Longest job: 5h 6m 7s"),
            Some(StatsReport {
                total_time: Some(93_784),
                longest_job: Some(5 * 3600 + 6 * 60 + 7),
                ..Default::default()
            })
        );
        let r = parse_stats("Stats: Filament used: 12.34m").unwrap();
        assert!((r.filament_used_mm.unwrap() - 12_340.0).abs() < 1e-6);
        assert_eq!(parse_stats("Stats:"), None);
        assert_eq!(parse_stats("Prints: 1"), None);
        assert_eq!(filament_mm("5 mm"), Some(5.0));
        assert_eq!(filament_mm("abc"), None);
    }

    #[test]
    fn print_time() {
        assert_eq!(parse_print_time("Print time: 1h 2m 3s"), Some(3723));
        assert_eq!(parse_print_time("Print time: 2d 3h 4m 5s"), Some(183_845));
        assert_eq!(parse_print_time("12 min, 5 sec"), Some(725));
        assert_eq!(parse_print_time("Print time: soon"), None);
        assert_eq!(parse_print_time("hello"), None);
    }
}
