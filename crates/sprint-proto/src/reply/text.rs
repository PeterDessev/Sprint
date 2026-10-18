//! Small text helpers shared by the reply parsers.

use super::HostAction;

/// `s` without `prefix`, compared ASCII case-insensitively.
pub(crate) fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let n = prefix.len();
    if s.len() >= n && s.is_char_boundary(n) && s[..n].eq_ignore_ascii_case(prefix) {
        Some(&s[n..])
    } else {
        None
    }
}

/// Whitespace-separated tokens with their byte offsets.
pub(crate) fn tokens_with_offsets(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(st)) => {
                out.push((st, &s[st..i]));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push((st, &s[st..]));
    }
    out
}

/// Parse an unsigned decimal integer (no sign, no fraction).
pub(crate) fn parse_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Remove one pair of surrounding quotes.
pub(crate) fn unquote(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(s)
}

/// Whether an error message means the firmware has stopped (needs M999,
/// a firmware restart or a power cycle).
pub(crate) fn is_fatal_error(message: &str) -> bool {
    const FATAL: [&str; 16] = [
        "halted",
        "kill() called",
        "killed",
        "system stopped",
        "printer stopped",
        "stopped due to errors",
        "thermal runaway",
        "mintemp",
        "maxtemp",
        "heating failed",
        "m999",
        "shutdown",
        "lost communication",
        "emergency stop",
        "watchdog",
        // Klipper verify_heater shutdown reason.
        "not heating at expected rate",
    ];
    let m = message.to_ascii_lowercase();
    FATAL.iter().any(|k| m.contains(k))
}

/// Parse the text after `action:` of a host action command.
pub(crate) fn parse_host_action(text: &str) -> HostAction {
    let text = text.trim();
    let (name, arg) = match text.split_once(char::is_whitespace) {
        Some((n, a)) => (n, a.trim()),
        None => (text, ""),
    };
    match name.to_ascii_lowercase().as_str() {
        "pause" => HostAction::Pause,
        "paused" => HostAction::Paused,
        "resume" => HostAction::Resume,
        "resumed" => HostAction::Resumed,
        "cancel" => HostAction::Cancel,
        "disconnect" => HostAction::Disconnect,
        "start" => HostAction::Start,
        "out_of_filament" => HostAction::OutOfFilament((!arg.is_empty()).then(|| arg.to_string())),
        "notification" => HostAction::Notification(arg.to_string()),
        "prompt_begin" => HostAction::PromptBegin(arg.to_string()),
        "prompt_choice" => HostAction::PromptChoice(arg.to_string()),
        "prompt_button" => HostAction::PromptButton(arg.to_string()),
        "prompt_show" => HostAction::PromptShow,
        "prompt_end" => HostAction::PromptEnd,
        _ => HostAction::Other(text.to_string()),
    }
}

/// Parse a duration such as "1y 2d 3h 4m 5s", "2d 3h 4m 5s", "45s",
/// "1h 2m" or Prusa's "12 min, 5 sec" into seconds. Years are 365 days.
pub fn parse_duration(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    let mut pending: Option<u64> = None; // number waiting for a unit word
    let mut any = false;
    for tok in text.split(|c: char| c.is_whitespace() || c == ',') {
        if tok.is_empty() {
            continue;
        }
        let digits = tok.bytes().take_while(u8::is_ascii_digit).count();
        let (num, unit) = tok.split_at(digits);
        let (value, unit) = if num.is_empty() {
            (pending.take()?, unit)
        } else {
            if pending.is_some() {
                return None;
            }
            let v: u64 = num.parse().ok()?;
            if unit.is_empty() {
                pending = Some(v);
                continue;
            }
            (v, unit)
        };
        let factor = match unit.to_ascii_lowercase().as_str() {
            "y" | "year" | "years" => 365 * 86_400,
            "d" | "day" | "days" => 86_400,
            "h" | "hour" | "hours" => 3600,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            _ => return None,
        };
        total = total.checked_add(value.checked_mul(factor)?)?;
        any = true;
    }
    (any && pending.is_none()).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_and_tokens() {
        assert_eq!(strip_prefix_ci("ECHO:hi", "echo:"), Some("hi"));
        assert_eq!(strip_prefix_ci("ec", "echo:"), None);
        assert_eq!(strip_prefix_ci("é", "e"), None);
        assert_eq!(
            tokens_with_offsets("  ok  N1 x"),
            vec![(2, "ok"), (6, "N1"), (9, "x")]
        );
        assert!(tokens_with_offsets("   ").is_empty());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1h 2m 3s"), Some(3723));
        assert_eq!(
            parse_duration("2d 3h 4m 5s"),
            Some(2 * 86_400 + 3 * 3600 + 4 * 60 + 5)
        );
        assert_eq!(parse_duration("1y 0d 0h 0m 1s"), Some(365 * 86_400 + 1));
        assert_eq!(parse_duration("45s"), Some(45));
        assert_eq!(parse_duration("12 min, 5 sec"), Some(725));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("5"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("99999999999999999999s"), None);
        assert_eq!(parse_duration("18446744073709551615y"), None);
    }

    #[test]
    fn host_actions() {
        assert_eq!(parse_host_action("pause"), HostAction::Pause);
        assert_eq!(
            parse_host_action("out_of_filament T0"),
            HostAction::OutOfFilament(Some("T0".into()))
        );
        assert_eq!(
            parse_host_action("out_of_filament"),
            HostAction::OutOfFilament(None)
        );
        assert_eq!(
            parse_host_action("prompt_begin Filament runout"),
            HostAction::PromptBegin("Filament runout".into())
        );
        assert_eq!(
            parse_host_action("probe_rewipe"),
            HostAction::Other("probe_rewipe".into())
        );
    }
}
