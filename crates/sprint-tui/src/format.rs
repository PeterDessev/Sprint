//! Number, length and duration formatting shared by the widgets and the
//! command-line reports.
//!
//! Every function accepts any `f32`/`f64`, including NaN and infinities
//! (shown as `"--"`), so values from files or printers can be passed
//! through unchecked.

use std::time::Duration;

/// Placeholder for values that are missing or not finite.
pub const MISSING: &str = "--";

/// Rounded whole seconds of a non-negative finite value.
fn whole_seconds(secs: f64) -> Option<u64> {
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    // Saturates at u64::MAX for absurd values instead of wrapping.
    Some(secs.round().min(u64::MAX as f64) as u64)
}

/// A human duration: `"2d 3h 04m"`, `"1h 02m 03s"`, `"4m 05s"`, `"12s"`.
/// Negative and non-finite values give [`MISSING`].
pub fn duration(secs: f64) -> String {
    let Some(total) = whole_seconds(secs) else {
        return MISSING.to_string();
    };
    let days = total / 86_400;
    let hours = total % 86_400 / 3600;
    let minutes = total % 3600 / 60;
    let seconds = total % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes:02}m")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// [`duration`] for a [`Duration`].
pub fn duration_of(d: Duration) -> String {
    duration(d.as_secs_f64())
}

/// A compact clock: `"1:02:03"` from an hour on, `"2:03"` below.
/// Negative and non-finite values give [`MISSING`].
pub fn clock(secs: f64) -> String {
    let Some(total) = whole_seconds(secs) else {
        return MISSING.to_string();
    };
    let hours = total / 3600;
    let minutes = total % 3600 / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// [`clock`] for a [`Duration`].
pub fn clock_of(d: Duration) -> String {
    clock(d.as_secs_f64())
}

/// A length given in millimetres: `"12.3 mm"` below a metre, `"4.32 m"` from
/// a metre on.
pub fn length_mm(mm: f64) -> String {
    if !mm.is_finite() {
        return MISSING.to_string();
    }
    if mm.abs() >= 1000.0 {
        format!("{:.2} m", mm / 1000.0)
    } else {
        format!("{mm:.1} mm")
    }
}

/// A fraction in `0..=1` as a percentage with one decimal: `"42.5%"`.
pub fn percent(fraction: f64) -> String {
    if !fraction.is_finite() {
        return MISSING.to_string();
    }
    format!("{:.1}%", fraction * 100.0)
}

/// A temperature in °C with one decimal (`"205.3"`), without the unit.
pub fn temperature(celsius: f32) -> String {
    if !celsius.is_finite() {
        return MISSING.to_string();
    }
    format!("{celsius:.1}")
}

/// A number with up to `decimals` decimals and trailing zeros removed:
/// `trimmed(0.20, 2) == "0.2"`, `trimmed(3.0, 2) == "3"`.
pub fn trimmed(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return MISSING.to_string();
    }
    let text = format!("{value:.decimals$}");
    if !text.contains('.') {
        return text;
    }
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "-0" | "" => "0".to_string(),
        t => t.to_string(),
    }
}

/// A number with exactly `decimals` decimals: `fixed(12.345, 1) == "12.3"`.
pub fn fixed(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return MISSING.to_string();
    }
    let text = format!("{value:.decimals$}");
    // "-0.0" reads as a sign error; drop the sign of a rounded zero.
    match text.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_string(),
        _ => text,
    }
}

/// A count with thousands separators: `"12,345"`.
pub fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A byte count: `"512 B"`, `"12.3 KB"`, `"1.2 MB"`, `"3.4 GB"` (powers of
/// 1024).
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Relative difference of `ours` against `reference` as a signed
/// percentage (`"+3.1%"`, `"-0.4%"`); [`MISSING`] when the reference is zero
/// or a value is not finite.
pub fn relative_difference(ours: f64, reference: f64) -> String {
    if !ours.is_finite() || !reference.is_finite() || reference == 0.0 {
        return MISSING.to_string();
    }
    let diff = (ours - reference) / reference * 100.0;
    format!("{diff:+.1}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(0.0), "0s");
        assert_eq!(duration(12.4), "12s");
        assert_eq!(duration(59.6), "1m 00s");
        assert_eq!(duration(245.0), "4m 05s");
        assert_eq!(duration(3723.0), "1h 02m 03s");
        assert_eq!(duration(2.0 * 86_400.0 + 3.0 * 3600.0 + 240.0), "2d 3h 04m");
        assert_eq!(duration(-1.0), MISSING);
        assert_eq!(duration(f64::NAN), MISSING);
        assert_eq!(duration(f64::INFINITY), MISSING);
        assert!(!duration(1e300).is_empty());
        assert_eq!(duration_of(Duration::from_secs(61)), "1m 01s");
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(83.0), "1:23");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(100.0 * 3600.0), "100:00:00");
        assert_eq!(clock(f64::NAN), MISSING);
        assert_eq!(clock(-5.0), MISSING);
        assert_eq!(clock_of(Duration::from_millis(1500)), "0:02");
    }

    #[test]
    fn lengths_and_percentages() {
        assert_eq!(length_mm(12.34), "12.3 mm");
        assert_eq!(length_mm(4321.0), "4.32 m");
        assert_eq!(length_mm(-2500.0), "-2.50 m");
        assert_eq!(length_mm(f64::NAN), MISSING);
        assert_eq!(percent(0.425), "42.5%");
        assert_eq!(percent(1.0), "100.0%");
        assert_eq!(percent(f64::INFINITY), MISSING);
        assert_eq!(temperature(205.26), "205.3");
        assert_eq!(temperature(f32::NAN), MISSING);
    }

    #[test]
    fn trimmed_numbers() {
        assert_eq!(trimmed(0.20, 2), "0.2");
        assert_eq!(trimmed(3.0, 2), "3");
        assert_eq!(trimmed(1.75, 2), "1.75");
        assert_eq!(trimmed(-0.0001, 2), "0");
        assert_eq!(trimmed(12.0, 0), "12");
        assert_eq!(trimmed(f64::NAN, 2), MISSING);
    }

    #[test]
    fn fixed_numbers_and_counts() {
        assert_eq!(fixed(12.345, 1), "12.3");
        assert_eq!(fixed(2.0, 2), "2.00");
        assert_eq!(fixed(-0.04, 1), "0.0");
        assert_eq!(fixed(-1.26, 1), "-1.3");
        assert_eq!(fixed(f64::NAN, 1), MISSING);
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(
            count(usize::MAX).matches(',').count(),
            (usize::MAX.to_string().len() - 1) / 3
        );
    }

    #[test]
    fn byte_counts() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(bytes(u64::MAX), "16777216.0 TB");
    }

    #[test]
    fn relative_differences() {
        assert_eq!(relative_difference(103.1, 100.0), "+3.1%");
        assert_eq!(relative_difference(99.6, 100.0), "-0.4%");
        assert_eq!(relative_difference(1.0, 0.0), MISSING);
        assert_eq!(relative_difference(f64::NAN, 1.0), MISSING);
    }
}
