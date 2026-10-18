//! Small helpers shared by the widgets.

use std::borrow::Cow;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Clamp to `0..=1`; NaN becomes 0.
pub(crate) fn clamp01(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// Make text safe to draw on one terminal row: tabs become spaces and
/// other control characters become `·`, so printer output cannot move the
/// cursor or change colors.
pub(crate) fn sanitize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push('·'),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Display width of a string in terminal columns.
pub(crate) fn width(text: &str) -> usize {
    text.width()
}

/// Display width of a character (0 for control and combining characters).
pub(crate) fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// The longest prefix of `text` that fits in `max` columns, with `…` as
/// the last column when something had to be cut.
pub(crate) fn truncate(text: &str, max: usize) -> Cow<'_, str> {
    if width(text) <= max {
        return Cow::Borrowed(text);
    }
    if max == 0 {
        return Cow::Borrowed("");
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > max - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    Cow::Owned(out)
}

/// The last part of `text` that fits in `max` columns, with `…` in front
/// when something had to be cut (for file paths).
pub(crate) fn truncate_start(text: &str, max: usize) -> Cow<'_, str> {
    if width(text) <= max {
        return Cow::Borrowed(text);
    }
    if max == 0 {
        return Cow::Borrowed("");
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = char_width(c);
        if used + w > max - 1 {
            break;
        }
        kept.push(c);
        used += w;
    }
    let mut out = String::from("…");
    out.extend(kept.iter().rev());
    Cow::Owned(out)
}

/// Partial-block characters for a smooth bar, by eighths (index 1..=7).
const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// A bar of `cells` columns filled to `fraction`, as (filled, empty) parts:
/// whole blocks plus one partial block, then `empty` characters.
pub(crate) fn bar(fraction: f32, cells: usize, empty: char) -> (String, String) {
    let eighths_total = cells * 8;
    // `clamp01` keeps the product within 0..=eighths_total.
    let filled_eighths = (clamp01(fraction) * eighths_total as f32).round() as usize;
    let full = filled_eighths / 8;
    let partial = filled_eighths % 8;
    let mut filled: String = std::iter::repeat_n('█', full).collect();
    let mut used = full;
    if partial > 0 && used < cells {
        filled.push(EIGHTHS[partial]);
        used += 1;
    }
    let rest: String = std::iter::repeat_n(empty, cells.saturating_sub(used)).collect();
    (filled, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_handles_nan() {
        assert_eq!(clamp01(f32::NAN), 0.0);
        assert_eq!(clamp01(-1.0), 0.0);
        assert_eq!(clamp01(2.0), 1.0);
        assert_eq!(clamp01(0.25), 0.25);
    }

    #[test]
    fn sanitize_replaces_controls() {
        assert!(matches!(sanitize("ok T:20"), Cow::Borrowed(_)));
        assert_eq!(sanitize("a\tb"), "a    b");
        assert_eq!(sanitize("x\u{1b}[31my\r"), "x·[31my·");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(truncate("hello", 1), "…");
        assert_eq!(truncate_start("C:/prints/cube.gcode", 10), "…ube.gcode");
        assert_eq!(truncate_start("cube", 10), "cube");
        // Wide characters never overflow.
        let t = truncate("日本語テキスト", 5);
        assert!(width(&t) <= 5, "{t}");
    }

    #[test]
    fn bars() {
        assert_eq!(bar(0.0, 4, '·'), (String::new(), "····".to_string()));
        assert_eq!(bar(1.0, 4, '·'), ("████".to_string(), String::new()));
        assert_eq!(bar(0.5, 4, ' '), ("██".to_string(), "  ".to_string()));
        assert_eq!(bar(0.5625, 2, ' '), ("█▏".to_string(), String::new()));
        assert_eq!(bar(f32::NAN, 3, '-'), (String::new(), "---".to_string()));
        assert_eq!(bar(0.5, 0, '-'), (String::new(), String::new()));
    }
}
