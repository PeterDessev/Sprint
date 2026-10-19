//! G-code script templating for the pause, resume and cancel scripts and
//! user macros.
//!
//! A template is G-code with one command per line. `;` comments and blank
//! lines are dropped. These placeholders are expanded:
//!
//! | Placeholder | Value |
//! |---|---|
//! | `{x}` `{y}` `{z}` | logical position (mm) from the [`MachineState`] |
//! | `{e}` | logical extruder position (mm) |
//! | `{f}` | feedrate in mm/min |
//! | `{tool}` | active tool number |
//! | `{hotend}` | target temperature of the active tool (°C) |
//! | `{bed}` | bed target temperature (°C) |
//! | `{fan}` | part-cooling fan speed (0..=255) |
//! | `{max_x}` `{max_y}` `{max_z}` | build volume limits from the profile (mm) |
//! | `{park_x}` `{park_y}` | parking position from the profile (mm) |
//!
//! Unknown placeholders are kept verbatim. A line that uses `{e}` is
//! dropped while the extruder is in relative mode (M83 or G91), because
//! there is no absolute extruder position to restore then; this is what
//! makes the default resume script's `G92 E{e}` disappear for files that
//! extrude relatively.

use sprint_gcode::{MachineState, clean_line};

/// Default pause script: retract, lift, park.
pub const DEFAULT_PAUSE_SCRIPT: &str =
    "M400\nG91\nG1 E-2 F2700\nG1 Z10 F600\nG90\nG1 X{park_x} Y{park_y} F6000\n";

/// Default resume script: return above the part, lower, prime, restore the
/// extruder position and feedrate.
pub const DEFAULT_RESUME_SCRIPT: &str =
    "G1 X{x} Y{y} F6000\nG1 Z{z} F600\nG91\nG1 E2 F2700\nG90\nG92 E{e}\nG1 F{f}\n";

/// Default cancel script: heaters and fan off, lift, home X/Y, motors off.
pub const DEFAULT_CANCEL_SCRIPT: &str =
    "M104 S0\nM140 S0\nM107\nG91\nG1 Z10 F600\nG90\nG28 X Y\nM84\n";

/// Values a script template is rendered with: the machine state plus the
/// profile-derived geometry placeholders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScriptContext<'a> {
    /// Positions, modes, temperatures and fan (e.g. at pause time).
    pub state: &'a MachineState,
    /// `{max_x}`: largest X coordinate of the build volume (mm).
    pub max_x: f32,
    /// `{max_y}`: largest Y coordinate of the build volume (mm).
    pub max_y: f32,
    /// `{max_z}`: largest Z coordinate of the build volume (mm).
    pub max_z: f32,
    /// `{park_x}`: X of the parking position (mm).
    pub park_x: f32,
    /// `{park_y}`: Y of the parking position (mm).
    pub park_y: f32,
}

impl<'a> ScriptContext<'a> {
    /// A context for a 220 x 220 x 250 mm printer with its origin at the
    /// front left corner, parking at X10 Y210 (the defaults of
    /// [`PrinterProfile`](crate::config::PrinterProfile)).
    pub fn new(state: &'a MachineState) -> ScriptContext<'a> {
        ScriptContext {
            state,
            max_x: 220.0,
            max_y: 220.0,
            max_z: 250.0,
            park_x: 10.0,
            park_y: 210.0,
        }
    }
}

/// Expand the placeholders of `template` with values from `state` (and the
/// default geometry of [`ScriptContext::new`]). Returns one cleaned command
/// per line; comments and blank lines are dropped.
///
/// ```
/// use sprint_host::render_script;
/// use sprint_host::sprint_gcode::MachineState;
///
/// let mut state = MachineState::new();
/// state.position = [12.5, 30.0, 0.2, 4.25];
/// let lines = render_script("G1 X{x} Y{y} F6000 ; back\nG92 E{e}", &state);
/// assert_eq!(lines, ["G1 X12.5 Y30 F6000", "G92 E4.25"]);
/// ```
pub fn render_script(template: &str, state: &MachineState) -> Vec<String> {
    render_script_with(template, &ScriptContext::new(state))
}

/// Expand the placeholders of `template` with the values in `ctx`. See the
/// [module documentation](self) for the placeholders.
pub fn render_script_with(template: &str, ctx: &ScriptContext<'_>) -> Vec<String> {
    let relative_e = !ctx.state.absolute_e;
    let mut out = Vec::new();
    for raw in template.split(['\n', '\r']) {
        let code = match raw.find(';') {
            Some(i) => &raw[..i],
            None => raw,
        };
        if code.trim().is_empty() {
            continue;
        }
        if relative_e && code.contains("{e}") {
            continue;
        }
        let rendered = expand(code, ctx);
        if let Some(cmd) = clean_line(&rendered) {
            out.push(cmd.into_owned());
        }
    }
    out
}

/// Replace every known `{name}` in `text`.
fn expand(text: &str, ctx: &ScriptContext<'_>) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let next_open = after.find('{');
        match after.find('}') {
            // "{{x}": this brace is literal, the inner one may be a placeholder.
            Some(close) if next_open.is_some_and(|o| o < close) => {
                out.push('{');
                rest = after;
            }
            Some(close) => {
                let name = &after[..close];
                match value(name, ctx) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn value(name: &str, ctx: &ScriptContext<'_>) -> Option<String> {
    let s = ctx.state;
    Some(match name {
        "x" => format_number(s.position[0], 3),
        "y" => format_number(s.position[1], 3),
        "z" => format_number(s.position[2], 3),
        "e" => format_number(s.position[3], 5),
        "f" => format_number(s.feedrate_mm_min, 1),
        "tool" => s.tool.to_string(),
        "hotend" => format_number(f64::from(s.hotend_target(s.tool)), 1),
        "bed" => format_number(f64::from(s.bed_target), 1),
        "fan" => s.fan.to_string(),
        "max_x" => format_number(f64::from(ctx.max_x), 3),
        "max_y" => format_number(f64::from(ctx.max_y), 3),
        "max_z" => format_number(f64::from(ctx.max_z), 3),
        "park_x" => format_number(f64::from(ctx.park_x), 3),
        "park_y" => format_number(f64::from(ctx.park_y), 3),
        _ => return None,
    })
}

/// Format a number for G-code: at most `decimals` decimals, trailing zeros
/// and a trailing point removed, `-0` printed as `0`, non-finite values as
/// `0`.
pub fn format_number(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{value:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MachineState {
        let mut s = MachineState::new();
        s.position = [101.25, 87.5, 12.4, 1234.567891];
        s.feedrate_mm_min = 1800.0;
        s.tool = 1;
        s.hotend_targets = vec![200.0, 215.0];
        s.bed_target = 60.0;
        s.fan = 128;
        s
    }

    #[test]
    fn expands_every_placeholder() {
        let s = state();
        let ctx = ScriptContext {
            state: &s,
            max_x: 250.0,
            max_y: 210.0,
            max_z: 220.5,
            park_x: 5.0,
            park_y: 200.0,
        };
        let lines = render_script_with(
            "G1 X{x} Y{y} Z{z} F{f}\nG92 E{e}\nT{tool}\nM104 S{hotend}\nM140 S{bed}\nM106 S{fan}\nG1 X{max_x} Y{max_y} Z{max_z}\nG1 X{park_x} Y{park_y}",
            &ctx,
        );
        assert_eq!(
            lines,
            [
                "G1 X101.25 Y87.5 Z12.4 F1800",
                "G92 E1234.56789",
                "T1",
                "M104 S215",
                "M140 S60",
                "M106 S128",
                "G1 X250 Y210 Z220.5",
                "G1 X5 Y200",
            ]
        );
    }

    #[test]
    fn drops_comments_blank_lines_and_keeps_unknown_placeholders() {
        let s = state();
        let lines = render_script(
            "; a comment\n\n  G28 X Y ; home\r\nM117 {unknown} {x}\nG1 X{x\n",
            &s,
        );
        assert_eq!(lines, ["G28 X Y", "M117 {unknown} 101.25", "G1 X{x"]);
    }

    #[test]
    fn relative_extrusion_drops_lines_using_e() {
        let mut s = state();
        s.absolute_e = false;
        s.relative_e_mode = true;
        let lines = render_script(DEFAULT_RESUME_SCRIPT, &s);
        assert!(lines.iter().all(|l| !l.starts_with("G92")), "{lines:?}");
        assert_eq!(lines.len(), 6);
        let s = state();
        let lines = render_script(DEFAULT_RESUME_SCRIPT, &s);
        assert_eq!(
            lines,
            [
                "G1 X101.25 Y87.5 F6000",
                "G1 Z12.4 F600",
                "G91",
                "G1 E2 F2700",
                "G90",
                "G92 E1234.56789",
                "G1 F1800",
            ]
        );
    }

    #[test]
    fn default_scripts_render() {
        let s = state();
        assert_eq!(
            render_script(DEFAULT_PAUSE_SCRIPT, &s),
            [
                "M400",
                "G91",
                "G1 E-2 F2700",
                "G1 Z10 F600",
                "G90",
                "G1 X10 Y210 F6000"
            ]
        );
        assert_eq!(
            render_script(DEFAULT_CANCEL_SCRIPT, &s),
            [
                "M104 S0", "M140 S0", "M107", "G91", "G1 Z10 F600", "G90", "G28 X Y", "M84"
            ]
        );
    }

    #[test]
    fn number_formatting() {
        assert_eq!(format_number(10.0, 3), "10");
        assert_eq!(format_number(10.5, 3), "10.5");
        assert_eq!(format_number(-0.0001, 3), "0");
        assert_eq!(format_number(-2.0, 3), "-2");
        assert_eq!(format_number(1.23456, 2), "1.23");
        assert_eq!(format_number(f64::NAN, 3), "0");
        assert_eq!(format_number(f64::INFINITY, 3), "0");
        assert_eq!(format_number(7.0, 0), "7");
    }

    #[test]
    fn garbage_templates_do_not_panic() {
        let s = state();
        for t in ["{", "}", "{{x}}", "{\u{e9}}", "G1 X{x}{", "\u{1F600}{x}", ";{e}", "{e}"] {
            let _ = render_script(t, &s);
        }
        assert_eq!(render_script("{{x}}", &s), ["{101.25}"]);
    }
}
