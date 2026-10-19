//! Human-readable numbers for the insight tabs.
//!
//! Every function accepts any input, including NaN, infinities and negative
//! values, and returns "—" for values that cannot be shown.

/// What non-finite (or otherwise unusable) values are shown as.
pub const MISSING: &str = "—";

/// A duration in the compact form used across Sprint: `"1d 2h 3m"`,
/// `"2h 5m"`, `"4m 12s"`, `"38s"`.
///
/// The two or three most significant units are shown (seconds only below
/// one hour, minutes as the smallest unit above it); zero components are
/// left out (`"2h"`, not `"2h 0m"`). Negative and non-finite values give
/// "—".
pub fn duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return MISSING.to_owned();
    }
    // Round to whole seconds first so 59.6 s shows as "1m", not "60s".
    let total = secs.round().min(u64::MAX as f64 / 2.0) as u64;
    let mut parts: Vec<String> = Vec::with_capacity(3);
    let mut push = |value: u64, unit: &str| {
        if value > 0 {
            parts.push(format!("{value}{unit}"));
        }
    };
    if total >= 3600 {
        // From an hour on, minutes are the smallest unit: round to them
        // rather than dropping the seconds.
        let rounded = (secs / 60.0).round().min(u64::MAX as f64 / 120.0) as u64 * 60;
        push(rounded / 86_400, "d");
        push((rounded % 86_400) / 3600, "h");
        push((rounded % 3600) / 60, "m");
    } else {
        push(total / 60, "m");
        push(total % 60, "s");
    }
    if parts.is_empty() {
        "0s".to_owned()
    } else {
        parts.join(" ")
    }
}

/// A duration as a clock: `"0:42"`, `"12:05"`, `"1:02:03"`. Negative and
/// non-finite values give "—".
pub fn clock(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return MISSING.to_owned();
    }
    let total = secs.floor().min(u64::MAX as f64) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A short duration with one decimal below a minute (`"8.4 s"`), else
/// [`duration`]. For heat-up and settling times.
pub fn seconds(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return MISSING.to_owned();
    }
    if secs < 59.95 {
        format!("{secs:.1} s")
    } else {
        duration(secs)
    }
}

/// A filament length: `"523 mm"`, `"12.35 m"`, `"1.24 km"`. Non-finite
/// values give "—"; negative values keep their sign.
pub fn length_mm(mm: f64) -> String {
    if !mm.is_finite() {
        return MISSING.to_owned();
    }
    let abs = mm.abs();
    if abs < 1000.0 {
        format!("{mm:.0} mm")
    } else if abs < 1_000_000.0 {
        format!("{:.2} m", mm / 1000.0)
    } else {
        format!("{:.2} km", mm / 1_000_000.0)
    }
}

/// A byte count with binary prefixes: `"512 B"`, `"47.1 KB"`, `"1.76 MB"`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let decimals = if value < 10.0 {
        2
    } else if value < 100.0 {
        1
    } else {
        0
    };
    format!("{value:.decimals$} {}", UNITS[unit])
}

/// An integer with thousands separators: `"12,345"`.
pub fn count(n: u64) -> String {
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

/// A rate with a unit, scaled to stay readable: `"1,234 lines/s"`,
/// `"12.5 lines/s"`, `"0.42 lines/s"`.
pub fn rate(per_sec: f64, unit: &str) -> String {
    if !per_sec.is_finite() || per_sec < 0.0 {
        return MISSING.to_owned();
    }
    let value = if per_sec >= 1000.0 {
        count(per_sec.round() as u64)
    } else if per_sec >= 100.0 {
        format!("{per_sec:.0}")
    } else if per_sec >= 10.0 {
        format!("{per_sec:.1}")
    } else {
        format!("{per_sec:.2}")
    };
    if unit.is_empty() {
        value
    } else {
        format!("{value} {unit}")
    }
}

/// `fraction` (0..=1) as a percentage with `decimals` decimals: `"98.5%"`.
pub fn percent(fraction: f64, decimals: usize) -> String {
    if !fraction.is_finite() {
        return MISSING.to_owned();
    }
    let decimals = decimals.min(6);
    let text = format!("{:.*}", decimals, fraction * 100.0);
    format!("{}%", strip_negative_zero(&text))
}

/// A number with `decimals` decimals and an explicit sign: `"+0.12"`,
/// `"−0.05"` (a real minus sign), `"0.00"`.
pub fn signed(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return MISSING.to_owned();
    }
    let decimals = decimals.min(9);
    let text = format!("{:.*}", decimals, value.abs());
    if text.chars().all(|c| c == '0' || c == '.') {
        text
    } else if value < 0.0 {
        format!("\u{2212}{text}")
    } else {
        format!("+{text}")
    }
}

/// A number with `decimals` decimals (no `-0`), "—" when not finite.
pub fn fixed(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return MISSING.to_owned();
    }
    let text = format!("{:.*}", decimals.min(9), value);
    strip_negative_zero(&text).to_owned()
}

/// A number with at most `max_decimals` decimals and no trailing zeros:
/// `80.0` -> `"80"`, `0.150` -> `"0.15"`.
pub fn trimmed(value: f64, max_decimals: usize) -> String {
    if !value.is_finite() {
        return MISSING.to_owned();
    }
    let text = format!("{:.*}", max_decimals.min(9), value);
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    };
    strip_negative_zero(&text).to_owned()
}

/// A temperature: `"215.0 °C"` (one decimal), "—" when not finite.
pub fn temperature(celsius: f64) -> String {
    if !celsius.is_finite() {
        return MISSING.to_owned();
    }
    format!("{} °C", fixed(celsius, 1))
}

/// A relative change from `old` to `new` as a signed percentage
/// (`"+2.6%"`), or `None` when it cannot be computed (old is zero or a
/// value is not finite).
pub fn change(old: f64, new: f64) -> Option<String> {
    if !old.is_finite() || !new.is_finite() || old == 0.0 {
        return None;
    }
    let pct = (new - old) / old.abs() * 100.0;
    Some(format!("{}%", signed(pct, 1)))
}

fn strip_negative_zero(text: &str) -> &str {
    if text.starts_with('-') && text[1..].chars().all(|c| c == '0' || c == '.') {
        &text[1..]
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_use_the_most_significant_units() {
        assert_eq!(duration(0.0), "0s");
        assert_eq!(duration(0.4), "0s");
        assert_eq!(duration(38.0), "38s");
        assert_eq!(duration(59.6), "1m");
        assert_eq!(duration(252.0), "4m 12s");
        assert_eq!(duration(3600.0), "1h");
        assert_eq!(duration(2.0 * 3600.0 + 5.0 * 60.0 + 20.0), "2h 5m");
        assert_eq!(duration(86_400.0 + 2.0 * 3600.0 + 3.0 * 60.0), "1d 2h 3m");
        assert_eq!(duration(3.0 * 86_400.0 + 4.0 * 60.0), "3d 4m");
        // Seconds round into minutes above an hour.
        assert_eq!(duration(3600.0 + 59.0 * 60.0 + 45.0), "2h");
    }

    #[test]
    fn durations_reject_garbage() {
        assert_eq!(duration(f64::NAN), MISSING);
        assert_eq!(duration(-1.0), MISSING);
        assert_eq!(duration(f64::INFINITY), MISSING);
        // Enormous values do not overflow.
        assert!(duration(1e30).ends_with('m') || duration(1e30).ends_with('d'));
    }

    #[test]
    fn clocks_pad_minutes_and_seconds() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(42.9), "0:42");
        assert_eq!(clock(725.0), "12:05");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(-3.0), MISSING);
    }

    #[test]
    fn seconds_switch_to_durations_after_a_minute() {
        assert_eq!(seconds(8.42), "8.4 s");
        assert_eq!(seconds(59.9), "59.9 s");
        assert_eq!(seconds(75.0), "1m 15s");
        assert_eq!(seconds(f64::NAN), MISSING);
    }

    #[test]
    fn lengths_scale_units() {
        assert_eq!(length_mm(523.4), "523 mm");
        assert_eq!(length_mm(12_345.0), "12.35 m");
        assert_eq!(length_mm(1_240_000.0), "1.24 km");
        assert_eq!(length_mm(-50.0), "-50 mm");
        assert_eq!(length_mm(f64::NAN), MISSING);
    }

    #[test]
    fn bytes_use_binary_prefixes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(48_213), "47.1 KB");
        assert_eq!(bytes(1_843_271), "1.76 MB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.00 GB");
        assert!(bytes(u64::MAX).ends_with("PB"));
    }

    #[test]
    fn counts_group_thousands() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn rates_scale_decimals() {
        assert_eq!(rate(1234.4, "lines/s"), "1,234 lines/s");
        assert_eq!(rate(123.4, "lines/s"), "123 lines/s");
        assert_eq!(rate(12.34, ""), "12.3");
        assert_eq!(rate(0.4211, "Hz"), "0.42 Hz");
        assert_eq!(rate(-1.0, "x"), MISSING);
    }

    #[test]
    fn percents_and_signs() {
        assert_eq!(percent(0.985, 1), "98.5%");
        assert_eq!(percent(1.0, 0), "100%");
        assert_eq!(percent(-0.0001, 1), "0.0%");
        assert_eq!(percent(f64::NAN, 1), MISSING);
        assert_eq!(signed(0.12, 2), "+0.12");
        assert_eq!(signed(-0.05, 2), "\u{2212}0.05");
        assert_eq!(signed(-0.0001, 2), "0.00");
        assert_eq!(signed(f64::INFINITY, 2), MISSING);
    }

    #[test]
    fn fixed_and_trimmed_numbers() {
        assert_eq!(fixed(-0.001, 2), "0.00");
        assert_eq!(fixed(93.456, 2), "93.46");
        assert_eq!(trimmed(80.0, 3), "80");
        assert_eq!(trimmed(0.150, 3), "0.15");
        assert_eq!(trimmed(-0.0001, 2), "0");
        assert_eq!(trimmed(f64::NAN, 2), MISSING);
        assert_eq!(temperature(214.96), "215.0 °C");
    }

    #[test]
    fn changes_are_relative() {
        assert_eq!(change(93.0, 95.0).as_deref(), Some("+2.2%"));
        assert_eq!(change(100.0, 90.0).as_deref(), Some("\u{2212}10.0%"));
        assert_eq!(change(0.0, 5.0), None);
        assert_eq!(change(f64::NAN, 5.0), None);
    }
}
