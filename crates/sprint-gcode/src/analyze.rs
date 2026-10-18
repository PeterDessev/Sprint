//! Toolpath analysis: walks a file once, tessellates arcs, classifies moves,
//! detects layers and features, plans the motion for time estimates and
//! gathers statistics.

use std::f64::consts::{PI, TAU};

use serde::{Deserialize, Serialize};

use crate::estimate::{KinematicLimits, PlannedBlock, Planner};
use crate::file::GcodeFile;
use crate::meta::{first_number, is_layer_marker, read_metadata};
use crate::parse::{ExtendedCommand, ParsedLine, parse_into};
use crate::state::{MachineState, Move, MoveCommand};
use crate::toolpath::{
    Bounds, Feature, FeatureStat, Layer, MoveKind, Retraction, Segment, SlicerMetadata, Stats,
    Toolpath,
};

/// Analysis settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalyzeOptions {
    /// Filament diameter in mm (default 1.75). A diameter found in the
    /// slicer metadata takes precedence.
    pub filament_diameter: f32,
    /// Filament density in g/cm³ (default 1.24, PLA). A density found in the
    /// slicer metadata takes precedence.
    pub filament_density: f32,
    /// Maximum chord error when tessellating arcs, mm (default 0.02).
    pub arc_tolerance: f32,
    /// Motion limits for the time estimate; M201/M203/M204/M205 in the file
    /// override them while walking.
    pub limits: KinematicLimits,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        AnalyzeOptions {
            filament_diameter: 1.75,
            filament_density: 1.24,
            arc_tolerance: 0.02,
            limits: KinematicLimits::default(),
        }
    }
}

/// Analyze a whole file. `progress` gets values in 0..=1 roughly every 1% and
/// returns false to cancel (then `None` is returned).
pub fn analyze(
    file: &GcodeFile,
    options: &AnalyzeOptions,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Option<Toolpath> {
    if !progress(0.0) {
        return None;
    }
    let metadata = read_metadata(file);
    let total = file.len();
    let mut analyzer = Analyzer::new(options, &metadata, file.command_count());
    let step = (total / 100).max(1);
    let mut next_report = step;
    let mut parsed = ParsedLine::default();
    for index in 0..total {
        if index >= next_report {
            next_report += step;
            if !progress(index as f32 / total as f32) {
                return None;
            }
        }
        parse_into(file.line(index), &mut parsed);
        analyzer.line(saturating_u32(index), &parsed);
    }
    let toolpath = analyzer.finish(metadata, saturating_u32(total));
    if !progress(1.0) {
        return None;
    }
    Some(toolpath)
}

/// Convenience: analyze with no progress reporting.
pub fn analyze_simple(file: &GcodeFile, options: &AnalyzeOptions) -> Toolpath {
    analyze(file, options, &mut |_| true).unwrap_or_default()
}

/// Map a slicer feature name (`;TYPE:`, `; FEATURE:`, Simplify3D
/// `; feature`) to a [`Feature`]. Unknown names give `Feature::Unknown`.
pub fn feature_from_name(name: &str) -> Feature {
    let lower = name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "external perimeter" | "outer wall" | "wall-outer" | "outer perimeter" => {
            Feature::ExternalPerimeter
        }
        "perimeter" | "inner wall" | "wall-inner" | "inner perimeter" | "thin wall" => {
            Feature::Perimeter
        }
        "overhang perimeter" | "overhang wall" => Feature::OverhangPerimeter,
        "internal infill" | "sparse infill" | "fill" | "infill" => Feature::InternalInfill,
        "solid infill"
        | "internal solid infill"
        | "bottom surface"
        | "skin"
        | "solid layer"
        | "solid-fill"
        | "bottom solid infill" => Feature::SolidInfill,
        "top solid infill" | "top surface" => Feature::TopSolidInfill,
        "bridge infill" | "bridge" | "internal bridge infill" | "internal bridge" => {
            Feature::BridgeInfill
        }
        "gap fill" | "gap infill" => Feature::GapFill,
        "skirt" | "skirt/brim" | "brim" | "skirt-brim" | "ooze shield" => Feature::Skirt,
        "support material" | "support" | "raft" | "support transition" => Feature::Support,
        "support material interface"
        | "support interface"
        | "support-interface"
        | "dense support"
        | "support-roof"
        | "support-bottom" => Feature::SupportInterface,
        "ironing" => Feature::Ironing,
        "wipe tower" | "prime tower" | "prime-tower" | "prime pillar" => Feature::WipeTower,
        "custom" => Feature::Custom,
        _ => Feature::Unknown,
    }
}

/// Moves with more XYZ length than this (mm) are segments.
const MIN_LENGTH: f64 = 1e-9;
/// E changes smaller than this (mm) are ignored.
const MIN_E: f64 = 1e-9;
/// Z difference (mm) that separates layers.
const LAYER_EPS: f64 = 1e-4;
/// Upper bound on the segments one arc command produces.
const MAX_ARC_SEGMENTS: usize = 4096;
/// Upper bound on `P` full turns of one arc.
const MAX_ARC_TURNS: f64 = 1000.0;
/// Longest dwell honoured (s); larger values are garbage.
const MAX_DWELL: f64 = 1e7;
/// Height used when a layer height cannot be determined (mm).
const FALLBACK_HEIGHT: f64 = 0.2;

fn saturating_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

fn to_f32(p: [f64; 3]) -> [f32; 3] {
    [p[0] as f32, p[1] as f32, p[2] as f32]
}

/// A layer while it is being collected.
#[derive(Clone, Copy, Debug)]
struct RawLayer {
    /// Z of the extrusion that started the layer.
    z_base: f64,
    /// Highest extrusion Z seen in the layer (spiral layers rise).
    z_max: f64,
    seg_start: usize,
    line_start: u32,
    /// Created after the first slicer layer marker.
    after_marker: bool,
    /// Contains extrusions that change Z (spiral vase).
    sloped: bool,
    /// Height estimate when the layer was created.
    height: f64,
}

/// Running time totals.
#[derive(Clone, Copy, Debug, Default)]
struct Clock {
    now: f64,
    other: f64,
}

/// Record a finished planner block: segment timing or untracked time.
fn commit(segments: &mut [Segment], clock: &mut Clock, block: PlannedBlock) {
    let duration = if block.duration.is_finite() {
        block.duration
    } else {
        0.0
    };
    match block.tag.and_then(|t| segments.get_mut(t as usize)) {
        Some(seg) => {
            seg.time = clock.now as f32;
            seg.duration = duration as f32;
        }
        None => clock.other += duration,
    }
    clock.now += duration;
}

struct Analyzer {
    state: MachineState,
    planner: Planner,
    segments: Vec<Segment>,
    retractions: Vec<Retraction>,
    clock: Clock,

    feature: Feature,
    width_hint: Option<f64>,
    height_hint: Option<f64>,

    layers: Vec<RawLayer>,
    last_extrusion: Option<usize>,
    markers_seen: bool,
    marker_pending: bool,
    first_marked_layer_done: bool,

    filament_area: f64,
    filament_density: f64,
    arc_tolerance: f64,
    fw_retract_length: f64,
    fw_retract_feed: f64,
    fw_recover_extra: f64,
    fw_recover_feed: f64,

    tool_changes: usize,
    hotend_temps: Vec<(u8, f32)>,
    bed_temps: Vec<f32>,
    uses_relative_e: bool,
    uses_arcs: bool,
    uses_firmware_retraction: bool,
    heat_waits: usize,
}

impl Analyzer {
    fn new(options: &AnalyzeOptions, meta: &SlicerMetadata, command_count: usize) -> Analyzer {
        let diameter = meta
            .filament_diameter
            .filter(|d| (0.1..=10.0).contains(d))
            .unwrap_or(options.filament_diameter);
        let diameter = if diameter.is_finite() && diameter > 0.0 {
            f64::from(diameter)
        } else {
            1.75
        };
        let density = meta
            .filament_density
            .filter(|d| (0.1..=25.0).contains(d))
            .unwrap_or(options.filament_density);
        let density = if density.is_finite() && density > 0.0 {
            f64::from(density)
        } else {
            1.24
        };
        let arc_tolerance = f64::from(options.arc_tolerance);
        let arc_tolerance = if arc_tolerance.is_finite() && arc_tolerance > 0.0 {
            arc_tolerance.clamp(1e-4, 10.0)
        } else {
            0.02
        };
        let cura = meta
            .slicer
            .as_deref()
            .is_some_and(|s| s.to_ascii_lowercase().contains("cura"));
        Analyzer {
            state: MachineState::new(),
            planner: Planner::new(options.limits),
            segments: Vec::with_capacity(command_count),
            retractions: Vec::new(),
            clock: Clock::default(),
            // Cura: everything before ";LAYER:0" (start G-code) is custom.
            feature: if cura {
                Feature::Custom
            } else {
                Feature::Unknown
            },
            width_hint: None,
            height_hint: None,
            layers: Vec::new(),
            last_extrusion: None,
            markers_seen: false,
            marker_pending: false,
            first_marked_layer_done: false,
            filament_area: PI * diameter * diameter / 4.0,
            filament_density: density,
            arc_tolerance,
            // Marlin FWRETRACT defaults.
            fw_retract_length: 3.0,
            fw_retract_feed: 45.0,
            fw_recover_extra: 0.0,
            fw_recover_feed: 8.0,
            tool_changes: 0,
            hotend_temps: Vec::new(),
            bed_temps: Vec::new(),
            uses_relative_e: false,
            uses_arcs: false,
            uses_firmware_retraction: false,
            heat_waits: 0,
        }
    }

    // ---------------------------------------------------------------- lines

    fn line(&mut self, ln: u32, p: &ParsedLine) {
        if let Some(c) = p.comment {
            self.comment(c);
        }
        let Some(code) = p.code else {
            if let Some(cmd) = p.text.and_then(ExtendedCommand::parse) {
                self.extended(&cmd);
            }
            return;
        };
        match (code.letter, code.number, code.sub) {
            (b'G', 0..=3, None) => {
                if let Some(mv) = self.state.apply(p) {
                    self.motion(mv, p, ln);
                }
            }
            (b'G', 4, None) => {
                let secs = match (p.get('S'), p.get('P')) {
                    (Some(s), _) => s,
                    (None, Some(ms)) => ms / 1000.0,
                    (None, None) => 0.0,
                };
                self.dwell(secs);
            }
            (b'G', 10, None) if !p.has('P') && !p.has('L') => {
                self.uses_firmware_retraction = true;
                let was = self.state.firmware_retracted;
                self.state.apply(p);
                if !was {
                    self.e_only(-self.fw_retract_length, self.fw_retract_feed, ln);
                }
            }
            (b'G', 11, None) => {
                self.uses_firmware_retraction = true;
                let was = self.state.firmware_retracted;
                self.state.apply(p);
                if was {
                    let amount = self.fw_retract_length + self.fw_recover_extra;
                    self.e_only(amount, self.fw_recover_feed, ln);
                }
            }
            (b'G', 28 | 29 | 80, _) | (b'M', 0 | 1 | 400 | 600 | 226, None) => {
                self.sync();
                self.state.apply(p);
            }
            (b'M', 104 | 109, None) => {
                if code.number == 109 {
                    self.sync();
                    self.heat_waits += 1;
                }
                self.state.apply(p);
                let tool = p
                    .get('T')
                    .filter(|t| (0.0..=255.0).contains(t))
                    .map_or(self.state.tool, |t| t as u8);
                self.record_hotend(tool);
            }
            (b'M', 140 | 190, None) => {
                if code.number == 190 {
                    self.sync();
                    self.heat_waits += 1;
                }
                self.state.apply(p);
                let t = self.state.bed_target;
                if t > 0.0 && !self.bed_temps.iter().any(|b| (b - t).abs() < 0.01) {
                    self.bed_temps.push(t);
                }
            }
            (b'M', 191, None) => {
                self.sync();
                self.heat_waits += 1;
                self.state.apply(p);
            }
            (b'M', 201 | 203 | 204 | 205, None) => {
                self.planner.limits_mut().apply_gcode(p);
            }
            (b'M', 207, None) => {
                if let Some(s) = p.get('S').filter(|v| v.is_finite() && *v >= 0.0) {
                    self.fw_retract_length = s.min(1000.0);
                }
                if let Some(f) = p.get('F').filter(|v| v.is_finite() && *v > 0.0) {
                    self.fw_retract_feed = f / 60.0;
                }
            }
            (b'M', 208, None) => {
                if let Some(s) = p.get('S').filter(|v| v.is_finite()) {
                    self.fw_recover_extra = s.clamp(-1000.0, 1000.0);
                }
                if let Some(f) = p.get('F').filter(|v| v.is_finite() && *v > 0.0) {
                    self.fw_recover_feed = f / 60.0;
                }
            }
            (b'M', 83, None) => {
                self.uses_relative_e = true;
                self.state.apply(p);
            }
            (b'T', _, None) => {
                let before = self.state.tool;
                self.state.apply(p);
                if self.state.tool != before {
                    self.sync();
                    self.tool_changes += 1;
                }
            }
            _ => {
                self.state.apply(p);
            }
        }
    }

    fn extended(&mut self, cmd: &ExtendedCommand<'_>) {
        if self.planner.limits_mut().apply_extended(cmd) {
            return;
        }
        if cmd.is("SET_RETRACTION") {
            if let Some(len) = cmd
                .param_f64("RETRACT_LENGTH")
                .filter(|v| (0.0..=1000.0).contains(v))
            {
                self.fw_retract_length = len;
            }
            if let Some(speed) = cmd.param_f64("RETRACT_SPEED").filter(|v| *v > 0.0) {
                self.fw_retract_feed = speed;
            }
            if let Some(speed) = cmd.param_f64("UNRETRACT_SPEED").filter(|v| *v > 0.0) {
                self.fw_recover_feed = speed;
            }
            if let Some(extra) = cmd
                .param_f64("UNRETRACT_EXTRA_LENGTH")
                .filter(|v| v.abs() <= 1000.0)
            {
                self.fw_recover_extra = extra;
            }
        }
    }

    fn record_hotend(&mut self, tool: u8) {
        let t = self.state.hotend_target(tool);
        if t > 0.0
            && !self
                .hotend_temps
                .iter()
                .any(|&(tt, c)| tt == tool && (c - t).abs() < 0.01)
        {
            self.hotend_temps.push((tool, t));
        }
    }

    fn comment(&mut self, c: &str) {
        let Some(&first) = c.as_bytes().first() else {
            return;
        };
        match first {
            b'T' => {
                if let Some(v) = c.strip_prefix("TYPE:") {
                    self.feature = feature_from_name(v);
                }
            }
            b'F' => {
                if let Some(v) = c.strip_prefix("FEATURE:") {
                    self.feature = feature_from_name(v);
                }
            }
            b'f' => {
                if let Some(v) = c.strip_prefix("feature ") {
                    self.feature = feature_from_name(v);
                }
            }
            b'W' => {
                if let Some(v) = c.strip_prefix("WIDTH:") {
                    self.width_hint = sane_size(first_number(v)).or(self.width_hint);
                }
            }
            b'H' => {
                if let Some(v) = c.strip_prefix("HEIGHT:") {
                    self.height_hint = sane_size(first_number(v)).or(self.height_hint);
                }
            }
            b'L' => {
                if let Some(v) = c.strip_prefix("LAYER_HEIGHT:") {
                    self.height_hint = sane_size(first_number(v)).or(self.height_hint);
                } else if is_layer_marker(c) {
                    self.marker();
                }
            }
            b't' => {
                // Simplify3D: "; tool H0.200 W0.480".
                if let Some(rest) = c.strip_prefix("tool ") {
                    for word in rest.split_whitespace() {
                        if let Some(h) = word.strip_prefix('H') {
                            self.height_hint = sane_size(first_number(h)).or(self.height_hint);
                        } else if let Some(w) = word.strip_prefix('W') {
                            self.width_hint = sane_size(first_number(w)).or(self.width_hint);
                        }
                    }
                }
            }
            b'C' | b'l' | b'B' if is_layer_marker(c) => self.marker(),
            _ => {}
        }
    }

    fn marker(&mut self) {
        self.markers_seen = true;
        self.marker_pending = true;
    }

    // --------------------------------------------------------------- motion

    fn motion(&mut self, mv: Move, p: &ParsedLine, ln: u32) {
        let off = self.state.origin_offset;
        let from = [
            mv.from[0] + off[0],
            mv.from[1] + off[1],
            mv.from[2] + off[2],
        ];
        let to = [mv.to[0] + off[0], mv.to[1] + off[1], mv.to[2] + off[2]];
        let de = mv.to[3] - mv.from[3];
        let feed = mv.feedrate_mm_min / 60.0;
        match mv.kind {
            MoveCommand::Rapid | MoveCommand::Linear => self.add_move(from, to, de, feed, ln),
            MoveCommand::ArcCw | MoveCommand::ArcCcw => {
                self.uses_arcs = true;
                self.arc(from, to, de, feed, p, mv.kind == MoveCommand::ArcCw, ln);
            }
            MoveCommand::Home => {}
        }
    }

    /// Tessellate a G2/G3 arc (I/J centre or R radius form, `P` extra full
    /// turns, helical Z, E proportional to length) into segments.
    #[allow(clippy::too_many_arguments)]
    fn arc(
        &mut self,
        from: [f64; 3],
        to: [f64; 3],
        de: f64,
        feed: f64,
        p: &ParsedLine,
        clockwise: bool,
        ln: u32,
    ) {
        let units = if self.state.inches { 25.4 } else { 1.0 };
        let (x1, y1) = (from[0], from[1]);
        let (x2, y2) = (to[0], to[1]);
        let same_xy = (x2 - x1).abs() < 1e-9 && (y2 - y1).abs() < 1e-9;
        let center = if let Some(r) = p.get('R') {
            let r = r * units;
            if same_xy || r == 0.0 || !r.is_finite() {
                None
            } else {
                // Marlin's R-form centre: perpendicular bisector of the chord.
                let d2 = ((x2 - x1) * 0.5, (y2 - y1) * 0.5);
                let len = (d2.0 * d2.0 + d2.1 * d2.1).sqrt();
                let e = if clockwise ^ (r < 0.0) { -1.0 } else { 1.0 };
                let h2 = (r - len) * (r + len);
                let h = if h2 >= 0.0 { h2.sqrt() } else { 0.0 };
                let s = (-d2.1, d2.0);
                Some((x1 + d2.0 + s.0 / len * e * h, y1 + d2.1 + s.1 / len * e * h))
            }
        } else if p.has('I') || p.has('J') {
            let i = p.get('I').unwrap_or(0.0) * units;
            let j = p.get('J').unwrap_or(0.0) * units;
            Some((x1 + i, y1 + j))
        } else {
            None
        };
        let Some((cx, cy)) = center.filter(|c| c.0.is_finite() && c.1.is_finite()) else {
            self.add_move(from, to, de, feed, ln);
            return;
        };
        let (rx, ry) = (x1 - cx, y1 - cy);
        let radius = (rx * rx + ry * ry).sqrt();
        if !radius.is_finite() || radius < 1e-6 {
            self.add_move(from, to, de, feed, ln);
            return;
        }
        let (tx, ty) = (x2 - cx, y2 - cy);
        let mut travel = (rx * ty - ry * tx).atan2(rx * tx + ry * ty);
        if travel < 0.0 {
            travel += TAU;
        }
        if clockwise {
            travel -= TAU;
        }
        if travel == 0.0 && same_xy {
            travel = TAU;
        }
        let turns = p
            .get('P')
            .filter(|v| v.is_finite())
            .map_or(0.0, |v| v.clamp(0.0, MAX_ARC_TURNS).floor());
        let sweep = travel.abs() + TAU * turns;
        let direction = if travel < 0.0 { -1.0 } else { 1.0 };
        let tol = self.arc_tolerance;
        let max_step = if tol < radius {
            (2.0 * (1.0 - tol / radius).acos()).min(TAU / 16.0)
        } else {
            TAU / 16.0
        };
        let max_step = if max_step > 1e-9 { max_step } else { 1e-9 };
        let n = ((sweep / max_step).ceil() as usize).clamp(1, MAX_ARC_SEGMENTS);
        let start_angle = ry.atan2(rx);
        let dz = to[2] - from[2];
        let de_step = de / n as f64;
        let mut prev = from;
        for k in 1..=n {
            let point = if k == n {
                to
            } else {
                let t = k as f64 / n as f64;
                let a = start_angle + direction * sweep * t;
                [
                    cx + radius * a.cos(),
                    cy + radius * a.sin(),
                    from[2] + dz * t,
                ]
            };
            self.add_move(prev, point, de_step, feed, ln);
            prev = point;
        }
    }

    /// One straight move (machine XYZ, E delta, feedrate in mm/s).
    fn add_move(&mut self, from: [f64; 3], to: [f64; 3], de: f64, feed: f64, ln: u32) {
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if !len.is_finite() || !de.is_finite() {
            return;
        }
        let speed = feed * f64::from(self.state.feedrate_pct) / 100.0;
        if len < MIN_LENGTH {
            if de.abs() > MIN_E {
                let pos = to_f32(to);
                self.retractions.push(Retraction {
                    pos,
                    amount: de as f32,
                    line: ln,
                    layer: 0,
                    tool: self.state.tool,
                });
                self.plan([0.0, 0.0, 0.0, de], speed, None);
            }
            return;
        }
        let extruding = de > MIN_E;
        let index = self.segments.len();
        let (width, height) = if extruding {
            self.layer_for_extrusion(index, from[2], to[2], ln);
            let height = self.segment_height();
            let width = match self.width_hint {
                Some(w) => w,
                None => (de * self.filament_area / (len * height)).clamp(0.0, 10.0),
            };
            (width, height)
        } else {
            (0.0, 0.0)
        };
        self.segments.push(Segment {
            start: to_f32(from),
            end: to_f32(to),
            feedrate: feed as f32,
            width: width as f32,
            height: height as f32,
            extruded: if extruding { de as f32 } else { 0.0 },
            time: 0.0,
            duration: 0.0,
            line: ln,
            layer: 0,
            kind: if extruding {
                MoveKind::Extrude
            } else {
                MoveKind::Travel
            },
            feature: self.feature,
            tool: self.state.tool,
            fan: self.state.fan,
        });
        if extruding {
            self.last_extrusion = Some(index);
        }
        self.plan([d[0], d[1], d[2], de], speed, Some(saturating_u32(index)));
    }

    /// Firmware retraction / recovery: an E-only move.
    fn e_only(&mut self, amount: f64, speed: f64, ln: u32) {
        if amount.abs() <= MIN_E {
            return;
        }
        let m = self.state.machine_position();
        self.retractions.push(Retraction {
            pos: to_f32(m),
            amount: amount as f32,
            line: ln,
            layer: 0,
            tool: self.state.tool,
        });
        self.plan([0.0, 0.0, 0.0, amount], speed, None);
    }

    fn plan(&mut self, delta: [f64; 4], speed: f64, tag: Option<u32>) {
        if let Some(block) = self.planner.push(delta, speed, tag) {
            commit(&mut self.segments, &mut self.clock, block);
        }
    }

    /// Bring the planner to a stop (dwell, heater wait, homing, ...).
    fn sync(&mut self) {
        let Analyzer {
            planner,
            segments,
            clock,
            ..
        } = self;
        planner.flush(|block| commit(segments, clock, block));
    }

    fn dwell(&mut self, secs: f64) {
        self.sync();
        if secs.is_finite() && secs > 0.0 {
            let secs = secs.min(MAX_DWELL);
            self.clock.now += secs;
            self.clock.other += secs;
        }
    }

    // --------------------------------------------------------------- layers

    /// Decide whether the extrusion about to become segment `index` starts a
    /// new layer.
    fn layer_for_extrusion(&mut self, index: usize, z_start: f64, z_end: f64, ln: u32) {
        let sloped = (z_end - z_start).abs() > LAYER_EPS;
        let marker = std::mem::take(&mut self.marker_pending);
        let Some(current) = self.layers.last().copied() else {
            if self.markers_seen {
                self.first_marked_layer_done = true;
            }
            self.push_layer(index, z_end, sloped, ln, false);
            return;
        };
        let new_layer = if (z_end - current.z_base).abs() <= LAYER_EPS {
            false
        } else if !sloped {
            // Planar extrusion at another Z: a new layer (Z hops are travel
            // and never get here).
            true
        } else if self.markers_seen {
            // Spiral vase with slicer markers: one layer per marker.
            marker
        } else {
            // Spiral vase without markers: bucket the rising Z by the
            // extrusion height.
            (z_end - current.z_base).abs() >= self.reference_height() - LAYER_EPS
        };
        let first_marked = self.markers_seen && !self.first_marked_layer_done;
        if first_marked {
            self.first_marked_layer_done = true;
        }
        if new_layer {
            self.push_layer(index, z_end, sloped, ln, first_marked);
        } else if sloped && let Some(layer) = self.layers.last_mut() {
            layer.sloped = true;
            layer.z_max = layer.z_max.max(z_end);
        }
    }

    fn push_layer(&mut self, index: usize, z: f64, sloped: bool, ln: u32, first_marked: bool) {
        let height = self.new_layer_height(z, sloped);
        // Purge lines and other start G-code extrusions before the first
        // slicer layer marker belong to the first marked layer.
        if first_marked
            && !self.layers.is_empty()
            && self.layers.len() <= 3
            && self.layers.iter().all(|l| !l.after_marker)
        {
            self.layers.clear();
            self.layers.push(RawLayer {
                z_base: z,
                z_max: z,
                seg_start: 0,
                line_start: 0,
                after_marker: true,
                sloped,
                height: if height > LAYER_EPS { height } else { z },
            });
            return;
        }
        let (seg_start, line_start) = match self.last_extrusion {
            Some(i) => (
                i + 1,
                self.segments
                    .get(i)
                    .map_or(ln, |s| s.line.saturating_add(1)),
            ),
            None => (0, 0),
        };
        let seg_start = seg_start.min(index);
        self.layers.push(RawLayer {
            z_base: z,
            z_max: z,
            seg_start,
            line_start: line_start.min(ln),
            after_marker: self.markers_seen,
            sloped,
            height,
        });
    }

    /// Height estimate for a layer starting at `z`.
    fn new_layer_height(&self, z: f64, sloped: bool) -> f64 {
        let h = if let Some(h) = self.height_hint {
            h
        } else if sloped {
            self.reference_height()
        } else {
            match self.layers.last() {
                Some(prev) => {
                    let top = if prev.sloped { prev.z_max } else { prev.z_base };
                    let d = z - top;
                    if d > LAYER_EPS { d } else { z }
                }
                None => z,
            }
        };
        if h.is_finite() && h > 1e-3 {
            h.min(10.0)
        } else {
            FALLBACK_HEIGHT
        }
    }

    /// Height used to bucket spiral layers without markers.
    fn reference_height(&self) -> f64 {
        let h = self
            .height_hint
            .or_else(|| self.layers.last().map(|l| l.height))
            .unwrap_or(FALLBACK_HEIGHT);
        if h.is_finite() {
            h.clamp(0.02, 2.0)
        } else {
            FALLBACK_HEIGHT
        }
    }

    /// Height of extrusion segments right now.
    fn segment_height(&self) -> f64 {
        let h = self
            .height_hint
            .or_else(|| self.layers.last().map(|l| l.height))
            .unwrap_or(FALLBACK_HEIGHT);
        if h.is_finite() && h > 1e-3 {
            h.min(10.0)
        } else {
            FALLBACK_HEIGHT
        }
    }

    // --------------------------------------------------------------- finish

    fn finish(mut self, metadata: SlicerMetadata, line_count: u32) -> Toolpath {
        self.sync();
        let total_time = self.clock.now;
        let layers = self.build_layers(line_count, total_time);
        let mut retractions = std::mem::take(&mut self.retractions);
        for r in &mut retractions {
            r.layer = layer_index_at_line(&layers, r.line);
        }
        let mut toolpath = Toolpath {
            segments: Vec::new(),
            layers,
            retractions,
            bounds: Bounds::empty(),
            travel_bounds: Bounds::empty(),
            stats: Stats::default(),
            metadata,
            line_count,
        };
        self.fill_stats(&mut toolpath, total_time);
        toolpath.segments = std::mem::take(&mut self.segments);
        toolpath
    }

    /// Turn raw layers into final layers and stamp `Segment::layer`.
    fn build_layers(&mut self, line_count: u32, total_time: f64) -> Vec<Layer> {
        let n = self.segments.len();
        if self.layers.is_empty() {
            if n == 0 {
                return Vec::new();
            }
            // Travel-only file: one layer so every segment has a home.
            let z = self.segments.last().map_or(0.0, |s| s.end[2]);
            return vec![Layer {
                z,
                height: 0.0,
                segments: 0..n,
                lines: 0..line_count,
                time_start: 0.0,
                duration: total_time as f32,
                extruded: 0.0,
            }];
        }
        let raw = std::mem::take(&mut self.layers);
        let mut layers: Vec<Layer> = Vec::with_capacity(raw.len());
        let mut prev_z: Option<f64> = None;
        for (i, r) in raw.iter().enumerate() {
            let seg_end = raw.get(i + 1).map_or(n, |next| next.seg_start).min(n);
            let line_end = raw
                .get(i + 1)
                .map_or(line_count, |next| next.line_start)
                .max(r.line_start);
            let z = if r.sloped { r.z_max } else { r.z_base };
            let height = match prev_z {
                None => z,
                Some(p) if z - p > LAYER_EPS => z - p,
                Some(_) => r.height,
            };
            prev_z = Some(z);
            let seg_start = r.seg_start.min(seg_end);
            let time_start = if i == 0 {
                0.0
            } else {
                self.segments.get(seg_start).map_or(0.0, |s| s.time)
            };
            for s in self
                .segments
                .get_mut(seg_start..seg_end)
                .into_iter()
                .flatten()
            {
                s.layer = saturating_u32(i);
            }
            layers.push(Layer {
                z: z as f32,
                height: height.max(0.0) as f32,
                segments: seg_start..seg_end,
                lines: r.line_start..line_end,
                time_start,
                duration: 0.0,
                extruded: 0.0,
            });
        }
        let total = total_time as f32;
        for i in 0..layers.len() {
            let end = layers.get(i + 1).map_or(total, |l| l.time_start);
            if let Some(l) = layers.get_mut(i) {
                l.duration = (end - l.time_start).max(0.0);
            }
        }
        layers
    }

    fn fill_stats(&self, toolpath: &mut Toolpath, total_time: f64) {
        #[derive(Clone, Copy, Default)]
        struct Acc {
            time: f64,
            extruded: f64,
            distance: f64,
            segments: usize,
        }
        let mut features = [Acc::default(); Feature::COUNT];
        let mut per_tool: Vec<f64> = Vec::new();
        let mut layer_extruded = vec![0.0f64; toolpath.layers.len()];
        let mut extrude_moves = 0usize;
        let mut travel_moves = 0usize;
        let mut travel_distance = 0.0f64;
        let mut travel_time = 0.0f64;
        let mut max_print = 0.0f32;
        let mut min_print = f32::INFINITY;
        let mut max_travel = 0.0f32;
        let mut speed_time = 0.0f64;
        let mut speed_distance = 0.0f64;
        let mut bounds = Bounds::empty();
        let mut travel_bounds = Bounds::empty();
        for s in &self.segments {
            travel_bounds.include(s.start);
            travel_bounds.include(s.end);
            let len = f64::from(s.length());
            if s.kind == MoveKind::Extrude {
                bounds.include(s.start);
                bounds.include(s.end);
                extrude_moves += 1;
                let acc = &mut features[s.feature.index()];
                acc.time += f64::from(s.duration);
                acc.extruded += f64::from(s.extruded);
                acc.distance += len;
                acc.segments += 1;
                let tool = usize::from(s.tool);
                if per_tool.len() <= tool {
                    per_tool.resize(tool + 1, 0.0);
                }
                if let Some(t) = per_tool.get_mut(tool) {
                    *t += f64::from(s.extruded);
                }
                if let Some(l) = layer_extruded.get_mut(s.layer as usize) {
                    *l += f64::from(s.extruded);
                }
                max_print = max_print.max(s.feedrate);
                min_print = min_print.min(s.feedrate);
                speed_time += f64::from(s.feedrate) * f64::from(s.duration);
                speed_distance += f64::from(s.feedrate) * len;
            } else {
                travel_moves += 1;
                travel_distance += len;
                travel_time += f64::from(s.duration);
                max_travel = max_travel.max(s.feedrate);
            }
        }
        for (layer, extruded) in toolpath.layers.iter_mut().zip(&layer_extruded) {
            layer.extruded = *extruded as f32;
        }
        let feature_stats: Vec<FeatureStat> = Feature::ALL
            .iter()
            .zip(&features)
            .filter(|(_, a)| a.segments > 0)
            .map(|(&feature, a)| FeatureStat {
                feature,
                time: a.time as f32,
                extruded: a.extruded as f32,
                distance: a.distance as f32,
                segments: a.segments,
            })
            .collect();
        let filament: f64 = features.iter().map(|a| a.extruded).sum();
        let extrude_distance: f64 = features.iter().map(|a| a.distance).sum();
        let extrude_time: f64 = features.iter().map(|a| a.time).sum();
        let avg_print = if extrude_time > 0.0 {
            speed_time / extrude_time
        } else if extrude_distance > 0.0 {
            speed_distance / extrude_distance
        } else {
            0.0
        };
        let tools_used: Vec<u8> = per_tool
            .iter()
            .enumerate()
            .filter(|(_, v)| **v > 0.0)
            .filter_map(|(i, _)| u8::try_from(i).ok())
            .collect();
        let volume = filament * self.filament_area;
        let layer_heights: Vec<f32> = toolpath.layers.iter().map(|l| l.height).collect();
        toolpath.bounds = bounds;
        toolpath.travel_bounds = travel_bounds;
        toolpath.stats = Stats {
            estimated_time: total_time as f32,
            filament_length: filament as f32,
            filament_per_tool: per_tool.iter().map(|v| *v as f32).collect(),
            filament_volume: volume as f32,
            filament_weight: (volume / 1000.0 * self.filament_density) as f32,
            extrude_moves,
            travel_moves,
            retractions: toolpath
                .retractions
                .iter()
                .filter(|r| r.amount < 0.0)
                .count(),
            tool_changes: self.tool_changes,
            travel_distance: travel_distance as f32,
            extrude_distance: extrude_distance as f32,
            max_print_speed: max_print,
            min_print_speed: if min_print.is_finite() {
                min_print
            } else {
                0.0
            },
            avg_print_speed: avg_print as f32,
            max_travel_speed: max_travel,
            layer_count: toolpath.layers.len(),
            first_layer_height: layer_heights.first().copied().unwrap_or(0.0),
            layer_height: most_common(&layer_heights),
            features: feature_stats,
            tools_used,
            hotend_temps: self.hotend_temps.clone(),
            bed_temps: self.bed_temps.clone(),
            uses_relative_e: self.uses_relative_e,
            uses_arcs: self.uses_arcs,
            uses_firmware_retraction: self.uses_firmware_retraction,
            heat_waits: self.heat_waits,
            travel_time: travel_time as f32,
            other_time: self.clock.other as f32,
        };
    }
}

fn sane_size(v: Option<f32>) -> Option<f64> {
    v.filter(|v| v.is_finite() && *v > 0.0 && *v < 10.0)
        .map(f64::from)
}

/// Index of the layer whose line range contains `line` (the last layer that
/// starts at or before it).
fn layer_index_at_line(layers: &[Layer], line: u32) -> u32 {
    let n = layers.partition_point(|l| l.lines.start <= line);
    saturating_u32(n.saturating_sub(1))
}

/// Most common value, bucketed to micrometres (0 for an empty slice).
fn most_common(values: &[f32]) -> f32 {
    let mut buckets: Vec<(i64, usize)> = Vec::new();
    for v in values {
        if !v.is_finite() {
            continue;
        }
        let key = (f64::from(*v) * 1000.0).round() as i64;
        match buckets.iter_mut().find(|(k, _)| *k == key) {
            Some((_, count)) => *count += 1,
            None => buckets.push((key, 1)),
        }
    }
    buckets
        .iter()
        // Ties go to the smaller height (stable across runs).
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map_or(0.0, |(k, _)| *k as f32 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Toolpath {
        analyze_simple(&GcodeFile::from_text("t", text), &AnalyzeOptions::default())
    }

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol * (1.0 + a.abs().max(b.abs()))
    }

    #[test]
    fn empty_and_comment_only_files() {
        let tp = run("");
        assert!(tp.segments.is_empty() && tp.layers.is_empty());
        assert_eq!(tp.stats.estimated_time, 0.0);
        assert!(tp.bounds.is_empty());
        let tp = run("; just a comment\n\n");
        assert_eq!(tp.line_count, 2);
        assert!(tp.segments.is_empty());
    }

    #[test]
    fn classifies_moves() {
        let tp = run("M83\nG28\nG1 Z0.2 F600\nG1 X10 Y0 F1200\nG1 X20 E1\nG1 E-1 F2400\nG1 E1\n");
        // Z move, travel, extrusion.
        assert_eq!(tp.segments.len(), 3);
        assert_eq!(tp.segments[0].kind, MoveKind::Travel);
        assert_eq!(tp.segments[2].kind, MoveKind::Extrude);
        assert_eq!(tp.segments[2].line, 4);
        assert_eq!(tp.segments[2].extruded, 1.0);
        assert!((tp.segments[2].feedrate - 20.0).abs() < 1e-6);
        assert_eq!(tp.retractions.len(), 2);
        assert_eq!(tp.retractions[0].amount, -1.0);
        assert_eq!(tp.retractions[1].amount, 1.0);
        assert_eq!(tp.stats.retractions, 1);
        assert_eq!(tp.stats.extrude_moves, 1);
        assert_eq!(tp.stats.travel_moves, 2);
        assert_eq!(tp.layers.len(), 1);
        assert!((tp.layers[0].z - 0.2).abs() < 1e-6);
        assert_eq!(tp.bounds.min, [10.0, 0.0, 0.2]);
        assert_eq!(tp.bounds.max, [20.0, 0.0, 0.2]);
        assert_eq!(tp.travel_bounds.min, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn width_from_extrusion_and_hint() {
        // 10 mm long, 0.2 high: E = w h L / A for w = 0.45.
        let area = PI * 1.75 * 1.75 / 4.0;
        let e = 0.45 * 0.2 * 10.0 / area;
        let tp = run(&format!(
            "M83\nG1 Z0.2\nG1 X0 Y0\nG1 X10 E{e:.5}\n;WIDTH:0.5\nG1 X20 E{e:.5}\n"
        ));
        let ext: Vec<_> = tp.segments.iter().filter(|s| s.is_extrusion()).collect();
        assert!((ext[0].width - 0.45).abs() < 1e-3, "{}", ext[0].width);
        assert!((ext[0].height - 0.2).abs() < 1e-6);
        assert_eq!(ext[1].width, 0.5);
        let tp = run(&format!(";HEIGHT:0.3\nG1 Z0.3\nG1 X10 E{e:.5}\n"));
        assert!((tp.segments[1].height - 0.3).abs() < 1e-6);
    }

    #[test]
    fn layers_and_z_hops() {
        let mut g = String::from("G28\nM83\n");
        for layer in 1..=3 {
            let z = layer as f64 * 0.2;
            g.push_str(&format!("G1 Z{z:.2} F600\nG1 X0 Y0\nG1 X10 E0.5\n"));
            // Z hop travel.
            g.push_str(&format!(
                "G1 Z{:.2}\nG1 X0 Y10\nG1 Z{z:.2}\nG1 X10 Y10 E0.5\n",
                z + 0.4
            ));
        }
        let tp = run(&g);
        assert_eq!(tp.layers.len(), 3);
        for (i, l) in tp.layers.iter().enumerate() {
            assert!((l.z - 0.2 * (i + 1) as f32).abs() < 1e-5);
            assert!((l.height - 0.2).abs() < 1e-5);
            assert!((l.extruded - 1.0).abs() < 1e-5);
        }
        // Contiguous segment ranges covering everything, consistent indices.
        assert_eq!(tp.layers[0].segments.start, 0);
        assert_eq!(tp.layers[2].segments.end, tp.segments.len());
        for w in tp.layers.windows(2) {
            assert_eq!(w[0].segments.end, w[1].segments.start);
            assert_eq!(w[0].lines.end, w[1].lines.start);
        }
        for (i, l) in tp.layers.iter().enumerate() {
            for s in &tp.segments[l.segments.clone()] {
                assert_eq!(s.layer as usize, i);
            }
        }
        // Travels before a layer's first extrusion belong to it: the Z move
        // to the next layer is the first segment of that layer.
        let l1 = &tp.layers[1];
        assert!((tp.segments[l1.segments.start].end[2] - 0.4).abs() < 1e-5);
        assert_eq!(tp.layers[0].lines.start, 0);
        assert_eq!(tp.layers[2].lines.end, tp.line_count);
        assert_eq!(tp.stats.layer_count, 3);
        assert!((tp.stats.layer_height - 0.2).abs() < 1e-6);
        assert!((tp.stats.first_layer_height - 0.2).abs() < 1e-6);
        assert!(tp.stats.uses_relative_e);
    }

    #[test]
    fn spiral_without_markers_buckets_by_height() {
        // 3 planar layers then 10 turns rising 0.2 mm per turn over 100
        // segments each.
        let mut g = String::from("M83\n");
        for layer in 1..=3 {
            let z = layer as f64 * 0.2;
            g.push_str(&format!(
                "G1 Z{z:.3}\nG1 X10 Y0\nG1 X10 Y10 E0.4\nG1 X0 Y10 E0.4\n"
            ));
        }
        let mut z = 0.6;
        for _turn in 0..10 {
            for k in 0..100 {
                let a = TAU * f64::from(k) / 100.0;
                z += 0.002;
                g.push_str(&format!(
                    "G1 X{:.3} Y{:.3} Z{z:.4} E0.02\n",
                    10.0 * a.cos(),
                    10.0 * a.sin()
                ));
            }
        }
        let tp = run(&g);
        assert!(
            (12..=14).contains(&tp.layers.len()),
            "{} layers",
            tp.layers.len()
        );
        // The last bucket may be partial.
        let n = tp.layers.len();
        for l in &tp.layers[3..n - 1] {
            assert!(l.height > 0.15 && l.height < 0.25, "{}", l.height);
        }
    }

    #[test]
    fn spiral_with_markers_follows_markers() {
        let mut g = String::from(
            ";FLAVOR:Marlin\n;Generated with Cura_SteamEngine 5.6.0\nM82\nG1 X0 Y0 Z0.3\nG1 X50 Y0 E5 ; purge\nG92 E0\n",
        );
        let mut e = 0.0;
        let mut z = 0.2;
        g.push_str(";LAYER:0\n;TYPE:WALL-OUTER\nG1 Z0.2\nG1 X10 Y0\n");
        for k in 0..100 {
            let a = TAU * f64::from(k) / 100.0;
            e += 0.02;
            g.push_str(&format!(
                "G1 X{:.3} Y{:.3} E{e:.4}\n",
                10.0 * a.cos(),
                10.0 * a.sin()
            ));
        }
        for layer in 1..=5 {
            g.push_str(&format!(";LAYER:{layer}\n"));
            for k in 0..100 {
                let a = TAU * f64::from(k) / 100.0;
                z += 0.002;
                e += 0.02;
                g.push_str(&format!(
                    "G1 X{:.3} Y{:.3} Z{z:.4} E{e:.4}\n",
                    10.0 * a.cos(),
                    10.0 * a.sin()
                ));
            }
        }
        let tp = run(&g);
        assert_eq!(
            tp.layers.len(),
            6,
            "{:?}",
            tp.layers.iter().map(|l| l.z).collect::<Vec<_>>()
        );
        // The purge line at Z0.3 before ;LAYER:0 is part of layer 0.
        assert!((tp.layers[0].z - 0.2).abs() < 1e-5);
        assert_eq!(tp.layers[0].segments.start, 0);
        assert!(
            tp.segments[..tp.layers[0].segments.end]
                .iter()
                .any(|s| s.is_extrusion() && (s.end[2] - 0.3).abs() < 1e-5)
        );
        // Everything before ;TYPE: in a Cura file is custom.
        assert_eq!(tp.segments[0].feature, Feature::Custom);
        assert!(
            tp.stats
                .features
                .iter()
                .any(|f| f.feature == Feature::ExternalPerimeter)
        );
        for l in &tp.layers[1..] {
            assert!((l.height - 0.2).abs() < 0.01, "{}", l.height);
        }
    }

    #[test]
    fn features_from_annotations() {
        assert_eq!(
            feature_from_name("External perimeter"),
            Feature::ExternalPerimeter
        );
        assert_eq!(feature_from_name(" Outer wall"), Feature::ExternalPerimeter);
        assert_eq!(feature_from_name("WALL-INNER"), Feature::Perimeter);
        assert_eq!(
            feature_from_name("Overhang wall"),
            Feature::OverhangPerimeter
        );
        assert_eq!(feature_from_name("Sparse infill"), Feature::InternalInfill);
        assert_eq!(feature_from_name("FILL"), Feature::InternalInfill);
        assert_eq!(feature_from_name("SKIN"), Feature::SolidInfill);
        assert_eq!(feature_from_name("Bottom surface"), Feature::SolidInfill);
        assert_eq!(feature_from_name("Top surface"), Feature::TopSolidInfill);
        assert_eq!(feature_from_name("Bridge"), Feature::BridgeInfill);
        assert_eq!(feature_from_name("Gap infill"), Feature::GapFill);
        assert_eq!(feature_from_name("Skirt/Brim"), Feature::Skirt);
        assert_eq!(feature_from_name("Brim"), Feature::Skirt);
        assert_eq!(feature_from_name("Support material"), Feature::Support);
        assert_eq!(
            feature_from_name("SUPPORT-INTERFACE"),
            Feature::SupportInterface
        );
        assert_eq!(
            feature_from_name("dense support"),
            Feature::SupportInterface
        );
        assert_eq!(feature_from_name("Ironing"), Feature::Ironing);
        assert_eq!(feature_from_name("Prime tower"), Feature::WipeTower);
        assert_eq!(feature_from_name("PRIME-TOWER"), Feature::WipeTower);
        assert_eq!(feature_from_name("prime pillar"), Feature::WipeTower);
        assert_eq!(feature_from_name("Custom"), Feature::Custom);
        assert_eq!(feature_from_name("Mystery"), Feature::Unknown);

        let tp = run(
            "M83\nG1 Z0.2\n;TYPE:Skirt\nG1 X10 E1\n; FEATURE: Inner wall\nG1 X20 E1\n; feature solid layer\nG1 X30 E1\n",
        );
        let f: Vec<_> = tp
            .segments
            .iter()
            .filter(|s| s.is_extrusion())
            .map(|s| s.feature)
            .collect();
        assert_eq!(
            f,
            vec![Feature::Skirt, Feature::Perimeter, Feature::SolidInfill]
        );
        // Simplify3D tool line gives width and height.
        let tp = run("; tool H0.300 W0.600\nG1 Z0.3\nG1 X10 E1\n");
        let s = tp.segments.iter().find(|s| s.is_extrusion());
        assert_eq!(s.map(|s| (s.width, s.height)), Some((0.6, 0.3)));
    }

    #[test]
    fn arcs_ij_and_r() {
        // Full CCW circle of radius 10 (I/J form, start == end).
        let tp = run("G1 X10 Y0 Z0.2\nG3 X10 Y0 I-10 J0 E3\n");
        let arc: Vec<_> = tp.segments.iter().filter(|s| s.is_extrusion()).collect();
        assert!(arc.len() >= 16);
        let length: f32 = arc.iter().map(|s| s.length()).sum();
        let circumference = (TAU * 10.0) as f32;
        assert!(length > circumference * 0.995 && length <= circumference + 1e-3);
        // Chord error within tolerance: midpoints stay within 0.02 of r.
        for s in &arc {
            let mx = (s.start[0] + s.end[0]) * 0.5;
            let my = (s.start[1] + s.end[1]) * 0.5;
            let r = (mx * mx + my * my).sqrt();
            assert!(10.0 - r <= 0.0201, "{r}");
        }
        let e: f32 = arc.iter().map(|s| s.extruded).sum();
        assert!((e - 3.0).abs() < 1e-4);
        assert!(tp.stats.uses_arcs);
        // Half circle in R form: from (0,0) to (20,0), radius 10, clockwise
        // goes through y > 0.
        let tp = run("G1 X0 Y0\nG2 X20 Y0 R10\n");
        let max_y = tp
            .segments
            .iter()
            .map(|s| s.end[1])
            .fold(f32::MIN, f32::max);
        assert!((max_y - 10.0).abs() < 0.05, "{max_y}");
        // Negative R: the long way round (> 180 degrees).
        let tp = run("G1 X0 Y0\nG2 X10 Y10 R-10\n");
        let length: f32 = tp.segments.iter().map(|s| s.length()).sum();
        assert!(
            (length - (0.75 * TAU * 10.0) as f32).abs() < 0.2,
            "{length}"
        );
    }

    #[test]
    fn helix_with_turns() {
        // Two extra turns plus a full circle, rising 3 mm.
        let tp = run("G1 X5 Y0 Z0\nG2 X5 Y0 I-5 J0 Z3 P2 E9\n");
        let arc: Vec<_> = tp.segments.iter().skip(1).collect();
        let length: f32 = arc.iter().map(|s| s.length()).sum();
        let expected = ((3.0 * TAU * 5.0).powi(2) + 9.0f64).sqrt() as f32;
        assert!(
            (length - expected).abs() < expected * 0.01,
            "{length} vs {expected}"
        );
        let last = arc.last().map(|s| s.end).unwrap_or_default();
        assert_eq!(last, [5.0, 0.0, 3.0]);
        let e: f32 = arc.iter().map(|s| s.extruded).sum();
        assert!((e - 9.0).abs() < 1e-3);
        // Rising extrusions without markers are bucketed, not one layer per
        // segment.
        assert!(tp.layers.len() < 20, "{}", tp.layers.len());
    }

    #[test]
    fn degenerate_arcs_fall_back_to_lines() {
        let tp = run("G1 X1 Y1\nG2 X5 Y5\nG2 X6 Y6 I0 J0\nG3 X7 Y7 R0\nG2 X8 Y8 I1e300\n");
        assert!(tp.segments.len() >= 4);
        assert!(
            tp.segments
                .iter()
                .all(|s| s.start.iter().chain(&s.end).all(|v| v.is_finite()))
        );
        let tp = run("G2 I1 P99999999 E1\n");
        assert!(tp.segments.len() <= MAX_ARC_SEGMENTS);
    }

    #[test]
    fn firmware_retraction() {
        let tp = run("M83\nM207 S2 F1800\nG1 Z0.2\nG1 X10 E1\nG10\nG10\nG1 X20\nG11\nG1 X30 E1\n");
        assert!(tp.stats.uses_firmware_retraction);
        assert_eq!(tp.stats.retractions, 1);
        let amounts: Vec<f32> = tp.retractions.iter().map(|r| r.amount).collect();
        assert_eq!(amounts, vec![-2.0, 2.0]);
        assert!(tp.stats.other_time > 0.0);
    }

    #[test]
    fn timing_is_consistent() {
        let mut g =
            String::from("M201 X1000 Y1000\nM204 P1000 T1000 R1000\nG28\nG1 Z0.2 F600\nM83\n");
        for i in 0..50 {
            let y = f64::from(i) * 0.5;
            g.push_str(&format!(";TYPE:Perimeter\nG1 X0 Y{y} F6000\nG1 X50 E2 F1800\nG1 E-0.8 F2400\nG4 P100\nG1 E0.8\n"));
        }
        let tp = run(&g);
        let st = &tp.stats;
        let seg_time: f64 = tp.segments.iter().map(|s| f64::from(s.duration)).sum();
        assert!(((seg_time + f64::from(st.other_time)) as f32 - st.estimated_time).abs() < 1e-3);
        let feature_time: f32 = st.features.iter().map(|f| f.time).sum();
        assert!(approx(
            feature_time + st.travel_time + st.other_time,
            st.estimated_time,
            1e-4
        ));
        let feature_e: f32 = st.features.iter().map(|f| f.extruded).sum();
        assert!(approx(feature_e, st.filament_length, 1e-5));
        assert!(approx(st.filament_length, 100.0, 1e-4));
        let per_tool: f32 = st.filament_per_tool.iter().sum();
        assert!(approx(per_tool, st.filament_length, 1e-6));
        let feature_d: f32 = st.features.iter().map(|f| f.distance).sum();
        assert!(approx(feature_d, st.extrude_distance, 1e-5));
        let feature_n: usize = st.features.iter().map(|f| f.segments).sum();
        assert_eq!(feature_n, st.extrude_moves);
        assert_eq!(st.extrude_moves + st.travel_moves, tp.segments.len());
        // 50 dwells of 0.1 s.
        assert!(st.other_time >= 5.0);
        // Segment times are monotonic and continuous.
        for w in tp.segments.windows(2) {
            assert!(w[1].time + 1e-3 >= w[0].time + w[0].duration);
        }
        let layer_time: f32 = tp.layers.iter().map(|l| l.duration).sum();
        assert!(approx(layer_time, st.estimated_time, 1e-4));
        assert!(approx(
            tp.time_at_line(tp.line_count),
            st.estimated_time - 0.0,
            0.05
        ));
        let w = st.filament_volume / 1000.0 * 1.24;
        assert!(approx(st.filament_weight, w, 1e-5));
        assert_eq!(st.max_print_speed, 30.0);
        assert_eq!(st.min_print_speed, 30.0);
        assert!((st.avg_print_speed - 30.0).abs() < 1e-3);
        assert_eq!(st.max_travel_speed, 100.0);
    }

    #[test]
    fn dwell_and_heat_waits() {
        let tp = run("M190 S60\nM109 S200\nG4 S2\nG4 P500\nG1 X10 F600\n");
        assert_eq!(tp.stats.heat_waits, 2);
        assert!((tp.segments[0].time - 2.5).abs() < 1e-6);
        assert_eq!(tp.stats.hotend_temps, vec![(0, 200.0)]);
        assert_eq!(tp.stats.bed_temps, vec![60.0]);
    }

    #[test]
    fn file_limits_change_the_estimate() {
        let slow = run("M204 T100\nG1 X100 F6000\n");
        let fast = run("M204 T10000\nM203 X1000\nG1 X100 F6000\n");
        assert!(slow.stats.estimated_time > fast.stats.estimated_time * 1.5);
        // M220 speed factor.
        let half = run("M220 S50\nM204 T10000\nM203 X1000\nG1 X100 F6000\n");
        assert!(half.stats.estimated_time > fast.stats.estimated_time * 1.8);
    }

    #[test]
    fn tools_and_temps() {
        let tp = run(
            "M83\nM104 T0 S200\nM104 T1 S210\nT0\nG1 Z0.2\nG1 X10 E1\nT1\nG1 X20 E2\nT1\nT0\nG1 X30 E1\n",
        );
        assert_eq!(tp.stats.tool_changes, 2);
        assert_eq!(tp.stats.tools_used, vec![0, 1]);
        assert_eq!(tp.stats.filament_per_tool, vec![2.0, 2.0]);
        assert_eq!(tp.stats.hotend_temps, vec![(0, 200.0), (1, 210.0)]);
        assert_eq!(tp.segments.iter().filter(|s| s.tool == 1).count(), 1);
    }

    #[test]
    fn fan_and_relative_modes() {
        let tp = run("G91\nG1 Z0.2\nM106 S128\nG1 X10 E1\nG90\nM107\nG1 X20 E2\n");
        let ext: Vec<_> = tp.segments.iter().filter(|s| s.is_extrusion()).collect();
        assert_eq!(ext[0].fan, 128);
        assert_eq!(ext[1].fan, 0);
        assert_eq!(ext[0].end, [10.0, 0.0, 0.2]);
        assert_eq!(ext[1].end, [20.0, 0.0, 0.2]);
        assert!((ext[1].extruded - 1.0).abs() < 1e-6);
    }

    #[test]
    fn g92_offsets_use_machine_coordinates() {
        let tp = run("G1 X10 Y10\nG92 X0 Y0\nG1 X5 Y0\n");
        assert_eq!(tp.segments[1].end, [15.0, 10.0, 0.0]);
        let tp = run("M82\nG1 Z0.2\nG1 X10 E5\nG92 E0\nG1 X20 E1\n");
        let e: Vec<f32> = tp
            .segments
            .iter()
            .filter(|s| s.is_extrusion())
            .map(|s| s.extruded)
            .collect();
        assert_eq!(e, vec![5.0, 1.0]);
    }

    #[test]
    fn inches_are_converted() {
        let tp = run("G20\nG1 X1 Y0 F60\n");
        assert!((tp.segments[0].end[0] - 25.4).abs() < 1e-5);
        assert!((tp.segments[0].feedrate - 25.4).abs() < 1e-4);
    }

    #[test]
    fn progress_and_cancel() {
        let text = "G1 X1 E1\n".repeat(1000);
        let file = GcodeFile::from_text("p", text);
        let mut calls = Vec::new();
        let tp = analyze(&file, &AnalyzeOptions::default(), &mut |p| {
            calls.push(p);
            true
        });
        assert!(tp.is_some());
        assert!(calls.len() >= 50 && calls.len() <= 110, "{}", calls.len());
        assert_eq!(calls.first(), Some(&0.0));
        assert_eq!(calls.last(), Some(&1.0));
        assert!(calls.windows(2).all(|w| w[0] <= w[1]));
        let mut n = 0;
        let cancelled = analyze(&file, &AnalyzeOptions::default(), &mut |_| {
            n += 1;
            n < 10
        });
        assert!(cancelled.is_none());
        assert_eq!(n, 10);
    }

    #[test]
    fn travel_only_file_has_one_layer() {
        let tp = run("G28\nG1 X10 Y10 Z5\nG1 X20\n");
        assert_eq!(tp.layers.len(), 1);
        assert_eq!(tp.layers[0].segments, 0..tp.segments.len());
        assert!(tp.bounds.is_empty());
        assert!(!tp.travel_bounds.is_empty());
    }

    #[test]
    fn klipper_commands() {
        let slow = run("SET_VELOCITY_LIMIT ACCEL=100\nG1 X100 F6000\n");
        let fast = run("SET_VELOCITY_LIMIT ACCEL=10000\nG1 X100 F6000\n");
        assert!(slow.stats.estimated_time > fast.stats.estimated_time);
        let tp = run("SET_RETRACTION RETRACT_LENGTH=0.5\nG10\n");
        assert_eq!(tp.retractions.first().map(|r| r.amount), Some(-0.5));
    }

    #[test]
    fn most_common_height() {
        assert_eq!(most_common(&[]), 0.0);
        assert_eq!(most_common(&[0.3, 0.2, 0.2, 0.1]), 0.2);
        assert_eq!(most_common(&[0.3, 0.2]), 0.2);
    }
}
