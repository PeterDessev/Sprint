//! PID autotune (M303) output.
//!
//! Marlin prints intermediate estimates every cycle (" Kp: 22.20 Ki: 1.08
//! Kd: 114.00") and, when done, "PID Autotune finished! Put the last Kp, Ki
//! and Kd constants from below into Configuration.h" followed by three
//! `#define DEFAULT_Kp ...` lines. Prusa prints Kp, Ki and Kd on separate
//! lines and says "from above". The tracker turns a whole session into
//! exactly one [`Reply::AutotuneResult`].

use super::Reply;
use super::text::strip_prefix_ci;

/// State of an M303 run, as far as the printed output tells.
#[derive(Clone, Debug, Default)]
pub(crate) struct PidTracker {
    /// "PID Autotune start" was seen; intermediate results are not final.
    session: bool,
    /// "finished ... from below": the `#define` lines follow.
    awaiting_defines: bool,
    /// Kp / Ki / Kd collected from "Kp:" style lines.
    partial: [Option<f32>; 3],
    /// Kp / Ki / Kd collected from `#define DEFAULT_*` lines.
    defines: [Option<f32>; 3],
    /// Latest complete intermediate estimate.
    last: Option<(f32, f32, f32)>,
}

fn number(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok().filter(|v| v.is_finite())
}

fn result((kp, ki, kd): (f32, f32, f32)) -> Reply {
    Reply::AutotuneResult { kp, ki, kd }
}

fn complete(slots: &[Option<f32>; 3]) -> Option<(f32, f32, f32)> {
    Some((slots[0]?, slots[1]?, slots[2]?))
}

/// "#define DEFAULT_Kp 22.20", "#define DEFAULT_bedKi 1.08",
/// "#define DEFAULT_chamberKd 300" -> (slot, value).
fn parse_define(body: &str) -> Option<(usize, f32)> {
    let rest = strip_prefix_ci(body, "#define DEFAULT_")?;
    let rest = strip_prefix_ci(rest, "bed")
        .or_else(|| strip_prefix_ci(rest, "chamber"))
        .unwrap_or(rest);
    let (name, value) = rest.split_once(char::is_whitespace)?;
    let slot = match name {
        "Kp" => 0,
        "Ki" => 1,
        "Kd" => 2,
        _ => return None,
    };
    Some((slot, number(value)?))
}

/// "Kp: 22.20 Ki: 1.08 Kd: 114.00" (any subset) -> values found.
fn parse_gains(body: &str) -> Option<[Option<f32>; 3]> {
    let toks: Vec<&str> = body.split_whitespace().collect();
    let mut found = [None; 3];
    let mut any = false;
    let mut i = 0;
    while i < toks.len() {
        let tok = toks[i];
        i += 1;
        let (key, glued) = match tok.split_once(':') {
            Some((k, v)) => (k, v),
            None => continue,
        };
        let slot = match key {
            "Kp" => 0,
            "Ki" => 1,
            "Kd" => 2,
            _ => continue,
        };
        let text = if glued.is_empty() {
            let next = toks.get(i).copied().unwrap_or("");
            i += 1;
            next
        } else {
            glued
        };
        found[slot] = Some(number(text)?);
        any = true;
    }
    any.then_some(found)
}

/// Klipper: "PID parameters: pid_Kp=22.865 pid_Ki=1.292 pid_Kd=101.178".
pub(crate) fn parse_klipper_pid(body: &str) -> Option<Reply> {
    let rest = strip_prefix_ci(body, "PID parameters:")?;
    let mut slots = [None; 3];
    for tok in rest.split_whitespace() {
        let (key, value) = tok.split_once('=')?;
        let slot = match key.to_ascii_lowercase().as_str() {
            "pid_kp" => 0,
            "pid_ki" => 1,
            "pid_kd" => 2,
            _ => continue,
        };
        slots[slot] = Some(number(value)?);
    }
    complete(&slots).map(result)
}

impl PidTracker {
    fn reset(&mut self) {
        *self = PidTracker::default();
    }

    /// Handle a line (with any `echo:` prefix removed; `raw` is the full
    /// line). Returns `None` when the line is not autotune output.
    pub(crate) fn feed(&mut self, body: &str, raw: &str) -> Option<Vec<Reply>> {
        let other = || vec![Reply::Other(raw.to_string())];
        if strip_prefix_ci(body, "PID Autotune start").is_some() {
            self.reset();
            self.session = true;
            return Some(other());
        }
        if let Some(rest) = strip_prefix_ci(body, "PID Autotune failed") {
            let reason = rest.trim_start_matches('!').trim();
            let reason = if reason.is_empty() { body } else { reason };
            self.reset();
            return Some(vec![Reply::AutotuneFailed(reason.to_string())]);
        }
        if let Some(rest) = strip_prefix_ci(body, "PID Autotune finished") {
            if !self.session {
                return Some(other());
            }
            if rest.to_ascii_lowercase().contains("below") || self.last.is_none() {
                self.awaiting_defines = true;
                return Some(other());
            }
            let last = self.last;
            self.reset();
            let mut out = other();
            out.extend(last.map(result));
            return Some(out);
        }
        if let Some((slot, value)) = parse_define(body) {
            self.defines[slot] = Some(value);
            if let Some(gains) = complete(&self.defines) {
                self.reset();
                return Some(vec![result(gains)]);
            }
            return Some(Vec::new());
        }
        if let Some(found) = parse_gains(body) {
            for (slot, value) in self.partial.iter_mut().zip(found) {
                if value.is_some() {
                    *slot = value;
                }
            }
            let Some(gains) = complete(&self.partial) else {
                return Some(Vec::new());
            };
            self.partial = [None; 3];
            if self.session {
                self.last = Some(gains);
                return Some(other());
            }
            return Some(vec![result(gains)]);
        }
        None
    }

    /// An `ok` arrived: if the final `#define` lines never came, report the
    /// last estimate instead.
    pub(crate) fn on_ok(&mut self) -> Option<Reply> {
        if !self.awaiting_defines {
            return None;
        }
        let last = self.last;
        self.reset();
        last.map(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(parse_define("#define DEFAULT_Kp 22.20"), Some((0, 22.2)));
        assert_eq!(parse_define("#define DEFAULT_bedKi 1.08"), Some((1, 1.08)));
        assert_eq!(
            parse_define("#define DEFAULT_chamberKd 300"),
            Some((2, 300.0))
        );
        assert_eq!(parse_define("#define DEFAULT_Kx 1"), None);
        assert_eq!(parse_define("#define DEFAULT_Kp abc"), None);
        assert_eq!(
            parse_gains(" Kp: 22.20 Ki: 1.08 Kd: 114.00"),
            Some([Some(22.2), Some(1.08), Some(114.0)])
        );
        assert_eq!(
            parse_gains(" Classic PID  Kp: 22.20"),
            Some([Some(22.2), None, None])
        );
        assert_eq!(
            parse_gains("Kp:1 Ki:2 Kd:3"),
            Some([Some(1.0), Some(2.0), Some(3.0)])
        );
        assert_eq!(parse_gains("Kp: nope"), None);
        assert_eq!(
            parse_gains("bias: 92 d: 92 min: 196.56 max: 203.75 Ku: 34.27 Tu: 26.44"),
            None
        );
        assert_eq!(
            parse_klipper_pid("PID parameters: pid_Kp=22.865 pid_Ki=1.292 pid_Kd=101.178"),
            Some(Reply::AutotuneResult {
                kp: 22.865,
                ki: 1.292,
                kd: 101.178
            })
        );
        assert_eq!(parse_klipper_pid("PID parameters: pid_Kp=1"), None);
    }
}
