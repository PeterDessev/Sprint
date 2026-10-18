//! Resume-from-line preamble: the G-code that safely brings a printer back
//! to the state a file had reached at a given line, so streaming can
//! continue there after an interruption.

use serde::{Deserialize, Serialize};

use crate::file::GcodeFile;
use crate::parse::{ParsedLine, parse_into};
use crate::state::MachineState;

/// Smallest lift used regardless of [`ResumeOptions::z_clearance`], mm.
const MIN_CLEARANCE: f64 = 0.5;

/// How [`resume_preamble`] restores the machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResumeOptions {
    /// `G28 X Y` before resuming (default true).
    pub home_xy: bool,
    /// Also home Z (default false: homing Z would crash into the part).
    pub home_z: bool,
    /// Lift above the resume Z before travelling, mm (default 2.0; at least
    /// 0.5 mm is always used so the nozzle never drags across the part).
    pub z_clearance: f32,
    /// `M190`/`M109` with the targets in effect at the line (default true).
    pub wait_for_temps: bool,
    /// Declare the nozzle to be at the resume height with `G92 Z` before
    /// lifting. Use this when the board reset (lost its Z position) while
    /// the nozzle stayed where the print stopped. Default false: trust the
    /// firmware's Z.
    pub restore_z_position: bool,
    /// Feedrate of the XY travel to the resume position, mm/min (3000).
    pub travel_feedrate_mm_min: f32,
    /// Feedrate of Z moves, mm/min (600).
    pub z_feedrate_mm_min: f32,
}

impl Default for ResumeOptions {
    fn default() -> Self {
        ResumeOptions {
            home_xy: true,
            home_z: false,
            z_clearance: 2.0,
            wait_for_temps: true,
            restore_z_position: false,
            travel_feedrate_mm_min: 3000.0,
            z_feedrate_mm_min: 600.0,
        }
    }
}

/// Format a number with at most `decimals` decimals, without trailing
/// zeros (`10`, `0.2`, `-1.25`).
fn num(v: f64, decimals: usize) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let mut s = format!("{v:.decimals$}");
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

/// State at the line plus whether bed leveling had been enabled
/// (G29 or `M420 S1`, until `M420 S0` or a G28, which turns leveling off
/// in Marlin; firmware that restores it after homing does so again after
/// the preamble's own G28).
fn walk(file: &GcodeFile, line: usize) -> (MachineState, bool) {
    let mut state = MachineState::new();
    let mut leveling = false;
    let mut parsed = ParsedLine::default();
    for i in 0..line.min(file.len()) {
        parse_into(file.line(i), &mut parsed);
        if let Some(code) = parsed.code {
            if code.letter == b'G' && code.number == 28 {
                leveling = false;
            } else if code.letter == b'G' && code.number == 29 {
                leveling = true;
            } else if code.is('M', 420)
                && let Some(s) = parsed.get('S')
            {
                leveling = s != 0.0;
            }
        }
        state.apply(&parsed);
    }
    (state, leveling)
}

/// G-code lines to run before streaming from `line` to continue an
/// interrupted print: temps, fan, units/modes, homing, safe travel to the
/// position in effect at `line`, `G92 E` restore, feedrate.
///
/// The sequence is ordered for safety: heaters are set (and awaited) first,
/// the nozzle is lifted by `z_clearance` with a relative move before any XY
/// motion (including XY homing), Z is only homed when `home_z` is set, the
/// head travels at the safe height and only then descends to the resume Z.
/// Afterwards the extruder position (`G92 E`), fan, speed/flow overrides,
/// firmware-retraction state, feedrate and the absolute/relative and unit
/// modes of the file are restored.
pub fn resume_preamble(file: &GcodeFile, line: usize, options: &ResumeOptions) -> Vec<String> {
    let (state, leveling) = walk(file, line);
    let mut out: Vec<String> = Vec::new();
    let clearance = f64::from(options.z_clearance).max(MIN_CLEARANCE);
    let z_feed = num(f64::from(options.z_feedrate_mm_min.max(1.0)), 0);
    let travel_feed = num(f64::from(options.travel_feedrate_mm_min.max(1.0)), 0);

    // Positions below are written in millimetres.
    out.push("G21".into());

    // Heaters: start everything, then wait.
    let tools: Vec<(usize, f32)> = state
        .hotend_targets
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, t)| *t > 0.0)
        .collect();
    let multi_tool = state.hotend_targets.len() > 1 || state.tool != 0;
    let hotend = |code: &str, tool: usize, temp: f32| {
        if multi_tool {
            format!("{code} T{tool} S{}", num(f64::from(temp), 1))
        } else {
            format!("{code} S{}", num(f64::from(temp), 1))
        }
    };
    if state.bed_target > 0.0 {
        out.push(format!("M140 S{}", num(f64::from(state.bed_target), 1)));
    }
    if state.chamber_target > 0.0 {
        out.push(format!("M141 S{}", num(f64::from(state.chamber_target), 1)));
    }
    for &(tool, temp) in &tools {
        out.push(hotend("M104", tool, temp));
    }
    if options.wait_for_temps {
        if state.bed_target > 0.0 {
            out.push(format!("M190 S{}", num(f64::from(state.bed_target), 1)));
        }
        if state.chamber_target > 0.0 {
            out.push(format!("M191 S{}", num(f64::from(state.chamber_target), 1)));
        }
        for &(tool, temp) in &tools {
            out.push(hotend("M109", tool, temp));
        }
    }

    // Lift off the part before any XY motion.
    let z = state.position[2];
    if options.restore_z_position {
        out.push(format!("G92 Z{}", num(z, 3)));
    }
    out.push("G91".into());
    out.push(format!("G1 Z{} F{z_feed}", num(clearance, 3)));
    out.push("G90".into());

    // Homing (never Z unless asked).
    match (options.home_xy, options.home_z) {
        (true, true) => out.push("G28".into()),
        (true, false) => out.push("G28 X Y".into()),
        (false, true) => out.push("G28 Z".into()),
        (false, false) => {}
    }
    if leveling && (options.home_xy || options.home_z) {
        // Marlin disables bed leveling when homing.
        out.push("M420 S1".into());
    }
    if multi_tool {
        out.push(format!("T{}", state.tool));
    }

    // Travel at a safe height, then descend. After homing an axis its G92
    // shift is gone, so travel in machine coordinates and re-apply the
    // logical position.
    let machine = state.machine_position();
    let homed = [options.home_xy, options.home_xy, options.home_z];
    let target = |axis: usize| {
        if homed[axis] {
            machine[axis]
        } else {
            state.position[axis]
        }
    };
    let (x, y, tz) = (target(0), target(1), target(2));
    out.push(format!("G1 Z{} F{z_feed}", num(tz + clearance, 3)));
    out.push(format!("G1 X{} Y{} F{travel_feed}", num(x, 3), num(y, 3)));
    if z > 0.0 {
        out.push(format!("G1 Z{} F{z_feed}", num(tz, 3)));
    }
    let mut shift = Vec::new();
    for (axis, letter) in ['X', 'Y', 'Z'].into_iter().enumerate() {
        if homed[axis] && state.origin_offset[axis].abs() > 1e-9 {
            shift.push(format!("{letter}{}", num(state.position[axis], 3)));
        }
    }
    if !shift.is_empty() {
        out.push(format!("G92 {}", shift.join(" ")));
    }

    // Extruder, fan, overrides, retraction state, feedrate.
    out.push(format!("G92 E{}", num(state.position[3], 5)));
    if state.fan > 0 {
        out.push(format!("M106 S{}", state.fan));
    } else {
        out.push("M107".into());
    }
    out.push(format!("M220 S{}", num(f64::from(state.feedrate_pct), 1)));
    out.push(format!("M221 S{}", num(f64::from(state.flow_pct), 1)));
    if state.firmware_retracted {
        out.push("G10".into());
    }
    out.push(format!("G1 F{}", num(state.feedrate_mm_min, 1)));

    // Modes. G90/G91 reset the extruder mode on some firmwares (Marlin 2),
    // so the order reproduces the file's state everywhere.
    let e_mode = if state.relative_e_mode { "M83" } else { "M82" };
    if state.absolute {
        out.push("G90".into());
        out.push(e_mode.into());
    } else {
        out.push(e_mode.into());
        out.push("G91".into());
    }
    if state.inches {
        out.push("G20".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_line;

    fn file(text: &str) -> GcodeFile {
        GcodeFile::from_text("t", text)
    }

    const PRINT: &str = "M140 S60\nM104 S215\nM190 S60\nM109 S215\nG28\nG29\nG90\nM83\nG1 Z0.2 F720\nG1 X10 Y10 E1 F1200\nM106 S200\nG1 Z0.4\nG1 X50 Y40 E2 F1800\nG92 E0\nG1 X60 Y40 E0.7\nG1 X70 Y40 E0.7\n";

    /// Index of the first command moving X or Y (including XY homing).
    fn first_xy_motion(lines: &[String]) -> Option<usize> {
        lines.iter().position(|l| {
            let p = parse_line(l);
            let motion = p.is('G', 0) || p.is('G', 1) || p.is('G', 28);
            let homes_all = p.is('G', 28) && p.words.is_empty();
            motion && (p.has('X') || p.has('Y') || homes_all)
        })
    }

    #[test]
    fn number_formatting() {
        assert_eq!(num(10.0, 3), "10");
        assert_eq!(num(0.2, 3), "0.2");
        assert_eq!(num(-1.25, 3), "-1.25");
        assert_eq!(num(-0.0001, 3), "0");
        assert_eq!(num(f64::NAN, 3), "0");
        assert_eq!(num(1.123456, 5), "1.12346");
    }

    #[test]
    fn full_preamble() {
        let f = file(PRINT);
        // Resume at "G1 X70 Y40 E0.7" (line 15).
        let lines = resume_preamble(&f, 15, &ResumeOptions::default());
        let expected = [
            "G21",
            "M140 S60",
            "M104 S215",
            "M190 S60",
            "M109 S215",
            "G91",
            "G1 Z2 F600",
            "G90",
            "G28 X Y",
            "M420 S1",
            "G1 Z2.4 F600",
            "G1 X60 Y40 F3000",
            "G1 Z0.4 F600",
            "G92 E0.7",
            "M106 S200",
            "M220 S100",
            "M221 S100",
            "G1 F1800",
            "G90",
            "M83",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn safety_properties() {
        let f = file(PRINT);
        for line in 0..=f.len() {
            for home_z in [false, true] {
                let options = ResumeOptions {
                    home_z,
                    ..ResumeOptions::default()
                };
                let lines = resume_preamble(&f, line, &options);
                let first_xy = first_xy_motion(&lines).unwrap_or(lines.len());
                let lift = lines
                    .iter()
                    .position(|l| l.starts_with("G1 Z"))
                    .unwrap_or(usize::MAX);
                assert!(lift < first_xy, "line {line}: {lines:?}");
                // The lift is relative.
                assert_eq!(lines.get(lift - 1).map(String::as_str), Some("G91"));
                let homes_z = lines.iter().any(|l| l == "G28" || l.starts_with("G28 Z"));
                assert_eq!(homes_z, home_z, "{lines:?}");
                assert!(lines.iter().all(|l| crate::parse::clean_line(l).is_some()));
            }
        }
    }

    #[test]
    fn resume_at_start_does_not_descend() {
        let lines = resume_preamble(&file(PRINT), 0, &ResumeOptions::default());
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("M104") || l.starts_with("M109"))
        );
        let z_moves: Vec<_> = lines.iter().filter(|l| l.starts_with("G1 Z")).collect();
        assert_eq!(z_moves, ["G1 Z2 F600", "G1 Z2 F600"]);
        assert_eq!(lines.last().map(String::as_str), Some("M82"));
    }

    #[test]
    fn no_wait_no_home() {
        let options = ResumeOptions {
            home_xy: false,
            wait_for_temps: false,
            restore_z_position: true,
            z_clearance: 5.0,
            ..ResumeOptions::default()
        };
        let lines = resume_preamble(&file(PRINT), 15, &options);
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("M190") || l.starts_with("M109"))
        );
        assert!(!lines.iter().any(|l| l.starts_with("G28")));
        assert!(!lines.iter().any(|l| l == "M420 S1"));
        assert!(lines.contains(&"G92 Z0.4".to_string()));
        assert!(lines.contains(&"G1 Z5 F600".to_string()));
        assert!(lines.contains(&"G1 Z5.4 F600".to_string()));
    }

    #[test]
    fn restores_relative_mode_inches_and_retraction() {
        let f = file("M82\nG1 X5 Y5 Z1 E3\nG10\nG20\nG91\nM107\nM220 S80\nM221 S90\nG1 X0.1\n");
        let lines = resume_preamble(&f, f.len(), &ResumeOptions::default());
        let tail: Vec<&str> = lines
            .iter()
            .rev()
            .take(5)
            .rev()
            .map(String::as_str)
            .collect();
        // G1 F is in mm/min before G20 switches units back.
        assert_eq!(tail, ["G10", "G1 F1500", "M82", "G91", "G20"]);
        assert!(lines.contains(&"M220 S80".to_string()));
        assert!(lines.contains(&"M221 S90".to_string()));
        assert!(lines.contains(&"M107".to_string()));
        // Positions converted to mm: X 5 + 0.1 in = 7.54 mm.
        assert!(
            lines.iter().any(|l| l.starts_with("G1 X7.54 Y5 ")),
            "{lines:?}"
        );
    }

    #[test]
    fn multiple_tools_and_chamber() {
        let f = file("M141 S40\nM104 T0 S200\nM104 T1 S220\nT1\nG1 Z0.3\nG1 X1 Y1 E1\n");
        let lines = resume_preamble(&f, f.len(), &ResumeOptions::default());
        for expected in [
            "M141 S40",
            "M191 S40",
            "M104 T0 S200",
            "M104 T1 S220",
            "M109 T0 S200",
            "M109 T1 S220",
            "T1",
        ] {
            assert!(
                lines.contains(&expected.to_string()),
                "{expected}: {lines:?}"
            );
        }
        let tool = lines.iter().position(|l| l == "T1").unwrap_or(0);
        let travel = lines
            .iter()
            .position(|l| l.starts_with("G1 X"))
            .unwrap_or(0);
        assert!(tool < travel);
    }

    #[test]
    fn g92_shift_is_reapplied_after_homing() {
        let f = file("G28\nG1 X10 Y20 Z1\nG92 X0 Y0\nG1 X5 Y5\n");
        let lines = resume_preamble(&f, f.len(), &ResumeOptions::default());
        assert!(lines.contains(&"G1 X15 Y25 F3000".to_string()), "{lines:?}");
        assert!(lines.contains(&"G92 X5 Y5".to_string()));
    }

    #[test]
    fn leveling_turned_off_is_not_restored() {
        let f = file("G28\nM420 S1\nM420 S0\nG1 Z1\nG1 X1 Y1\n");
        let lines = resume_preamble(&f, f.len(), &ResumeOptions::default());
        assert!(!lines.contains(&"M420 S1".to_string()));
        let f = file("G28\nM420 S1 Z10\nG1 Z1\n");
        let lines = resume_preamble(&f, f.len(), &ResumeOptions::default());
        assert!(lines.contains(&"M420 S1".to_string()));
        // Marlin's G28 turns leveling off: enabling it before homing means
        // the print ran without it, so the preamble must not turn it on.
        let f = file("M420 S1\nG28\nG1 Z1\nG1 X1 Y1\n");
        let lines = resume_preamble(&f, f.len(), &ResumeOptions::default());
        assert!(!lines.contains(&"M420 S1".to_string()), "{lines:?}");
        let f = file("G28\nG29\nG28 X\nG1 Z1\n");
        let lines = resume_preamble(&f, f.len(), &ResumeOptions::default());
        assert!(!lines.contains(&"M420 S1".to_string()), "{lines:?}");
    }

    #[test]
    fn out_of_range_and_empty() {
        let lines = resume_preamble(&file(""), 10, &ResumeOptions::default());
        assert!(lines.contains(&"G28 X Y".to_string()));
        let options = ResumeOptions {
            z_clearance: -3.0,
            travel_feedrate_mm_min: f32::NAN,
            ..ResumeOptions::default()
        };
        let lines = resume_preamble(&file(PRINT), 99, &options);
        assert!(!lines.iter().any(|l| l.contains("NaN") || l.contains("inf")));
        // A minimum lift is always applied.
        assert!(lines.contains(&"G1 Z0.5 F600".to_string()), "{lines:?}");
    }
}
