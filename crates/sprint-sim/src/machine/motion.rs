//! Motion: positions and coordinate modes, linear and arc moves, homing,
//! probing and leveling, and the motion-related reports.

use super::{Machine, Outcome, Segment, TaskKind};
use crate::format::f2;
use crate::mesh::Mesh;
use crate::parse::Command;
use crate::planner::MoveKind;
use std::collections::VecDeque;

/// Marlin EXTRUDE_MAXLENGTH.
const EXTRUDE_MAXLENGTH: f64 = 200.0;
/// Marlin MAX_ARC_SEGMENT_MM.
const ARC_SEGMENT_MM: f64 = 1.0;
/// Marlin MIN_CIRCLE_SEGMENTS (segments in a full circle at least).
const MIN_CIRCLE_SEGMENTS: f64 = 72.0;
/// Homing feedrates (HOMING_FEEDRATE_MM_M / 60).
const HOMING_FEED_XY: f64 = 50.0;
const HOMING_FEED_Z: f64 = 4.0;
/// XY_PROBE_FEEDRATE.
const PROBE_TRAVEL_FEED: f64 = 133.0;
/// Z_CLEARANCE_BETWEEN_PROBES.
const PROBE_CLEARANCE: f64 = 5.0;
/// Seconds per probe point besides travel (deploy, fast + slow probe, stow).
const PROBE_POINT_TIME: f64 = 2.4;

/// Positions, coordinate modes and motion overrides.
#[derive(Clone, Debug)]
pub(crate) struct Motion {
    /// Commanded position in machine coordinates (E is logical).
    pub native: [f64; 4],
    /// G92 position shift for X Y Z.
    pub shift: [f64; 3],
    /// G90 (true) / G91.
    pub absolute: bool,
    /// M83 = Some(true), M82 = Some(false); None follows G90/G91.
    pub e_relative: Option<bool>,
    /// G20.
    pub inches: bool,
    /// Current feedrate in mm/s.
    pub feedrate: f64,
    pub homed: [bool; 3],
    pub steppers: bool,
    pub tool: usize,
    pub feedrate_pct: f64,
    pub flow_pct: Vec<f64>,
    pub babystep: [f64; 3],
    pub soft_endstops: bool,
    pub retracted: bool,
}

impl Motion {
    /// Power-on state.
    pub(crate) fn new(extruders: usize) -> Motion {
        Motion {
            native: [0.0; 4],
            shift: [0.0; 3],
            absolute: true,
            e_relative: None,
            inches: false,
            feedrate: 50.0,
            homed: [false; 3],
            steppers: false,
            tool: 0,
            feedrate_pct: 100.0,
            flow_pct: vec![100.0; extruders.max(1)],
            babystep: [0.0; 3],
            soft_endstops: true,
            retracted: false,
        }
    }

    fn e_is_relative(&self) -> bool {
        self.e_relative.unwrap_or(!self.absolute)
    }
}

/// G28 in progress.
#[derive(Clone, Debug)]
pub(crate) struct Homing {
    axes: [bool; 3],
    until: f64,
}

/// G29 in progress.
#[derive(Clone, Debug)]
pub(crate) struct Probing {
    home_first: [bool; 3],
    until: f64,
}

const AXES: [char; 4] = ['X', 'Y', 'Z', 'E'];

impl Machine {
    fn unit_factor(&self) -> f64 {
        if self.motion.inches { 25.4 } else { 1.0 }
    }

    /// Workspace offset of an axis (M206 + G92).
    fn offset(&self, axis: usize) -> f64 {
        if axis < 3 {
            self.settings.home_offset[axis] + self.motion.shift[axis]
        } else {
            0.0
        }
    }

    /// Logical X Y Z E (what M114 reports).
    pub(super) fn logical_position(&self) -> [f64; 4] {
        let mut p = self.motion.native;
        for (i, v) in p.iter_mut().enumerate() {
            *v += self.offset(i);
        }
        p
    }

    /// Leveling correction at a machine position.
    fn leveling_offset(&self, x: f64, y: f64, z: f64) -> f64 {
        if !self.leveling {
            return 0.0;
        }
        let Some(mesh) = &self.mesh else {
            return 0.0;
        };
        let fade = self.settings.fade_height;
        let factor = if fade > 0.0 {
            (1.0 - z / fade).clamp(0.0, 1.0)
        } else {
            1.0
        };
        mesh.z_at(x, y) * factor
    }

    /// Planner coordinates for a machine position with physical E.
    pub(super) fn physical(&self, native: [f64; 4], e_physical: f64) -> [f64; 4] {
        let [x, y, z, _] = native;
        [x, y, z + self.leveling_offset(x, y, z), e_physical]
    }

    /// After a quickstop: take the position from where the tool stopped.
    pub(super) fn sync_native_from_physical(&mut self, physical: [f64; 4]) {
        let [x, y, z, _] = physical;
        let mut zn = z - self.leveling_offset(x, y, z);
        // One refinement step is plenty for the gentle mesh slopes.
        zn = z - self.leveling_offset(x, y, zn);
        self.motion.native[0] = x;
        self.motion.native[1] = y;
        self.motion.native[2] = zn;
    }

    /// The M114 / M154 line.
    pub(super) fn position_line(&self) -> String {
        let p = self.logical_position();
        let t = self.planner.target();
        let s = self.settings.steps_per_mm;
        let count = |i: usize| -> i64 {
            let v = (t[i] * s[i]).round();
            if v.is_finite() {
                v.clamp(-9.0e15, 9.0e15) as i64
            } else {
                0
            }
        };
        format!(
            "X:{} Y:{} Z:{} E:{} Count X:{} Y:{} Z:{}",
            f2(p[0]),
            f2(p[1]),
            f2(p[2]),
            f2(p[3]),
            count(0),
            count(1),
            count(2)
        )
    }

    /// Clamp a machine target to the soft endstops (homed axes only).
    fn clamp_to_soft_endstops(&self, target: &mut [f64; 4]) {
        if !self.motion.soft_endstops {
            return;
        }
        for (i, v) in target.iter_mut().take(3).enumerate() {
            if self.motion.homed[i] {
                *v = v.clamp(0.0, f64::from(self.cfg.build_volume[i]));
            }
        }
    }

    /// E travel actually performed for a commanded E change (cold and long
    /// extrusion prevention, flow).
    fn physical_e_delta(&mut self, de: f64) -> f64 {
        if de == 0.0 || !de.is_finite() {
            return 0.0;
        }
        let tool = self.motion.tool;
        let temp = self.hotends.get(tool).map_or(0.0, |h| h.temp());
        if !self.allow_cold_extrude && temp < self.extrude_min_temp {
            self.emit("echo: cold extrusion prevented");
            return 0.0;
        }
        let flow = self.motion.flow_pct.get(tool).copied().unwrap_or(100.0) / 100.0;
        let delta = de * flow;
        if delta.abs() > EXTRUDE_MAXLENGTH {
            self.emit("echo: too long extrusion prevented");
            return 0.0;
        }
        delta
    }

    /// Queue segments now if possible; otherwise wait for the planner.
    fn queue_segments(&mut self, segments: VecDeque<Segment>) -> Outcome {
        let mut segments = segments;
        let limits = self.settings.limits();
        while let Some(seg) = segments.front().copied() {
            if !self.planner.has_room() {
                break;
            }
            self.planner.push(seg.target, seg.feed, seg.kind, &limits);
            segments.pop_front();
        }
        if segments.is_empty() {
            self.motion.steppers = true;
            Outcome::Done
        } else {
            self.motion.steppers = true;
            Outcome::Task {
                kind: TaskKind::Moves(segments),
                sync: false,
            }
        }
    }

    /// Commanded machine target for G0-G3 parameters.
    fn move_target(&self, c: &Command) -> [f64; 4] {
        let unit = self.unit_factor();
        let mut target = self.motion.native;
        for (i, letter) in AXES.iter().enumerate() {
            if let Some(v) = c.value(*letter) {
                let v = (v * unit).clamp(-1.0e6, 1.0e6);
                let relative = if i == 3 {
                    self.motion.e_is_relative()
                } else {
                    !self.motion.absolute
                };
                target[i] = if relative {
                    target[i] + v
                } else {
                    v - self.offset(i)
                };
            }
        }
        target
    }

    fn update_feedrate(&mut self, c: &Command) {
        if let Some(f) = c.value('F')
            && f > 0.0
        {
            self.motion.feedrate = (f * self.unit_factor() / 60.0).min(100_000.0);
        }
    }

    fn move_kind(from: &[f64; 4], to: &[f64; 4], de: f64) -> MoveKind {
        let xyz = (0..3).any(|i| (to[i] - from[i]).abs() > 1e-9);
        match (xyz, de != 0.0) {
            (true, true) => MoveKind::Print,
            (true, false) => MoveKind::Travel,
            (false, _) => MoveKind::Retract,
        }
    }

    /// G0 / G1.
    pub(super) fn cmd_linear_move(&mut self, c: &Command) -> Outcome {
        let mut target = self.move_target(c);
        self.update_feedrate(c);
        self.clamp_to_soft_endstops(&mut target);
        self.linear_to(
            target,
            self.motion.feedrate * self.motion.feedrate_pct / 100.0,
        )
    }

    /// Move to a machine target (E logical).
    fn linear_to(&mut self, target: [f64; 4], feed: f64) -> Outcome {
        let from = self.motion.native;
        let de = self.physical_e_delta(target[3] - from[3]);
        let e_phys = self.planner.target()[3] + de;
        self.motion.native = target;
        let seg = Segment {
            target: self.physical(target, e_phys),
            feed,
            kind: Machine::move_kind(&from, &target, de),
        };
        self.queue_segments(VecDeque::from([seg]))
    }

    /// G2 / G3 (XY plane; I/J centre or R radius; P full turns).
    pub(super) fn cmd_arc(&mut self, c: &Command) -> Outcome {
        let clockwise = c.code == 2;
        let unit = self.unit_factor();
        let mut target = self.move_target(c);
        self.update_feedrate(c);
        self.clamp_to_soft_endstops(&mut target);
        let start = self.motion.native;
        let (mut ox, mut oy) = (
            c.value('I').unwrap_or(0.0) * unit,
            c.value('J').unwrap_or(0.0) * unit,
        );
        if let Some(r) = c.value('R') {
            let r = r * unit;
            let (dx, dy) = ((target[0] - start[0]) * 0.5, (target[1] - start[1]) * 0.5);
            let len = (dx * dx + dy * dy).sqrt();
            ox = 0.0;
            oy = 0.0;
            if r != 0.0 && len > 0.0 {
                let e = if clockwise ^ (r < 0.0) { -1.0 } else { 1.0 };
                let h2 = (r - len) * (r + len);
                let h = if h2 >= 0.0 { h2.sqrt() } else { 0.0 };
                let (sx, sy) = (-dy, dx);
                ox = dx + sx / len * e * h;
                oy = dy + sy / len * e * h;
            }
        }
        let radius = (ox * ox + oy * oy).sqrt();
        if !(radius > 1e-6 && radius.is_finite()) {
            self.emit("Error:G2/G3 bad parameters");
            return Outcome::Done;
        }
        let (cx, cy) = (start[0] + ox, start[1] + oy);
        let (rx, ry) = (-ox, -oy);
        let (tx, ty) = (target[0] - cx, target[1] - cy);
        // Marlin decides "full circle" by comparing the end points (NEAR),
        // not the angle: recomputing the end from the centre can be off by
        // an ulp, which would otherwise turn a circle into a no-op.
        let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
        let mut travel = if near(start[0], target[0]) && near(start[1], target[1]) {
            if clockwise {
                -std::f64::consts::TAU
            } else {
                std::f64::consts::TAU
            }
        } else {
            let mut t = (rx * ty - ry * tx).atan2(rx * tx + ry * ty);
            if t == 0.0 {
                // Angular travel too small to detect: Marlin skips the arc.
                return Outcome::Done;
            }
            if t < 0.0 && !clockwise {
                t += std::f64::consts::TAU;
            } else if t > 0.0 && clockwise {
                t -= std::f64::consts::TAU;
            }
            t
        };
        // Marlin: at least MIN_CIRCLE_SEGMENTS per full circle (before
        // adding P turns), otherwise one segment per MAX_ARC_SEGMENT_MM.
        let min_segments = (MIN_CIRCLE_SEGMENTS * travel.abs() / std::f64::consts::TAU).ceil();
        let turns = c.int('P').unwrap_or(0).clamp(0, 100) as f64;
        travel += travel.signum() * turns * std::f64::consts::TAU;
        let dz = target[2] - start[2];
        let flat = travel.abs() * radius;
        let n = ((flat / ARC_SEGMENT_MM).round().max(min_segments) as usize).clamp(1, 20_000);
        let de_total = self.physical_e_delta(target[3] - start[3]);
        let e0 = self.planner.target()[3];
        let feed = self.motion.feedrate * self.motion.feedrate_pct / 100.0;
        let start_angle = ry.atan2(rx);
        let kind = if de_total != 0.0 {
            MoveKind::Print
        } else {
            MoveKind::Travel
        };
        let mut segments = VecDeque::with_capacity(n);
        for i in 1..=n {
            let f = i as f64 / n as f64;
            let native = if i == n {
                target
            } else {
                let a = start_angle + travel * f;
                [
                    cx + radius * a.cos(),
                    cy + radius * a.sin(),
                    start[2] + dz * f,
                    start[3] + (target[3] - start[3]) * f,
                ]
            };
            segments.push_back(Segment {
                target: self.physical(native, e0 + de_total * f),
                feed,
                kind,
            });
        }
        self.motion.native = target;
        self.queue_segments(segments)
    }

    /// G4 P<ms> / S<s>.
    pub(super) fn cmd_dwell(&mut self, c: &Command) -> Outcome {
        let secs = c
            .value('S')
            .map(|s| s.max(0.0))
            .or_else(|| c.value('P').map(|p| p.max(0.0) / 1000.0))
            .unwrap_or(0.0)
            .min(86_400.0);
        Outcome::Task {
            kind: TaskKind::Dwell { secs, until: 0.0 },
            sync: true,
        }
    }

    /// G10 / G11 firmware retraction.
    pub(super) fn cmd_firmware_retract(&mut self, retract: bool) -> Outcome {
        if retract == self.motion.retracted {
            return Outcome::Done;
        }
        self.motion.retracted = retract;
        let s = &self.settings;
        let (de, dz, feed) = if retract {
            (-s.retract_length, s.retract_zraise, s.retract_feedrate)
        } else {
            (
                s.retract_length + s.recover_extra,
                -s.retract_zraise,
                s.recover_feedrate,
            )
        };
        let mut target = self.motion.native;
        target[3] += de;
        target[2] += dz;
        self.linear_to(target, feed)
    }

    /// G92.
    pub(super) fn cmd_set_position(&mut self, c: &Command) -> Outcome {
        let unit = self.unit_factor();
        for (i, letter) in AXES.iter().enumerate() {
            if let Some(v) = c.value(*letter) {
                let v = (v * unit).clamp(-1.0e6, 1.0e6);
                if i == 3 {
                    self.motion.native[3] = v;
                } else {
                    self.motion.shift[i] = v - self.motion.native[i] - self.settings.home_offset[i];
                }
            }
        }
        Outcome::Done
    }

    // ----- homing -----

    fn homing_time(&self, axes: [bool; 3]) -> f64 {
        if self.cfg.instant_moves {
            return 0.0;
        }
        let vol = self.cfg.build_volume.map(f64::from);
        let n = self.motion.native;
        let dist = |i: usize, unknown: f64| {
            if self.motion.homed[i] {
                n[i].abs()
            } else {
                unknown
            }
        };
        let mut t = 0.0;
        if axes[0] || axes[1] {
            // Raise Z before moving XY (Z_HOMING_HEIGHT).
            let raise = if self.motion.homed[2] {
                (4.0 - n[2]).max(0.0)
            } else {
                4.0
            };
            t += raise / HOMING_FEED_Z;
        }
        for i in 0..2 {
            if axes[i] {
                // Fast approach, 5 mm bump back and a slow second approach.
                t += dist(i, vol[i] * 0.5) / HOMING_FEED_XY
                    + 5.0 / HOMING_FEED_XY
                    + 10.0 / HOMING_FEED_XY
                    + 0.2;
            }
        }
        if axes[2] {
            // Z_SAFE_HOMING: travel to the bed centre, then probe down.
            let travel = (vol[0] * 0.5).hypot(vol[1] * 0.5);
            t += travel / PROBE_TRAVEL_FEED;
            // Fast probe, 2 mm bump up, slow probe at 2 mm/s, deploy/stow.
            t += (dist(2, 20.0) + 4.0) / HOMING_FEED_Z + 2.0 / HOMING_FEED_Z + 1.0 + 1.0;
        }
        t
    }

    /// G28 [X] [Y] [Z] [O].
    pub(super) fn cmd_home(&mut self, c: &Command) -> Outcome {
        let mut axes = [c.seen('X'), c.seen('Y'), c.seen('Z')];
        if !axes.iter().any(|a| *a) {
            axes = [true; 3];
        }
        if c.seen('O') && (0..3).all(|i| !axes[i] || self.motion.homed[i]) {
            return Outcome::Done;
        }
        Outcome::Task {
            kind: TaskKind::Home(Homing { axes, until: 0.0 }),
            sync: true,
        }
    }

    pub(super) fn begin_homing(&mut self, h: &mut Homing) {
        h.until = self.now + self.homing_time(h.axes);
        self.motion.steppers = true;
    }

    pub(super) fn progress_homing(&mut self, h: &mut Homing) -> bool {
        if self.now < h.until {
            return false;
        }
        self.finish_homing(h.axes);
        true
    }

    fn finish_homing(&mut self, axes: [bool; 3]) {
        for (i, home) in axes.iter().enumerate() {
            if *home {
                self.motion.native[i] = 0.0;
                self.motion.shift[i] = 0.0;
                self.motion.homed[i] = true;
            }
        }
        self.motion.babystep = [0.0; 3];
        let e = self.planner.target()[3];
        let p = self.physical(self.motion.native, e);
        self.planner.set_position(p);
    }

    // ----- probing / leveling -----

    /// G29: probe the bed (homing first if needed) and enable leveling.
    pub(super) fn cmd_probe(&mut self) -> Outcome {
        let home_first = [
            !self.motion.homed[0],
            !self.motion.homed[1],
            !self.motion.homed[2],
        ];
        Outcome::Task {
            kind: TaskKind::Probe(Probing {
                home_first,
                until: 0.0,
            }),
            sync: true,
        }
    }

    pub(super) fn begin_probing(&mut self, p: &mut Probing) {
        let mut t = if p.home_first.iter().any(|h| *h) {
            self.homing_time(p.home_first)
        } else {
            0.0
        };
        if !self.cfg.instant_moves {
            let (cols, rows) = self.cfg.mesh_size;
            let points = (cols * rows) as f64;
            let vol = self.cfg.build_volume.map(f64::from);
            let spacing = (vol[0] / cols as f64).min(vol[1] / rows as f64);
            t += points * (PROBE_POINT_TIME + spacing / PROBE_TRAVEL_FEED) + 2.0;
        }
        p.until = self.now + t;
        self.leveling = false;
        self.motion.steppers = true;
    }

    pub(super) fn progress_probing(&mut self, p: &mut Probing) -> bool {
        if self.now < p.until {
            return false;
        }
        if p.home_first.iter().any(|h| *h) {
            self.finish_homing(p.home_first);
        }
        let (cols, rows) = self.cfg.mesh_size;
        let vol = self.cfg.build_volume.map(f64::from);
        let off = self.settings.probe_offset;
        self.probe_runs += 1;
        let mesh = Mesh::probe(
            cols,
            rows,
            [vol[0], vol[1]],
            [off[0], off[1]],
            &self.bed_shape,
            self.cfg.seed,
            self.probe_runs,
        );
        self.out.extend(mesh.report());
        // The nozzle ends above the last probed point.
        let last_x = mesh.x.last().copied().unwrap_or(0.0);
        let last_y = mesh.y.last().copied().unwrap_or(0.0);
        self.mesh = Some(mesh);
        self.leveling = true;
        self.motion.native[0] = (last_x - off[0]).clamp(0.0, vol[0]);
        self.motion.native[1] = (last_y - off[1]).clamp(0.0, vol[1]);
        self.motion.native[2] = PROBE_CLEARANCE;
        let e = self.planner.target()[3];
        let pos = self.physical(self.motion.native, e);
        self.planner.set_position(pos);
        true
    }

    /// M420 S<0|1> Z<fade> V.
    pub(super) fn cmd_leveling(&mut self, c: &Command) -> Outcome {
        if let Some(z) = c.value('Z')
            && z.is_finite()
        {
            self.settings.fade_height = z.clamp(0.0, 100.0);
        }
        if c.seen('V')
            && let Some(mesh) = &self.mesh
        {
            let report = mesh.report();
            self.out.extend(report);
        }
        if c.seen('S') {
            let enable = c.flag('S');
            if enable && self.mesh.is_none() {
                self.leveling = false;
                self.emit("Error:Failed to enable Bed Leveling");
            } else {
                self.leveling = enable;
            }
            // Keep the commanded position; the planner applies the new
            // correction from the next move on.
        }
        self.emit(format!(
            "echo:Bed Leveling {}",
            if self.leveling { "ON" } else { "OFF" }
        ));
        let fade = self.settings.fade_height;
        if fade > 0.0 {
            self.emit(format!("echo:Fade Height {}", f2(fade)));
        } else {
            self.emit("echo:Fade Height OFF");
        }
        Outcome::Done
    }

    // ----- overrides and small commands -----

    /// M220 [S<percent>] [B] [R].
    pub(super) fn cmd_feedrate_pct(&mut self, c: &Command) -> Outcome {
        if !c.has_params() {
            self.emit(format!("FR:{}%", self.motion.feedrate_pct.round() as i64));
            return Outcome::Done;
        }
        if let Some(s) = c.value('S')
            && s.is_finite()
        {
            self.motion.feedrate_pct = s.trunc().clamp(10.0, 999.0);
        }
        Outcome::Done
    }

    /// M221 [T<tool>] [S<percent>].
    pub(super) fn cmd_flow_pct(&mut self, c: &Command) -> Outcome {
        let tool = match c.int('T') {
            None => self.motion.tool,
            Some(t) if t >= 0 && (t as usize) < self.hotends.len() => t as usize,
            Some(t) => {
                self.emit(format!("echo:T{t} Invalid extruder"));
                return Outcome::Done;
            }
        };
        match c.value('S') {
            Some(s) if s.is_finite() => {
                if let Some(f) = self.motion.flow_pct.get_mut(tool) {
                    *f = s.trunc().clamp(1.0, 999.0);
                }
            }
            _ => {
                let f = self.motion.flow_pct.get(tool).copied().unwrap_or(100.0);
                self.emit(format!("echo:E{tool} Flow: {}%", f.round() as i64));
            }
        }
        Outcome::Done
    }

    /// M290 X Y Z / S babystepping (accumulates).
    pub(super) fn cmd_babystep(&mut self, c: &Command) -> Outcome {
        let mut any = false;
        for (i, letter) in ['X', 'Y', 'Z'].iter().enumerate() {
            if let Some(v) = c.value(*letter)
                && v.is_finite()
            {
                self.motion.babystep[i] += v.clamp(-2.0, 2.0);
                any = true;
            }
        }
        if let Some(v) = c.value('S')
            && v.is_finite()
        {
            self.motion.babystep[2] += v.clamp(-2.0, 2.0);
            any = true;
        }
        if !any {
            self.emit(format!("echo:Babystep Z{}", f2(self.motion.babystep[2])));
        }
        Outcome::Done
    }

    /// M211 [S<0|1>].
    pub(super) fn cmd_soft_endstops(&mut self, c: &Command) -> Outcome {
        if c.seen('S') {
            self.motion.soft_endstops = c.flag('S');
            return Outcome::Done;
        }
        let on = self.motion.soft_endstops;
        let v = self.cfg.build_volume.map(f64::from);
        let lo: Vec<f64> = (0..3).map(|i| self.offset(i)).collect();
        self.emit("echo:; Soft endstops:");
        self.emit(format!(
            "echo:  M211 S{} ; {}",
            u8::from(on),
            if on { "ON" } else { "OFF" }
        ));
        self.emit(format!(
            "echo:  Min:  X{} Y{} Z{}   Max:  X{} Y{} Z{}",
            f2(lo[0]),
            f2(lo[1]),
            f2(lo[2]),
            f2(v[0] + lo[0]),
            f2(v[1] + lo[1]),
            f2(v[2] + lo[2])
        ));
        Outcome::Done
    }

    /// M302 [S<temp>] [P<0|1>].
    pub(super) fn cmd_cold_extrude(&mut self, c: &Command) -> Outcome {
        let seen_s = c.seen('S');
        if let Some(s) = c.value('S')
            && s.is_finite()
        {
            self.extrude_min_temp = s.clamp(0.0, 300.0);
            self.allow_cold_extrude = self.extrude_min_temp == 0.0;
        }
        if c.seen('P') {
            self.allow_cold_extrude = self.extrude_min_temp == 0.0 || c.flag('P');
        } else if !seen_s {
            self.emit(format!(
                "echo:Cold extrudes are {}abled (min temp {}C)",
                if self.allow_cold_extrude { "en" } else { "dis" },
                self.extrude_min_temp.round() as i64
            ));
        }
        Outcome::Done
    }

    /// M119.
    pub(super) fn cmd_endstops(&mut self) -> Outcome {
        let p = self.planner.current_position();
        let state = |hit: bool| if hit { "TRIGGERED" } else { "open" };
        self.emit("Reporting endstop status");
        let x = self.motion.homed[0] && p[0] <= 1e-6;
        let y = self.motion.homed[1] && p[1] <= 1e-6;
        self.emit(format!("x_min: {}", state(x)));
        self.emit(format!("y_min: {}", state(y)));
        self.emit("z_min: open".to_string());
        self.emit("z_probe: open".to_string());
        self.emit(format!("filament: {}", state(self.runout)));
        Outcome::Done
    }
}
