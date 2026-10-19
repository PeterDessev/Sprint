//! Toolpath model produced by [`crate::analyze()`].
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
    /// No slicer annotation.
    #[default]
    Unknown = 0,
    /// Outermost wall.
    ExternalPerimeter = 1,
    /// Inner walls.
    Perimeter = 2,
    /// Walls printed over air.
    OverhangPerimeter = 3,
    /// Sparse infill.
    InternalInfill = 4,
    /// Solid infill (including bottom surfaces).
    SolidInfill = 5,
    /// Top surfaces.
    TopSolidInfill = 6,
    /// Infill bridging over air.
    BridgeInfill = 7,
    /// Thin gaps between walls.
    GapFill = 8,
    /// Skirt and brim.
    Skirt = 9,
    /// Support material.
    Support = 10,
    /// Dense support layers in contact with the part.
    SupportInterface = 11,
    /// Ironing passes over top surfaces.
    Ironing = 12,
    /// Wipe / prime tower.
    WipeTower = 13,
    /// Start/end G-code and other custom blocks.
    Custom = 14,
}

impl Feature {
    /// Number of variants.
    pub const COUNT: usize = 15;

    /// Every variant, in discriminant order.
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

    /// Inverse of [`Feature::index`].
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
/// `Toolpath::segments`. `Default` gives a travel outside any object.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// Travel or extrusion.
    pub kind: MoveKind,
    /// Feature from the slicer annotations in effect.
    pub feature: Feature,
    /// Active tool (extruder) index.
    pub tool: u8,
    /// Part-cooling fan speed, 0..=255.
    pub fan: u8,
    /// Index into `Toolpath::objects` of the labelled object this move
    /// belongs to, or [`Segment::NO_OBJECT`] outside any object (start
    /// G-code, purge lines, skirts, files without labels).
    pub object: u16,
}

impl Default for Segment {
    fn default() -> Self {
        Segment {
            start: [0.0; 3],
            end: [0.0; 3],
            feedrate: 0.0,
            width: 0.0,
            height: 0.0,
            extruded: 0.0,
            time: 0.0,
            duration: 0.0,
            line: 0,
            layer: 0,
            kind: MoveKind::Travel,
            feature: Feature::Unknown,
            tool: 0,
            fan: 0,
            object: Segment::NO_OBJECT,
        }
    }
}

impl Segment {
    /// [`Segment::object`] of moves outside any object.
    pub const NO_OBJECT: u16 = u16::MAX;

    /// Index into `Toolpath::objects`, `None` outside any object.
    pub fn object_index(&self) -> Option<usize> {
        (self.object != Segment::NO_OBJECT).then_some(usize::from(self.object))
    }

    /// Straight-line length in mm.
    pub fn length(&self) -> f32 {
        let d = [
            self.end[0] - self.start[0],
            self.end[1] - self.start[1],
            self.end[2] - self.start[2],
        ];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }

    /// True for extruding segments.
    pub fn is_extrusion(&self) -> bool {
        self.kind == MoveKind::Extrude
    }
}

/// An E-only move (retract or prime), shown as a marker by viewers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Retraction {
    /// Nozzle position (machine coordinates, mm).
    pub pos: [f32; 3],
    /// Filament length in mm; negative = retract, positive = unretract/prime.
    pub amount: f32,
    /// 0-based source line.
    pub line: u32,
    /// Index into `Toolpath::layers`.
    pub layer: u32,
    /// Active tool (extruder) index.
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
    /// Smallest X Y Z (mm); +infinity when empty.
    pub min: [f32; 3],
    /// Largest X Y Z (mm); -infinity when empty.
    pub max: [f32; 3],
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds::empty()
    }
}

impl Bounds {
    /// Bounds containing nothing.
    pub fn empty() -> Bounds {
        Bounds {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
        }
    }

    /// True when no point was included.
    pub fn is_empty(&self) -> bool {
        (0..3).any(|i| self.min[i] > self.max[i])
    }

    /// Grow to contain `p`.
    pub fn include(&mut self, p: [f32; 3]) {
        for ((lo, hi), v) in self.min.iter_mut().zip(self.max.iter_mut()).zip(p) {
            *lo = lo.min(v);
            *hi = hi.max(v);
        }
    }

    /// Extent per axis (zero when empty).
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

    /// Centre point (origin when empty).
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

    /// Grow to contain the deposited bead of `segment`: its path widened by
    /// half the extrusion width in X and Y and reaching one layer height
    /// below it (the bead sits on the layer below). Travel moves have no
    /// width or height, so this includes just their path. Non-finite
    /// coordinates are ignored.
    pub fn include_bead(&mut self, segment: &Segment) {
        let finite = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
        let half_width = finite(segment.width) * 0.5;
        let height = finite(segment.height);
        let (s, e) = (segment.start, segment.end);
        if s.iter().chain(&e).any(|v| !v.is_finite()) {
            return;
        }
        self.include([
            s[0].min(e[0]) - half_width,
            s[1].min(e[1]) - half_width,
            s[2].min(e[2]) - height,
        ]);
        self.include([
            s[0].max(e[0]) + half_width,
            s[1].max(e[1]) + half_width,
            s[2].max(e[2]),
        ]);
    }
}

/// Per-feature totals over extruding segments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct FeatureStat {
    /// The feature.
    pub feature: Feature,
    /// Estimated seconds spent on this feature.
    pub time: f32,
    /// Filament used, mm.
    pub extruded: f32,
    /// Path length, mm.
    pub distance: f32,
    /// Number of extruding segments.
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
    /// Number of extruding segments.
    pub extrude_moves: usize,
    /// Number of travel segments.
    pub travel_moves: usize,
    /// Number of retractions (negative `Toolpath::retractions` entries,
    /// including firmware retraction).
    pub retractions: usize,
    /// Number of tool changes (T commands that switch tools).
    pub tool_changes: usize,
    /// Total travel path length, mm.
    pub travel_distance: f32,
    /// Total extrusion path length, mm.
    pub extrude_distance: f32,
    /// Fastest / slowest / time-weighted average extrusion speed, mm/s.
    pub max_print_speed: f32,
    /// Slowest commanded extrusion speed, mm/s.
    pub min_print_speed: f32,
    /// Time-weighted average commanded extrusion speed, mm/s.
    pub avg_print_speed: f32,
    /// Fastest commanded travel speed, mm/s.
    pub max_travel_speed: f32,
    /// Number of layers.
    pub layer_count: usize,
    /// Height of the first layer, mm.
    pub first_layer_height: f32,
    /// Most common layer height, mm.
    pub layer_height: f32,
    /// One entry per feature that occurs in the file (extruding segments
    /// only; travel is in `travel_*`).
    pub features: Vec<FeatureStat>,
    /// Tools that extruded, ascending.
    pub tools_used: Vec<u8>,
    /// Distinct hotend target temperatures set by the file: (tool, celsius).
    pub hotend_temps: Vec<(u8, f32)>,
    /// Distinct bed target temperatures set by the file.
    pub bed_temps: Vec<f32>,
    /// The file switches the extruder to relative mode (M83).
    pub uses_relative_e: bool,
    /// The file contains G2/G3 arcs.
    pub uses_arcs: bool,
    /// The file uses firmware retraction (G10/G11).
    pub uses_firmware_retraction: bool,
    /// Number of blocking heat waits (M109/M190/M191, RepRapFirmware M116).
    pub heat_waits: usize,
    /// Estimated seconds spent on travel segments.
    pub travel_time: f32,
    /// Estimated seconds not covered by any segment: E-only moves
    /// (retract/prime), firmware retraction and dwells. The feature times
    /// plus `travel_time` plus `other_time` add up to `estimated_time`.
    pub other_time: f32,
}

/// Image format of an embedded thumbnail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum ThumbnailFormat {
    /// `; thumbnail begin` / `; thumbnail_PNG begin`.
    #[default]
    Png,
    /// `; thumbnail_JPG begin`.
    Jpeg,
    /// `; thumbnail_QOI begin` (Quite OK Image).
    Qoi,
}

/// Preview image embedded by the slicer (`; thumbnail begin WxH len`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Thumbnail {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Image format of `data`.
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
    /// Filament length the slicer reported (all extruders), mm.
    pub filament_used_mm: Option<f32>,
    /// Filament weight the slicer reported, grams.
    pub filament_used_g: Option<f32>,
    /// Layer height, mm.
    pub layer_height: Option<f32>,
    /// First layer height, mm.
    pub first_layer_height: Option<f32>,
    /// Nozzle diameter, mm (first extruder).
    pub nozzle_diameter: Option<f32>,
    /// Filament diameter, mm (first extruder).
    pub filament_diameter: Option<f32>,
    /// Filament type such as "PLA" (first extruder).
    pub filament_type: Option<String>,
    /// Hotend temperature, °C: the one the start G-code waits for before the
    /// first layer (else the last one set there, else the first commanded),
    /// or from the settings when the file commands none.
    pub hotend_temp: Option<f32>,
    /// Bed temperature, °C, chosen like `hotend_temp`.
    pub bed_temp: Option<f32>,
    /// Object labels (`EXCLUDE_OBJECT_DEFINE`, `M486`, `; printing object`,
    /// Cura `;MESH:`, Simplify3D `applyToModels`), in first-seen order.
    pub object_names: Vec<String>,
    /// Every embedded thumbnail, in file order.
    pub thumbnails: Vec<Thumbnail>,
    /// `; key = value` settings block (PrusaSlicer/Orca config dump, Cura
    /// `;SETTING_3` is not decoded). Kept in file order.
    pub settings: Vec<(String, String)>,
    /// Filament density in g/cm³ (`filament_density`, Simplify3D
    /// `filamentDensities`, ideaMaker `Filament Density #1`).
    pub filament_density: Option<f32>,
    /// Number of layers reported by the slicer (Cura `;LAYER_COUNT:`,
    /// Orca/Bambu `total layer number`).
    pub layer_count: Option<u32>,
    /// G-code flavor (Cura `;FLAVOR:`, PrusaSlicer/Orca `gcode_flavor`).
    pub flavor: Option<String>,
}

/// A labelled object (part) of a multi-object print, as the slicer marked it
/// for object exclusion / cancel-object.
///
/// Markers: Marlin/Prusa/RepRapFirmware `M486 S<id> [A<name>]` (`S-1` ends
/// the object; a later `M486 A<name>` names the current one), Klipper
/// `EXCLUDE_OBJECT_DEFINE NAME=` / `EXCLUDE_OBJECT_START NAME=` /
/// `EXCLUDE_OBJECT_END`, PrusaSlicer `; printing object <name>` / `; stop
/// printing object <name>`, and Cura `;MESH:<name>` (`;MESH:NONMESH` ends
/// it). An object block runs from its start marker to the next start or end
/// marker.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ObjectInfo {
    /// The label exactly as written, trimmed and unquoted (`M486 A"name"`,
    /// `NAME=`, `; printing object name`, `;MESH:name`). M486 objects
    /// without a name are `"Object <id>"`. Object exclusion in the host uses
    /// the same names.
    pub name: String,
    /// Numeric id of M486 objects (`S<id>`).
    pub id: Option<u32>,
    /// Physical extent of the object's extrusions ([`Bounds::include_bead`];
    /// empty when it has none).
    pub bounds: Bounds,
    /// Source line ranges of the object's blocks that contain moves, from
    /// the start marker up to (not including) the end marker, ascending.
    pub lines: Vec<Range<u32>>,
    /// Estimated seconds spent on the object's segments.
    pub time: f32,
    /// Filament the object uses, mm.
    pub extruded: f32,
    /// Number of segments (extrusions and travels) inside the object.
    pub segments: usize,
}

impl ObjectInfo {
    /// True when source line `line` lies in one of the object's blocks.
    pub fn contains_line(&self, line: u32) -> bool {
        let n = self.lines.partition_point(|r| r.start <= line);
        n.checked_sub(1)
            .and_then(|i| self.lines.get(i))
            .is_some_and(|r| r.contains(&line))
    }
}

/// Result of analysing a G-code file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Toolpath {
    /// Every move, in file order.
    pub segments: Vec<Segment>,
    /// Layers, contiguous ranges of `segments` in file order.
    pub layers: Vec<Layer>,
    /// E-only moves and firmware retractions, in file order.
    pub retractions: Vec<Retraction>,
    /// Bounds of extruding moves only.
    pub bounds: Bounds,
    /// Bounds of all moves (including travel).
    pub travel_bounds: Bounds,
    /// Physical extent of the printed part(s) ([`Bounds::include_bead`] of
    /// the extrusions): without start/end G-code and purge lines
    /// (`Feature::Custom`), skirt/brim and wipe tower, and, when the file
    /// labels objects, without extrusions outside every object. Falls back to
    /// all extrusions when none of them qualifies. The size of the part a
    /// 20 mm calibration cube was sliced from is about 20 x 20 x 20 mm.
    pub part_bounds: Bounds,
    /// Whole-file statistics.
    pub stats: Stats,
    /// Slicer metadata read from comments.
    pub metadata: SlicerMetadata,
    /// Number of lines in the source file.
    pub line_count: u32,
    /// Labelled objects in first-seen order (empty when the file has no
    /// object labels); `Segment::object` indexes this list.
    pub objects: Vec<ObjectInfo>,
}

impl Toolpath {
    /// Index of the object whose block contains source line `line`.
    pub fn object_at_line(&self, line: u32) -> Option<usize> {
        self.objects.iter().position(|o| o.contains_line(line))
    }

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

    /// True when a print can start at layer `layer` by streaming from its
    /// first line (`lines.start`): that line does not continue a command of
    /// the layers below.
    ///
    /// A helical arc (G2/G3 with Z) that climbs through several layers is a
    /// single command, so every layer it reaches after its first one starts
    /// on the arc's own line. Starting there would print the whole helix
    /// again from its bottom, and the resume preamble would lower the nozzle
    /// to the helix's start height, below the part already printed. Layer 0
    /// always starts cleanly; layers out of range never do.
    pub fn layer_starts_cleanly(&self, layer: usize) -> bool {
        let Some(l) = self.layers.get(layer) else {
            return false;
        };
        let Some(before) = l.segments.start.checked_sub(1) else {
            return true;
        };
        self.segments
            .get(before)
            .is_none_or(|s| s.line < l.lines.start)
    }

    /// The first layer at or after `layer` that starts cleanly (see
    /// [`Toolpath::layer_starts_cleanly`]): what a start-from-layer dialog
    /// offers instead of a layer inside a multi-layer command. `None` when
    /// no such layer follows.
    pub fn next_clean_layer(&self, layer: usize) -> Option<usize> {
        (layer..self.layers.len()).find(|&i| self.layer_starts_cleanly(i))
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
