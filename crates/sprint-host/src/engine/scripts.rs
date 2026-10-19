//! Turning the pause, resume and cancel templates into the commands a job
//! sends: rendering with the profile, the modes the templates are written
//! for (and the stream's modes back after a resume), and leaving out moves
//! of axes that are not known to be homed.

use std::collections::VecDeque;

use sprint_gcode::{MachineState, parse_line};
use sprint_proto::{HeaterReading, TempReport};

use super::Shared;
use super::meta::Meta;
use crate::console::ConsoleKind;
use crate::script::{format_number, render_script_with};
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

/// The printer's modes once the queued user commands have run: `printer`
/// (every command sent so far) with `queued` applied. Scripts go after the
/// commands queued before them, so a `G91` or `G20` typed while paused
/// counts even when it has not been sent yet.
pub(super) fn modes_after(printer: &MachineState, queued: &VecDeque<Meta>) -> MachineState {
    let mut modes = printer.clone();
    for m in queued {
        modes.apply(&parse_line(&m.text));
    }
    modes
}

/// A pause or cancel script with the modes templates are written for
/// first: millimetres (their placeholders and distances are mm) and
/// absolute positioning, each only when `printer` is not in it already.
pub(super) fn in_script_modes(lines: Vec<String>, printer: &MachineState) -> Vec<String> {
    if lines.is_empty() {
        return lines;
    }
    let mut out = Vec::with_capacity(lines.len() + 2);
    if printer.inches {
        out.push("G21".to_string());
    }
    if !printer.absolute {
        out.push("G90".to_string());
    }
    out.extend(lines);
    out
}

/// How far below its target a heater the job uses may have cooled while
/// paused before the resume waits for it again (°C).
const REHEAT_MARGIN: f32 = 5.0;

/// What the printer needs when a paused job resumes, besides the template.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ResumeExtras {
    /// Heaters back to the job's targets (set, then wait).
    pub reheat: Vec<String>,
    /// The part fan back to the job's speed.
    pub fan: Option<String>,
}

impl ResumeExtras {
    /// What it takes to continue the job of `snapshot` (the state at pause
    /// time) on a printer reporting `temps` with its part fan at `fan`:
    /// every heater the job used whose target was changed, or that cooled
    /// down more than a few degrees, goes back to the job's target (all
    /// targets first, then the waits: bed, chamber, hotends), and a part fan
    /// changed while paused gets its speed back.
    pub(super) fn needed(snapshot: &MachineState, temps: &TempReport, fan: Option<u8>) -> Self {
        let cooled = |target: f32, reading: Option<&HeaterReading>| {
            target > 0.0
                && reading.is_none_or(|r| {
                    r.target.is_none_or(|t| (t - target).abs() > 0.5)
                        || r.current.is_nan()
                        || r.current < target - REHEAT_MARGIN
                })
        };
        let t = |v: f32| format_number(f64::from(v), 1);
        let (mut set, mut wait) = (Vec::new(), Vec::new());
        if cooled(snapshot.bed_target, temps.bed.as_ref()) {
            set.push(format!("M140 S{}", t(snapshot.bed_target)));
            wait.push(format!("M190 S{}", t(snapshot.bed_target)));
        }
        if cooled(snapshot.chamber_target, temps.chamber.as_ref()) {
            set.push(format!("M141 S{}", t(snapshot.chamber_target)));
            wait.push(format!("M191 S{}", t(snapshot.chamber_target)));
        }
        let several = snapshot.hotend_targets.len() > 1;
        for (i, &target) in snapshot.hotend_targets.iter().enumerate() {
            if cooled(target, temps.hotends.get(i)) {
                let tool = if several {
                    format!("T{i} ")
                } else {
                    String::new()
                };
                set.push(format!("M104 {tool}S{}", t(target)));
                wait.push(format!("M109 {tool}S{}", t(target)));
            }
        }
        set.extend(wait);
        let fan = (fan != Some(snapshot.fan)).then(|| match snapshot.fan {
            0 => "M107".to_string(),
            speed => format!("M106 S{speed}"),
        });
        ResumeExtras { reheat: set, fan }
    }
}

/// The resume script: the rendered template between the modes it is
/// written for and the stream's modes.
///
/// Before: `G21` when the printer is in inches, `G90`, and the stream's
/// extruder mode (`M82` makes the template's `G92 E{e}` meaningful), so a
/// `G91` or `G20` typed while paused cannot send the return move and the
/// rest of the layer elsewhere; then the heaters back to the job's
/// temperatures (while still parked). After: the part fan, the stream's
/// positioning and extruder modes explicitly (on Marlin `G90`/`G91` also
/// switch the extruder mode) and `G20` for an inch stream.
pub(super) fn resume_script(
    template: Vec<String>,
    snapshot: &MachineState,
    printer: &MachineState,
    extras: ResumeExtras,
) -> Vec<String> {
    let e_mode = if snapshot.absolute_e { "M82" } else { "M83" };
    let mut out = Vec::with_capacity(template.len() + extras.reheat.len() + 8);
    if printer.inches || snapshot.inches {
        out.push("G21".to_string());
    }
    out.push("G90".to_string());
    out.push(e_mode.to_string());
    out.extend(extras.reheat);
    out.extend(template);
    out.extend(extras.fan);
    out.push(if snapshot.absolute { "G90" } else { "G91" }.to_string());
    out.push(e_mode.to_string());
    if snapshot.inches {
        out.push("G20".to_string());
    }
    out
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

/// Whether hotend `tool` is not short of its target: false while it heats
/// up (or cooled down) to more than a few degrees below a target above 0,
/// true otherwise, also when nothing is reported or nothing is heated (a
/// file that allows cold extrusion).
pub(super) fn hotend_hot(temps: &TempReport, tool: u8) -> bool {
    temps.hotends.get(usize::from(tool)).is_none_or(|r| {
        r.target
            .is_none_or(|t| t <= 0.0 || r.current >= t - REHEAT_MARGIN)
    })
}

/// `lines` without the moves that only drive the extruder (a retract or a
/// prime): G0/G1 with `E` and no X/Y/Z.
pub(super) fn without_extruder_moves(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter(|l| {
            let p = parse_line(l);
            let moves = p
                .code
                .is_some_and(|c| c.letter == b'G' && c.sub.is_none() && c.number <= 1);
            !(moves && p.has('E') && !p.has('X') && !p.has('Y') && !p.has('Z'))
        })
        .collect()
}

/// [`guard_unhomed`], saying in the console what the `script` (pause,
/// resume, cancel) leaves out.
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

    fn modes(gcode: &[&str]) -> MachineState {
        let mut m = MachineState::new();
        for l in gcode {
            m.apply(&parse_line(l));
        }
        m
    }

    #[test]
    fn resume_restores_the_modes_around_the_template() {
        let template = vec!["G1 X1 Y2 F6000".to_string(), "G1 Z3 F600".to_string()];
        // An absolute file; the user left the printer relative.
        let none = ResumeExtras::default;
        let out = resume_script(
            template.clone(),
            &modes(&["G90", "M82"]),
            &modes(&["G91"]),
            none(),
        );
        assert_eq!(
            out,
            ["G90", "M82", "G1 X1 Y2 F6000", "G1 Z3 F600", "G90", "M82"]
        );
        // Relative extrusion; the user typed G20.
        let out = resume_script(
            template.clone(),
            &modes(&["M83"]),
            &modes(&["M83", "G20"]),
            none(),
        );
        assert_eq!(
            out,
            [
                "G21",
                "G90",
                "M83",
                "G1 X1 Y2 F6000",
                "G1 Z3 F600",
                "G90",
                "M83"
            ]
        );
        // A relative inch stream gets both back.
        let stream = modes(&["G20", "G91"]);
        let out = resume_script(template.clone(), &stream, &stream, none());
        assert_eq!(out.first().map(String::as_str), Some("G21"));
        assert_eq!(&out[out.len() - 3..], ["G91", "M83", "G20"]);
        // Heating while parked, before the template; the fan after it.
        let extras = ResumeExtras {
            reheat: vec!["M109 S200".to_string()],
            fan: Some("M106 S255".to_string()),
        };
        let out = resume_script(template, &modes(&["M83"]), &modes(&["M83"]), extras);
        assert_eq!(
            out,
            [
                "G90",
                "M83",
                "M109 S200",
                "G1 X1 Y2 F6000",
                "G1 Z3 F600",
                "M106 S255",
                "G90",
                "M83"
            ]
        );
    }

    #[test]
    fn extruder_only_moves_and_hot_hotends() {
        let script = vec![
            "G91".to_string(),
            "G1 E-2 F2700".to_string(),
            "G1 Z10 E-1".to_string(),
            "G0 E1".to_string(),
            "G90".to_string(),
        ];
        assert_eq!(without_extruder_moves(script), ["G91", "G1 Z10 E-1", "G90"]);
        let temps = TempReport {
            hotends: vec![
                reading(150.0, 200.0),
                reading(219.0, 220.0),
                reading(30.0, 0.0),
            ],
            ..TempReport::default()
        };
        assert!(!hotend_hot(&temps, 0));
        assert!(hotend_hot(&temps, 1));
        assert!(hotend_hot(&temps, 2), "not heated at all");
        assert!(hotend_hot(&temps, 7), "unknown: assume it is fine");
    }

    fn reading(current: f32, target: f32) -> HeaterReading {
        HeaterReading {
            current,
            target: Some(target),
            power: None,
        }
    }

    #[test]
    fn heaters_the_job_used_come_back_when_they_changed_or_cooled() {
        let job = modes(&["M104 S210", "M140 S60", "M106 S128"]);
        // Unchanged and hot: nothing to do.
        let temps = TempReport {
            hotends: vec![reading(209.0, 210.0)],
            bed: Some(reading(60.0, 60.0)),
            ..TempReport::default()
        };
        assert_eq!(
            ResumeExtras::needed(&job, &temps, Some(128)),
            ResumeExtras::default()
        );
        // Hotend switched off, bed cooled down, fan changed.
        let temps = TempReport {
            hotends: vec![reading(150.0, 0.0)],
            bed: Some(reading(40.0, 60.0)),
            ..TempReport::default()
        };
        let extras = ResumeExtras::needed(&job, &temps, Some(0));
        assert_eq!(
            extras.reheat,
            ["M140 S60", "M104 S210", "M190 S60", "M109 S210"]
        );
        assert_eq!(extras.fan.as_deref(), Some("M106 S128"));
        // Two hotends: each by number; unknown readings count as cold.
        let job = modes(&["M104 T0 S200", "M104 T1 S220", "M141 S40", "T1"]);
        let extras = ResumeExtras::needed(&job, &TempReport::default(), None);
        assert_eq!(
            extras.reheat,
            [
                "M141 S40",
                "M104 T0 S200",
                "M104 T1 S220",
                "M191 S40",
                "M109 T0 S200",
                "M109 T1 S220"
            ]
        );
        assert_eq!(extras.fan.as_deref(), Some("M107"));
    }

    #[test]
    fn pause_and_cancel_scripts_start_in_absolute_millimetres() {
        let script = vec!["G1 Z10".to_string()];
        assert_eq!(
            in_script_modes(script.clone(), &modes(&["G90"])),
            ["G1 Z10"]
        );
        assert_eq!(
            in_script_modes(script, &modes(&["G20", "G91"])),
            ["G21", "G90", "G1 Z10"]
        );
        assert!(in_script_modes(Vec::new(), &modes(&["G91"])).is_empty());
    }

    #[test]
    fn queued_commands_count_for_the_modes() {
        use super::super::meta::{CmdMeta, Source};
        let queued: VecDeque<Meta> = ["G91", "G1 Z5", "G20"]
            .iter()
            .map(|c| CmdMeta::new(*c, Source::User))
            .collect();
        let m = modes_after(&MachineState::new(), &queued);
        assert!(!m.absolute && m.inches);
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
