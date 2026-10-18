//! Number and duration formatting the way Marlin prints them.

/// Format with a fixed number of decimals, never printing "-0.00" for an
/// exact zero (Arduino's `Print::printFloat` behaviour).
pub(crate) fn fixed(value: f64, decimals: usize) -> String {
    // Non-finite values never come from the model; print them as zero
    // rather than "inf"/"NaN", which no host parses.
    let v = if value.is_finite() && value != 0.0 {
        value
    } else {
        0.0
    };
    let s = format!("{v:.decimals$}");
    // A tiny negative value rounds to "-0.00"; Marlin prints the sign too,
    // but a clean zero reads better and hosts parse both identically.
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
        s[1..].to_string()
    } else {
        s
    }
}

/// Two decimals (Marlin's default float precision).
pub(crate) fn f2(value: f64) -> String {
    fixed(value, 2)
}

/// Three decimals with an explicit sign, as in Marlin's leveling grids
/// ("+0.123", "-0.045").
pub(crate) fn signed3(value: f64) -> String {
    let s = fixed(value, 3);
    if s.starts_with('-') {
        s
    } else {
        format!("+{s}")
    }
}

/// Marlin `duration_t::toString` ("1d 2h 3m 4s", "5m 0s", "12s").
pub(crate) fn duration(total_secs: u64) -> String {
    let y = total_secs / (365 * 86_400);
    let d = (total_secs / 86_400) % 365;
    let h = (total_secs / 3600) % 24;
    let m = (total_secs / 60) % 60;
    let s = total_secs % 60;
    if y > 0 {
        format!("{y}y {d}d {h}h {m}m {s}s")
    } else if d > 0 {
        format!("{d}d {h}h {m}m {s}s")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Seconds (possibly fractional, possibly garbage) to whole seconds.
pub(crate) fn whole_secs(secs: f64) -> u64 {
    if secs.is_finite() && secs > 0.0 {
        secs.min(1e15) as u64
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_formats() {
        assert_eq!(f2(10.0), "10.00");
        assert_eq!(f2(-0.0), "0.00");
        assert_eq!(f2(-0.001), "0.00");
        assert_eq!(f2(-1.234), "-1.23");
        assert_eq!(fixed(20.04, 1), "20.0");
        assert_eq!(f2(f64::NAN), "0.00");
        assert_eq!(signed3(0.1234), "+0.123");
        assert_eq!(signed3(-0.045), "-0.045");
        assert_eq!(signed3(0.0), "+0.000");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(61), "1m 1s");
        assert_eq!(duration(3723), "1h 2m 3s");
        assert_eq!(duration(90_061), "1d 1h 1m 1s");
        assert_eq!(duration(365 * 86_400 + 1), "1y 0d 0h 0m 1s");
        assert_eq!(whole_secs(-5.0), 0);
        assert_eq!(whole_secs(f64::NAN), 0);
        assert_eq!(whole_secs(12.9), 12);
    }
}
