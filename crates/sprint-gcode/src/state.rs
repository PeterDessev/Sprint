//! Machine-state tracking: positions, modes, temperatures, fan and overrides
//! while walking a file line by line.

use crate::file::GcodeFile;
use crate::parse::{ParsedLine, parse_into};

/// Positions are clamped to this magnitude (mm) so arithmetic on garbage
/// input stays finite.
const POSITION_LIMIT: f64 = 1e9;
/// Millimetres per inch (G20).
const MM_PER_INCH: f64 = 25.4;

/// Interpreter state while walking a file: positions and modes.
#[derive(Clone, Debug, PartialEq)]
pub struct MachineState {
    /// Logical X Y Z E (mm) after G92 offsets.
    pub position: [f64; 4],
    /// G90 (true) / G91 (false).
    pub absolute: bool,
    /// Effective extruder mode: false after M83 or G91 (G91 also makes E
    /// relative). See [`MachineState::relative_e_mode`].
    pub absolute_e: bool,
    /// G20 (true) / G21 (false).
    pub inches: bool,
    /// Last F, converted to mm/min.
    pub feedrate_mm_min: f64,
    /// Active tool (T<n>).
    pub tool: u8,
    /// Hotend targets per tool from M104/M109 (and RepRapFirmware `G10 P S`).
    pub hotend_targets: Vec<f32>,
    /// M140/M190 target.
    pub bed_target: f32,
    /// M141/M191 target.
    pub chamber_target: f32,
    /// Part-cooling fan from M106/M107 (0..=255).
    pub fan: u8,
    /// M220 speed factor in percent.
    pub feedrate_pct: f32,
    /// M221 flow factor in percent.
    pub flow_pct: f32,
    /// G10 (true) / G11 (false) firmware retraction state.
    pub firmware_retracted: bool,
    /// M83 requested (M82 clears it). Unlike `absolute_e` this is not
    /// changed by G90/G91: after `M83`, `G91`, `G90` the extruder is
    /// relative again, as in Klipper, RepRapFirmware and PrusaSlicer.
    pub relative_e_mode: bool,
    /// G92 shift of X Y Z: machine coordinate = `position + origin_offset`.
    /// G28 clears it for the homed axes.
    pub origin_offset: [f64; 3],
}

impl Default for MachineState {
    fn default() -> Self {
        MachineState {
            position: [0.0; 4],
            absolute: true,
            absolute_e: true,
            inches: false,
            // Marlin's power-on feedrate.
            feedrate_mm_min: 1500.0,
            tool: 0,
            hotend_targets: vec![0.0],
            bed_target: 0.0,
            chamber_target: 0.0,
            fan: 0,
            feedrate_pct: 100.0,
            flow_pct: 100.0,
            firmware_retracted: false,
            relative_e_mode: false,
            origin_offset: [0.0; 3],
        }
    }
}

/// A motion command after modes and units were applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Move {
    /// Logical X Y Z E before the move (mm).
    pub from: [f64; 4],
    /// Logical X Y Z E after the move (mm). For arcs this is the endpoint.
    pub to: [f64; 4],
    /// Feedrate in effect (mm/min).
    pub feedrate_mm_min: f64,
    /// Which command produced it.
    pub kind: MoveCommand,
}

/// The command behind a [`Move`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveCommand {
    /// G0.
    Rapid,
    /// G1.
    Linear,
    /// G2.
    ArcCw,
    /// G3.
    ArcCcw,
    /// G28 (the path is unknown; `to` is the homed position).
    Home,
}

impl MachineState {
    /// Power-on state (absolute mode, mm, tool 0, everything off).
    pub fn new() -> MachineState {
        MachineState::default()
    }

    /// Machine X Y Z (logical position plus the G92 offset).
    pub fn machine_position(&self) -> [f64; 3] {
        [
            self.position[0] + self.origin_offset[0],
            self.position[1] + self.origin_offset[1],
            self.position[2] + self.origin_offset[2],
        ]
    }

    /// Hotend target of `tool` (0 when never set).
    pub fn hotend_target(&self, tool: u8) -> f32 {
        self.hotend_targets
            .get(usize::from(tool))
            .copied()
            .unwrap_or(0.0)
    }

    fn units(&self) -> f64 {
        if self.inches { MM_PER_INCH } else { 1.0 }
    }

    fn set_hotend(&mut self, tool: u8, celsius: f64) {
        let t = usize::from(tool);
        if self.hotend_targets.len() <= t {
            self.hotend_targets.resize(t + 1, 0.0);
        }
        if let Some(slot) = self.hotend_targets.get_mut(t) {
            *slot = clamp_temp(celsius);
        }
    }

    /// Apply one parsed line, returning the resulting move if it was one.
    ///
    /// Handles G0/G1/G2/G3/G28/G90/G91/G92/G20/G21, M82/M83, T<n>,
    /// M104/M109/M140/M190/M141/M191, M106/M107, M220/M221 and G10/G11
    /// (firmware retraction, or RepRapFirmware tool temperatures when `G10`
    /// carries `P`). Everything else leaves the state unchanged.
    pub fn apply(&mut self, line: &ParsedLine) -> Option<Move> {
        let code = line.code?;
        match code.letter {
            b'G' if code.sub.is_none() => match code.number {
                0 => self.motion(line, MoveCommand::Rapid),
                1 => self.motion(line, MoveCommand::Linear),
                2 => self.motion(line, MoveCommand::ArcCw),
                3 => self.motion(line, MoveCommand::ArcCcw),
                10 => {
                    if line.has('P') || line.has('L') {
                        // RepRapFirmware: G10 P<tool> S<active> R<standby>
                        // (or a workspace/tool offset with L).
                        if let (Some(tool), Some(temp)) = (
                            line.get('P').and_then(as_u8),
                            line.get('S').or_else(|| line.get('R')),
                        ) {
                            self.set_hotend(tool, temp);
                        }
                    } else {
                        self.firmware_retracted = true;
                    }
                    None
                }
                11 => {
                    self.firmware_retracted = false;
                    None
                }
                20 => {
                    self.inches = true;
                    None
                }
                21 => {
                    self.inches = false;
                    None
                }
                28 => Some(self.home(line)),
                90 => {
                    self.absolute = true;
                    self.absolute_e = !self.relative_e_mode;
                    None
                }
                91 => {
                    self.absolute = false;
                    self.absolute_e = false;
                    None
                }
                92 => {
                    self.set_position(line);
                    None
                }
                _ => None,
            },
            b'G' => {
                if code.number == 92 && code.sub == Some(1) {
                    // G92.1: drop the G92 shift, keeping the machine position.
                    for axis in 0..3 {
                        self.position[axis] += self.origin_offset[axis];
                        self.origin_offset[axis] = 0.0;
                    }
                }
                None
            }
            b'M' if code.sub.is_none() => {
                match code.number {
                    82 => {
                        self.relative_e_mode = false;
                        self.absolute_e = self.absolute;
                    }
                    83 => {
                        self.relative_e_mode = true;
                        self.absolute_e = false;
                    }
                    104 | 109 => {
                        let tool = match line.get('T') {
                            Some(t) => as_u8(t),
                            None => Some(self.tool),
                        };
                        let temp = line.get('S').or_else(|| {
                            if code.number == 109 {
                                line.get('R')
                            } else {
                                None
                            }
                        });
                        if let (Some(tool), Some(temp)) = (tool, temp) {
                            self.set_hotend(tool, temp);
                        }
                    }
                    140 | 190 => {
                        if let Some(t) = line.get('S').or_else(|| line.get('R')) {
                            self.bed_target = clamp_temp(t);
                        }
                    }
                    141 | 191 => {
                        if let Some(t) = line.get('S').or_else(|| line.get('R')) {
                            self.chamber_target = clamp_temp(t);
                        }
                    }
                    106 => {
                        if is_part_fan(line) {
                            self.fan = fan_value(line.get('S'));
                        }
                    }
                    107 => {
                        if is_part_fan(line) {
                            self.fan = 0;
                        }
                    }
                    220 => {
                        if let Some(s) = line.get('S').filter(|&s| s > 0.0) {
                            self.feedrate_pct = s.min(100_000.0) as f32;
                        }
                    }
                    221 => {
                        if let Some(s) = line.get('S').filter(|&s| s >= 0.0) {
                            self.flow_pct = s.min(100_000.0) as f32;
                        }
                    }
                    _ => {}
                }
                None
            }
            b'T' if code.sub.is_none() => {
                if let Ok(tool) = u8::try_from(code.number) {
                    self.tool = tool;
                    let t = usize::from(tool);
                    if self.hotend_targets.len() <= t {
                        self.hotend_targets.resize(t + 1, 0.0);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// G0/G1/G2/G3: update F and the endpoint.
    fn motion(&mut self, line: &ParsedLine, kind: MoveCommand) -> Option<Move> {
        let units = self.units();
        let from = self.position;
        let mut any = false;
        let is_arc = matches!(kind, MoveCommand::ArcCw | MoveCommand::ArcCcw);
        for w in &line.words {
            let axis = match w.letter {
                b'X' => 0,
                b'Y' => 1,
                b'Z' => 2,
                b'E' => 3,
                b'F' => {
                    let f = w.value * units;
                    if f > 0.0 && f.is_finite() {
                        self.feedrate_mm_min = f.min(1e9);
                    }
                    continue;
                }
                b'I' | b'J' | b'R' if is_arc => {
                    any |= !w.value.is_nan();
                    continue;
                }
                _ => continue,
            };
            if w.value.is_nan() {
                continue;
            }
            any = true;
            let v = w.value * units;
            let relative = if axis == 3 {
                !self.absolute_e
            } else {
                !self.absolute
            };
            let target = if relative { self.position[axis] + v } else { v };
            if target.is_finite() {
                self.position[axis] = target.clamp(-POSITION_LIMIT, POSITION_LIMIT);
            }
        }
        any.then_some(Move {
            from,
            to: self.position,
            feedrate_mm_min: self.feedrate_mm_min,
            kind,
        })
    }

    /// G28: home the listed axes (all when none is listed) to 0.
    fn home(&mut self, line: &ParsedLine) -> Move {
        let listed = [line.has('X'), line.has('Y'), line.has('Z')];
        let all = !listed.iter().any(|&l| l);
        let from = self.position;
        for (axis, &l) in listed.iter().enumerate() {
            if all || l {
                self.position[axis] = 0.0;
                self.origin_offset[axis] = 0.0;
            }
        }
        Move {
            from,
            to: self.position,
            feedrate_mm_min: self.feedrate_mm_min,
            kind: MoveCommand::Home,
        }
    }

    /// G92: set the logical position without moving. No arguments zeroes
    /// every axis.
    fn set_position(&mut self, line: &ParsedLine) {
        let units = self.units();
        if line.words.is_empty() {
            for axis in 0..4 {
                self.set_axis(axis, 0.0);
            }
            return;
        }
        for (axis, letter) in ['X', 'Y', 'Z', 'E'].into_iter().enumerate() {
            if let Some(v) = line.get(letter) {
                let v = (v * units).clamp(-POSITION_LIMIT, POSITION_LIMIT);
                self.set_axis(axis, v);
            }
        }
    }

    fn set_axis(&mut self, axis: usize, value: f64) {
        if axis < 3 {
            let machine = self.position[axis] + self.origin_offset[axis];
            self.position[axis] = value;
            self.origin_offset[axis] =
                (machine - value).clamp(-2.0 * POSITION_LIMIT, 2.0 * POSITION_LIMIT);
        } else if let Some(e) = self.position.get_mut(axis) {
            *e = value;
        }
    }
}

fn as_u8(v: f64) -> Option<u8> {
    if (0.0..=255.0).contains(&v) {
        Some(v as u8)
    } else {
        None
    }
}

fn clamp_temp(t: f64) -> f32 {
    if t.is_finite() {
        t.clamp(0.0, 2000.0) as f32
    } else {
        0.0
    }
}

/// M106/M107 without `P`, or with `P0`, address the part-cooling fan.
fn is_part_fan(line: &ParsedLine) -> bool {
    match line.get('P') {
        Some(p) => p == 0.0,
        None => !line.has('P'),
    }
}

/// M106 `S`: 0..=255 (Marlin). A fractional value up to 1 is a
/// RepRapFirmware-style fraction. Missing `S` means full speed.
fn fan_value(s: Option<f64>) -> u8 {
    let Some(s) = s else {
        return 255;
    };
    let raw = if s > 0.0 && s < 1.0 { s * 255.0 } else { s };
    raw.round().clamp(0.0, 255.0) as u8
}

/// Machine state after executing lines `0..line` (exclusive).
pub fn state_at_line(file: &GcodeFile, line: usize) -> MachineState {
    let mut state = MachineState::new();
    let mut parsed = ParsedLine::default();
    for i in 0..line.min(file.len()) {
        parse_into(file.line(i), &mut parsed);
        state.apply(&parsed);
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_line;

    fn run(lines: &[&str]) -> MachineState {
        let mut s = MachineState::new();
        for l in lines {
            s.apply(&parse_line(l));
        }
        s
    }

    #[test]
    fn absolute_and_relative_moves() {
        let mut s = MachineState::new();
        let m = s.apply(&parse_line("G1 X10 Y20 F3000"));
        assert_eq!(
            m,
            Some(Move {
                from: [0.0; 4],
                to: [10.0, 20.0, 0.0, 0.0],
                feedrate_mm_min: 3000.0,
                kind: MoveCommand::Linear
            })
        );
        s.apply(&parse_line("G91"));
        let m = s.apply(&parse_line("G0 X1 Y-1 E2"));
        assert_eq!(m.map(|m| m.to), Some([11.0, 19.0, 0.0, 2.0]));
        assert_eq!(m.map(|m| m.kind), Some(MoveCommand::Rapid));
        assert!(!s.absolute_e);
        s.apply(&parse_line("G90"));
        assert!(s.absolute && s.absolute_e);
        assert_eq!(s.apply(&parse_line("G1 F1200")), None);
        assert_eq!(s.feedrate_mm_min, 1200.0);
        assert_eq!(s.apply(&parse_line("; nothing")), None);
    }

    #[test]
    fn extruder_modes() {
        let s = run(&["M83", "G1 E1", "G1 E1"]);
        assert_eq!(s.position[3], 2.0);
        assert!(!s.absolute_e && s.absolute);
        // G91 makes E relative too; G90 restores the M83/M82 choice.
        let s = run(&["M82", "G91", "G1 E1", "G1 E1", "G90", "G1 E5"]);
        assert_eq!(s.position[3], 5.0);
        let s = run(&["M83", "G91", "G90", "G1 E1", "G1 E1"]);
        assert_eq!(s.position[3], 2.0);
        assert!(!s.absolute_e);
        let s = run(&["G91", "M82"]);
        assert!(!s.absolute_e, "G91 keeps E relative");
    }

    #[test]
    fn g92_offsets() {
        let s = run(&["G1 X10 Y10 Z1 E5", "G92 E0"]);
        assert_eq!(s.position, [10.0, 10.0, 1.0, 0.0]);
        let s = run(&["G1 X10", "G92 X0", "G1 X5"]);
        assert_eq!(s.position[0], 5.0);
        assert_eq!(s.machine_position()[0], 15.0);
        let s = run(&["G1 X10 Y5 Z2 E3", "G92"]);
        assert_eq!(s.position, [0.0; 4]);
        assert_eq!(s.machine_position(), [10.0, 5.0, 2.0]);
        let s = run(&["G1 X10", "G92 X0", "G92.1"]);
        assert_eq!(s.position[0], 10.0);
        assert_eq!(s.origin_offset[0], 0.0);
    }

    #[test]
    fn homing() {
        let mut s = run(&["G1 X10 Y20 Z5 E3"]);
        let m = s.apply(&parse_line("G28 X"));
        assert_eq!(m.map(|m| m.kind), Some(MoveCommand::Home));
        assert_eq!(s.position, [0.0, 20.0, 5.0, 3.0]);
        s.apply(&parse_line("G28"));
        assert_eq!(s.position, [0.0, 0.0, 0.0, 3.0]);
        let s = run(&["G1 X10 Y20 Z5", "G92 Z0", "G28 Z"]);
        assert_eq!(s.machine_position(), [10.0, 20.0, 0.0]);
        // Prusa's "G28 W" homes everything.
        let s = run(&["G1 X10 Y20 Z5", "G28 W"]);
        assert_eq!(s.position[..3], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn inches() {
        let s = run(&["G20", "G1 X1 F10"]);
        assert!((s.position[0] - 25.4).abs() < 1e-9);
        assert!((s.feedrate_mm_min - 254.0).abs() < 1e-9);
        let s = run(&["G20", "G21", "G1 X1"]);
        assert_eq!(s.position[0], 1.0);
    }

    #[test]
    fn temperatures() {
        let s = run(&["M104 S200", "M140 S60", "M141 S40"]);
        assert_eq!(s.hotend_targets, vec![200.0]);
        assert_eq!(s.bed_target, 60.0);
        assert_eq!(s.chamber_target, 40.0);
        let s = run(&["M104 T1 S210", "M109 R180", "M190 R50", "M191 S35"]);
        assert_eq!(s.hotend_targets, vec![180.0, 210.0]);
        assert_eq!(s.bed_target, 50.0);
        assert_eq!(s.chamber_target, 35.0);
        let s = run(&["T1", "M109 S230"]);
        assert_eq!(s.tool, 1);
        assert_eq!(s.hotend_target(1), 230.0);
        assert_eq!(s.hotend_target(7), 0.0);
        let s = run(&["M104 T300 S200", "M104 S99999"]);
        assert_eq!(s.hotend_targets, vec![2000.0]);
        // RepRapFirmware tool temperatures.
        let s = run(&["G10 P1 S215 R150"]);
        assert_eq!(s.hotend_target(1), 215.0);
        assert!(!s.firmware_retracted);
    }

    #[test]
    fn fan_and_overrides() {
        assert_eq!(run(&["M106 S128"]).fan, 128);
        assert_eq!(run(&["M106"]).fan, 255);
        assert_eq!(run(&["M106 S0.5"]).fan, 128);
        assert_eq!(run(&["M106 S999"]).fan, 255);
        assert_eq!(run(&["M106 S255", "M107"]).fan, 0);
        assert_eq!(run(&["M106 S255", "M107 P1"]).fan, 255);
        assert_eq!(run(&["M106 P1 S100"]).fan, 0);
        let s = run(&["M220 S150", "M221 S95"]);
        assert_eq!((s.feedrate_pct, s.flow_pct), (150.0, 95.0));
        let s = run(&["M220 S0", "M220 S-5"]);
        assert_eq!(s.feedrate_pct, 100.0);
    }

    #[test]
    fn firmware_retraction_and_tools() {
        let s = run(&["G10"]);
        assert!(s.firmware_retracted);
        let s = run(&["G10", "G11"]);
        assert!(!s.firmware_retracted);
        let s = run(&["T3"]);
        assert_eq!(s.tool, 3);
        assert_eq!(s.hotend_targets.len(), 4);
        let s = run(&["T999"]);
        assert_eq!(s.tool, 0);
    }

    #[test]
    fn arcs_return_endpoints() {
        let mut s = run(&["G1 X10 Y10"]);
        let m = s.apply(&parse_line("G2 X20 Y10 I5 J0 E1"));
        assert_eq!(m.map(|m| m.kind), Some(MoveCommand::ArcCw));
        assert_eq!(m.map(|m| m.to), Some([20.0, 10.0, 0.0, 1.0]));
        // Full circle in place.
        let m = s.apply(&parse_line("G3 I-5"));
        assert_eq!(
            m.map(|m| (m.from, m.to)),
            Some(([20.0, 10.0, 0.0, 1.0], [20.0, 10.0, 0.0, 1.0]))
        );
        assert_eq!(s.apply(&parse_line("G2")), None);
    }

    #[test]
    fn garbage_values_stay_finite() {
        let s = run(&[
            "G91",
            "G1 X99999999999999",
            "G1 X99999999999999",
            "G1 X-1e5",
        ]);
        assert!(s.position[0].is_finite());
        assert!(s.position[0] <= POSITION_LIMIT);
        let s = run(&["G1 X", "G1 F0", "G1 F-5"]);
        assert_eq!(s.position[0], 0.0);
        assert_eq!(s.feedrate_mm_min, 1500.0);
    }

    #[test]
    fn state_at_line_walks_prefix() {
        let f = GcodeFile::from_text("t", "M104 S200\nG1 X5\nG1 X6\nM106 S255\n");
        assert_eq!(state_at_line(&f, 0), MachineState::new());
        let s = state_at_line(&f, 2);
        assert_eq!(s.position[0], 5.0);
        assert_eq!(s.hotend_targets, vec![200.0]);
        assert_eq!(s.fan, 0);
        assert_eq!(state_at_line(&f, 100).fan, 255);
    }
}
