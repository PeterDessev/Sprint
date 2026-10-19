//! Turning the pause, resume and cancel templates into the commands a job
//! sends: rendering with the profile, millimetres for inch streams, and
//! leaving out moves of axes that are not known to be homed.

use sprint_gcode::{MachineState, parse_line};

use super::Shared;
use crate::console::ConsoleKind;
use crate::script::render_script_with;
use crate::state::Axes;

/// Render a pause, resume or cancel script for `state` with the profile's
/// geometry; `{heaters_off}` also covers every heater the printer reports.
pub(super) fn render_job_script(
    shared: &Shared,
    template: &str,
    state: &MachineState,
) -> Vec<String> {
    let temps = shared.state.read().temps.clone();
    let profile = shared.profile.read();
    render_script_with(template, &profile.script_context(state).covering(&temps))
}

/// Scripts are written in millimetres (and their placeholders are mm): for
/// a stream in inch mode (G20) run them after a G21 and, with `restore`,
/// switch back to G20 at the end.
pub(super) fn in_millimetres(
    mut lines: Vec<String>,
    state: &MachineState,
    restore: bool,
) -> Vec<String> {
    if state.inches && !lines.is_empty() {
        lines.insert(0, "G21".to_string());
        if restore {
            lines.push("G20".to_string());
        }
    }
    lines
}

/// Leave the X/Y/Z words out of the moves (G0..G3) of axes that are not
/// known to be homed: Marlin keeps no soft endstops for them, so a park or
/// a lift relative to an unknown position can drive the head into the
/// frame. A `G28` in the script homes its axes for the lines after it. A
/// move left with nothing to do is dropped, an arc on an unhomed axis
/// entirely. Returns the lines and the axes whose moves were left out.
pub(super) fn guard_unhomed(lines: Vec<String>, homed: Option<Axes>) -> (Vec<String>, Axes) {
    let mut homed = homed.unwrap_or(Axes::NONE);
    let mut skipped = Axes::NONE;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let parsed = parse_line(&line);
        let Some(code) = parsed.code.filter(|c| c.letter == b'G' && c.sub.is_none()) else {
            out.push(line);
            continue;
        };
        let listed = Axes {
            x: parsed.has('X'),
            y: parsed.has('Y'),
            z: parsed.has('Z'),
        };
        match code.number {
            28 => {
                homed = homed.union(if listed.is_empty() { Axes::ALL } else { listed });
                out.push(line);
            }
            0..=3 => {
                let unhomed = listed.without(homed);
                if unhomed.is_empty() {
                    out.push(line);
                    continue;
                }
                skipped = skipped.union(unhomed);
                if code.number >= 2 {
                    continue;
                }
                let kept: Vec<&str> = line
                    .split_whitespace()
                    .filter(|w| {
                        let axis = w.chars().next().map(|c| c.to_ascii_uppercase());
                        !matches!(axis, Some('X') if unhomed.x)
                            && !matches!(axis, Some('Y') if unhomed.y)
                            && !matches!(axis, Some('Z') if unhomed.z)
                    })
                    .collect();
                let kept = kept.join(" ");
                let rest = parse_line(&kept);
                let still_unhomed = [('X', unhomed.x), ('Y', unhomed.y), ('Z', unhomed.z)]
                    .iter()
                    .any(|&(a, on)| on && rest.has(a));
                let moves = ['X', 'Y', 'Z', 'E'].iter().any(|&a| rest.has(a));
                // Words that could not be separated ("G1X10Y20") drop the
                // whole line.
                if moves && !still_unhomed {
                    out.push(kept);
                }
            }
            _ => out.push(line),
        }
    }
    (out, skipped)
}

/// [`guard_unhomed`], saying in the console what the `script` (pause,
/// cancel) leaves out.
pub(super) fn guarded(
    shared: &Shared,
    lines: Vec<String>,
    homed: Option<Axes>,
    script: &str,
) -> Vec<String> {
    let (lines, skipped) = guard_unhomed(lines, homed);
    if !skipped.is_empty() {
        shared.log(
            ConsoleKind::Warning,
            format!(
                "{} not homed: the {script} script does not move {} (from an unknown position \
                 a move could hit the frame)",
                skipped.letters(),
                if skipped.letters().len() > 1 {
                    "them"
                } else {
                    "it"
                }
            ),
            false,
            None,
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn homed_axes_move_as_written() {
        let script = lines("M400\nG91\nG1 E-2 F2700\nG1 Z10 F600\nG90\nG1 X10 Y210 F6000");
        let (out, skipped) = guard_unhomed(script.clone(), Some(Axes::ALL));
        assert_eq!(out, script);
        assert!(skipped.is_empty());
    }

    #[test]
    fn unhomed_axes_are_neither_parked_nor_lifted() {
        let script = lines("M400\nG91\nG1 E-2 F2700\nG1 Z10 F600\nG90\nG1 X10 Y210 F6000");
        let (out, skipped) = guard_unhomed(script.clone(), None);
        assert_eq!(out, ["M400", "G91", "G1 E-2 F2700", "G90"]);
        assert_eq!(skipped, Axes::ALL);
        // Z homed (e.g. G28 Z): the lift stays, the park goes.
        let (out, skipped) = guard_unhomed(script, Some(Axes::Z));
        assert_eq!(out, ["M400", "G91", "G1 E-2 F2700", "G1 Z10 F600", "G90"]);
        assert_eq!(skipped, Axes::XY);
    }

    #[test]
    fn partial_moves_keep_their_homed_axes_and_extrusion() {
        let (out, skipped) = guard_unhomed(
            lines("G1 X5 Y6 Z7 E1 F100\nG0 Y3\nG2 X1 Y1 I1 J0\nG1X10Y20"),
            Some(Axes::X),
        );
        assert_eq!(out, ["G1 X5 E1 F100"]);
        assert_eq!(
            skipped,
            Axes {
                x: true,
                y: true,
                z: true
            }
            .without(Axes::X)
        );
    }

    #[test]
    fn homing_inside_the_script_counts() {
        let (out, skipped) = guard_unhomed(lines("G91\nG1 Z10\nG90\nG28 X Y\nG1 X5 Y5\nM84"), None);
        assert_eq!(out, ["G91", "G90", "G28 X Y", "G1 X5 Y5", "M84"]);
        assert_eq!(skipped, Axes::Z);
    }
}
