//! Toolpath model produced by [`crate::analyze`].
//!
//! These types are a shared contract with `sprint-viewer`, `sprint-gui` and
//! `sprint-tui`. Fields may be added (every struct derives `Default`, and
//! consumers construct them with `..Default::default()`), but existing fields
//! must not be renamed, removed or change meaning.

use std::ops::Range;

use serde::Serialize;

/// What a move does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[repr(u8)]
pub enum MoveKind {
    /// Non-extruding move (including Z hops and moves with only retraction).
    #[default]
    Travel = 0,
    /// Move that deposits filament (E increases while XY/Z moves).
    Extrude = 1,
}

/// Print feature a segment belongs to, derived from slicer annotations
/// (`;TYPE:` in PrusaSlicer/SuperSlicer/OrcaSlicer/Cura, `; feature` in
/// Simplify3D, ...). `Unknown` when the file carries no annotations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[repr(u8)]
pub enum Feature {
    #[default]
    Unknown = 0,
    ExternalPerimeter = 1,
    Perimeter = 2,
    OverhangPerimeter = 3,
    InternalInfill = 4,
    SolidInfill = 5,
    TopSolidInfill = 6,
    BridgeInfill = 7,
    GapFill = 8,
    /// Skirt and brim.
    Skirt = 9,
    Support = 10,
    SupportInterface = 11,
    Ironing = 12,
    WipeTower = 13,
    /// Start/end G-code and other custom blocks.
    Custom = 14,
}

impl Feature {
    pub const COUNT: usize = 15;

    pub const ALL: [Feature; Feature::COUNT] = [
        Feature::Unknown,
        Feature::ExternalPerimeter,
        Feature::Perimeter,
        Feature::OverhangPerimeter,
        Feature::InternalInfill,
        Feature::SolidInfill,
        Feature::TopSolidInfill,
        Feature::BridgeInfill,
        Feature::GapFill,
        Feature::Skirt,
        Feature::Support,
        Feature::SupportInterface,
        Feature::Ironing,
        Feature::WipeTower,
        Feature::Custom,
    ];

    /// Stable index in `0..Feature::COUNT` (same as the discriminant).
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(index: usize) -> Option<Feature> {
        Feature::ALL.get(index).copied()
    }

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Feature::Unknown => "Unknown",
            Feature::ExternalPerimeter => "External perimeter",
            Feature::Perimeter => "Perimeter",
            Feature::OverhangPerimeter => "Overhang perimeter",
            Feature::InternalInfill => "Internal infill",
            Feature::SolidInfill => "Solid infill",
            Feature::TopSolidInfill => "Top solid infill",
            Feature::BridgeInfill => "Bridge infill",
            Feature::GapFill => "Gap fill",
            Feature::Skirt => "Skirt / brim",
            Feature::Support => "Support",
            Feature::SupportInterface => "Support interface",
            Feature::Ironing => "Ironing",
            Feature::WipeTower => "Wipe tower",
            Feature::Custom => "Custom",
        }
    }

    /// Default display color (sRGB), close to the PrusaSlicer palette.
    pub fn default_color(self) -> [u8; 3] {
        match self {
            Feature::Unknown => [0xB0, 0xB8, 0xC4],
            Feature::ExternalPerimeter => [0xFF, 0x7D, 0x38],
            Feature::Perimeter => [0xFF, 0xE6, 0x4D],
            Feature::OverhangPerimeter => [0x1F, 0x1F, 0xFF],
            Feature::InternalInfill => [0xB0, 0x30, 0x29],
            Feature::SolidInfill => [0x96, 0x54, 0xCC],
            Feature::TopSolidInfill => [0xF0, 0x40, 0x40],
            Feature::BridgeInfill => [0x4D, 0x80, 0xBA],
            Feature::GapFill => [0xFF, 0xFF, 0xFF],
            Feature::Skirt => [0x00, 0x87, 0x87],
            Feature::Support => [0x00, 0xFF, 0x00],
            Feature::SupportInterface => [0x00, 0x80, 0x00],
            Feature::Ironing => [0xFF, 0x8C, 0x69],
            Feature::WipeTower => [0xB3, 0xE3, 0xAB],
            Feature::Custom => [0x5E, 0xD1, 0xC4],
        }
    }
}

/// One straight move. Arcs (G2/G3) are tessellated into several segments.
///
/// Segments are stored in file order, so `line` is non-decreasing across
/// `Toolpath::segments`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Segment {
    /// Start position in machine coordinates (mm).
    pub start: [f32; 3],
    /// End position in machine coordinates (mm).
    pub end: [f32; 3],
    /// Commanded feedrate in mm/s.
    pub feedrate: f32,
    /// Estimated extrusion width in mm (0 for travel).
    pub width: f32,
    /// Layer height in mm (0 for travel).
    pub height: f32,
    /// Filament length pushed during this segment, in mm (0 for travel).
    pub extruded: f32,
    /// Estimated elapsed print time at the start of this segment, seconds.
    pub time: f32,
    /// Estimated duration of this segment, seconds.
    pub duration: f32,
    /// 0-based index of the source line in the `GcodeFile`.
    pub line: u32,
    /// Index into `Toolpath::layers`.
    pub layer: u32,
    pub kind: MoveKind,
    pub feature: Feature,
    /// Active tool (extruder) index.
    pub tool: u8,
    /// Part-cooling fan speed, 0..=255.
    pub fan: u8,
}

impl Segment {
    pub fn length(&self) -> f32 {
        let d = [
            self.end[0] - self.start[0],
            self.end[1] - self.start[1],
            self.end[2] - self.start[2],
        ];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }

    pub fn is_extrusion(&self) -> bool {
        self.kind == MoveKind::Extrude
    }
}

/// An E-only move (retract or prime), shown as a marker by viewers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Retraction {
    pub pos: [f32; 3],
    /// Filament length in mm; negative = retract, positive = unretract/prime.
    pub amount: f32,
    pub line: u32,
    pub layer: u32,
    pub tool: u8,
}

/// A printed layer: a run of segments sharing one extrusion Z.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layer {
    /// Z of the extrusions on this layer (mm).
    pub z: f32,
    /// Layer height (mm): z minus the previous layer's z (first layer: z).
    pub height: f32,
    /// Range into `Toolpath::segments` (travels before the first extrusion
    /// of the layer belong to this layer).
    pub segments: Range<usize>,
    /// Range of source line indices covered by this layer.
    pub lines: Range<u32>,
    /// Estimated elapsed time when this layer starts, seconds.
    pub time_start: f32,
    /// Estimated duration of this layer, seconds.
    pub duration: f32,
    /// Filament used on this layer, mm.
    pub extruded: f32,
}

/// Axis-aligned bounding box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds::empty()
    }
}

impl Bounds {
    pub fn empty() -> Bounds {
        Bounds {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
        }
    }

    pub fn is_empty(&self) -> bool {
        (0..3).any(|i| self.min[i] > self.max[i])
    }

    pub fn include(&mut self, p: [f32; 3]) {
        for ((lo, hi), v) in self.min.iter_mut().zip(self.max.iter_mut()).zip(p) {
            *lo = lo.min(v);
            *hi = hi.max(v);
        }
    }

    pub fn size(&self) -> [f32; 3] {
        if self.is_empty() {
            return [0.0; 3];
        }
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }

    pub fn center(&self) -> [f32; 3] {
        if self.is_empty() {
            return [0.0; 3];
        }
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }
}

/// Per-feature totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct FeatureStat {
    pub feature: Feature,
    /// Estimated seconds spent on this feature.
    pub time: f32,
    /// Filament used, mm.
    pub extruded: f32,
    /// Path length, mm.
    pub distance: f32,
    pub segments: usize,
}

/// Whole-file statistics.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Stats {
    /// Sprint's own kinematic estimate of the print time, seconds.
    pub estimated_time: f32,
    /// Total filament length (all tools), mm.
    pub filament_length: f32,
    /// Filament length per tool index, mm.
    pub filament_per_tool: Vec<f32>,
    /// Filament volume, mm^3.
    pub filament_volume: f32,
    /// Filament weight in grams (using the configured density).
    pub filament_weight: f32,
    pub extrude_moves: usize,
    pub travel_moves: usize,
    pub retractions: usize,
    pub tool_changes: usize,
    /// Total travel path length, mm.
    pub travel_distance: f32,
    /// Total extrusion path length, mm.
    pub extrude_distance: f32,
    /// Fastest / slowest / time-weighted average extrusion speed, mm/s.
    pub max_print_speed: f32,
    pub min_print_speed: f32,
    pub avg_print_speed: f32,
    pub max_travel_speed: f32,
    pub layer_count: usize,
    pub first_layer_height: f32,
    /// Most common layer height, mm.
    pub layer_height: f32,
    /// One entry per feature that occurs in the file.
    pub features: Vec<FeatureStat>,
    pub tools_used: Vec<u8>,
    /// Distinct hotend target temperatures set by the file: (tool, celsius).
    pub hotend_temps: Vec<(u8, f32)>,
    /// Distinct bed target temperatures set by the file.
    pub bed_temps: Vec<f32>,
    pub uses_relative_e: bool,
    pub uses_arcs: bool,
    pub uses_firmware_retraction: bool,
    /// Number of blocking heat waits (M109/M190/M191).
    pub heat_waits: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum ThumbnailFormat {
    #[default]
    Png,
    Jpeg,
    Qoi,
}

/// Preview image embedded by the slicer (`; thumbnail begin WxH len`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub format: ThumbnailFormat,
    /// Decoded (base64-decoded) image file bytes.
    #[serde(skip)]
    pub data: Vec<u8>,
}

/// Information the slicer wrote into comments.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SlicerMetadata {
    /// e.g. "PrusaSlicer 2.7.1", "Cura_SteamEngine 5.6.0", "OrcaSlicer 2.1.0".
    pub slicer: Option<String>,
    /// The slicer's own print time estimate, seconds.
    pub estimated_time: Option<f32>,
    pub filament_used_mm: Option<f32>,
    pub filament_used_g: Option<f32>,
    pub layer_height: Option<f32>,
    pub first_layer_height: Option<f32>,
    pub nozzle_diameter: Option<f32>,
    pub filament_diameter: Option<f32>,
    pub filament_type: Option<String>,
    pub hotend_temp: Option<f32>,
    pub bed_temp: Option<f32>,
    pub object_names: Vec<String>,
    pub thumbnails: Vec<Thumbnail>,
    /// `; key = value` settings block (PrusaSlicer/Orca config dump, Cura
    /// `;SETTING_3` is not decoded). Kept in file order.
    pub settings: Vec<(String, String)>,
}

/// Result of analysing a G-code file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Toolpath {
    pub segments: Vec<Segment>,
    pub layers: Vec<Layer>,
    pub retractions: Vec<Retraction>,
    /// Bounds of extruding moves only.
    pub bounds: Bounds,
    /// Bounds of all moves (including travel).
    pub travel_bounds: Bounds,
    pub stats: Stats,
    pub metadata: SlicerMetadata,
    /// Number of lines in the source file.
    pub line_count: u32,
}

impl Toolpath {
    /// Index of the last segment whose source line is `<= line`.
    pub fn segment_at_line(&self, line: u32) -> Option<usize> {
        let n = self.segments.partition_point(|s| s.line <= line);
        n.checked_sub(1)
    }

    /// Layer containing the given source line (the last layer that starts at
    /// or before it).
    pub fn layer_at_line(&self, line: u32) -> Option<usize> {
        if self.layers.is_empty() {
            return None;
        }
        let n = self.layers.partition_point(|l| l.lines.start <= line);
        Some(n.saturating_sub(1))
    }

    /// Estimated elapsed print time (seconds) once `line` has executed.
    pub fn time_at_line(&self, line: u32) -> f32 {
        match self.segment_at_line(line) {
            Some(i) => {
                let s = &self.segments[i];
                s.time + s.duration
            }
            None => 0.0,
        }
    }

    /// Fraction of estimated print time completed once `line` has executed,
    /// in `0.0..=1.0`.
    pub fn progress_at_line(&self, line: u32) -> f32 {
        let total = self.stats.estimated_time;
        if total <= 0.0 {
            if self.line_count == 0 {
                return 0.0;
            }
            return ((line + 1) as f32 / self.line_count as f32).clamp(0.0, 1.0);
        }
        (self.time_at_line(line) / total).clamp(0.0, 1.0)
    }

    /// Layer whose Z is closest to `z`.
    pub fn layer_at_z(&self, z: f32) -> Option<usize> {
        self.layers
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1.z - z).abs().total_cmp(&(b.1.z - z).abs()))
            .map(|(i, _)| i)
    }
}
