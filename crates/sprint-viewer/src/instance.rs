//! GPU instance data packed from a [`Toolpath`], built off the UI thread.
//!
//! Each segment becomes one 48-byte [`SegmentInstance`]. Extrusions and
//! travels go into separate buffers in layer order, so every layer maps to a
//! contiguous instance sub-range of each ([`LayerRanges`]). Inside a big
//! layer, extrusions are sorted along a Z-order curve and cut into spatially
//! coherent [`Cluster`]s; runs of small consecutive layers share clusters.
//! The renderer culls clusters against the view frustum and draws them
//! either as prisms or as cheap ribbons depending on their size on screen.
//! Nothing depends on the order inside a layer: line limits,
//! progress and highlights are evaluated per instance from its source line.
//! Retractions get their own 24-byte [`RetractionInstance`].

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use bytemuck::{Pod, Zeroable};
use sprint_gcode::{Feature, MoveKind, Segment, Toolpath};

use crate::color::{FeatureTally, LegendData, ScalarRange, ScalarRanges};

/// Rendered width used when a segment reports an unusable width (mm).
pub const DEFAULT_WIDTH: f32 = 0.45;
/// Rendered height used when a segment reports an unusable height (mm).
pub const DEFAULT_HEIGHT: f32 = 0.2;
/// Widths are clamped to this range when rendering (mm).
pub const WIDTH_LIMITS: (f32, f32) = (0.02, 5.0);
/// Heights are clamped to this range when rendering (mm).
pub const HEIGHT_LIMITS: (f32, f32) = (0.01, 3.0);
/// Coordinates with a larger magnitude are treated as garbage (mm).
pub const MAX_COORD: f32 = 1.0e6;

/// Bit layout of [`SegmentInstance::attrs`].
pub mod attrs {
    /// Feature index, bits 0..5.
    pub const FEATURE_MASK: u32 = 0x1F;
    /// Set for travel moves.
    pub const TRAVEL_BIT: u32 = 1 << 5;
    /// Tool index shift (8 bits).
    pub const TOOL_SHIFT: u32 = 8;
    /// Fan speed shift (8 bits, 0..=255).
    pub const FAN_SHIFT: u32 = 16;
}

/// Per-segment GPU instance (48 bytes, `#[repr(C)]`, matches the WGSL
/// vertex layout).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct SegmentInstance {
    /// Start position (mm).
    pub start: [f32; 3],
    /// Rendered extrusion width (mm).
    pub width: f32,
    /// End position (mm).
    pub end: [f32; 3],
    /// Rendered layer height (mm).
    pub height: f32,
    /// Source line index.
    pub line: u32,
    /// Layer index.
    pub layer: u32,
    /// Feature (bits 0..5), travel flag (bit 5), tool (bits 8..16), fan
    /// (bits 16..24). See [`attrs`].
    pub attrs: u32,
    /// Speed in mm/s as an IEEE half float (low 16 bits) and the segment
    /// start time as a fraction of the total time in 1/65535 units (high 16
    /// bits).
    pub scalars: u32,
}

const _: () = assert!(std::mem::size_of::<SegmentInstance>() == 48);
const _: () = assert!(std::mem::size_of::<RetractionInstance>() == 24);

impl SegmentInstance {
    /// Size of one instance in bytes.
    pub const SIZE: usize = std::mem::size_of::<SegmentInstance>();

    /// Pack a segment. `time_scale` converts seconds into a fraction of the
    /// whole print (`1 / total_time`, or 0 when unknown); `fallback_time` is
    /// used for that fraction when timing is unknown. Returns `None` for
    /// segments with non-finite or absurd coordinates.
    pub fn pack(seg: &Segment, time_scale: f32, fallback_time: f32) -> Option<SegmentInstance> {
        if !valid_point(seg.start) || !valid_point(seg.end) {
            return None;
        }
        let travel = seg.kind == MoveKind::Travel;
        let (width, height) = if travel {
            (0.0, 0.0)
        } else {
            (render_width(seg.width), render_height(seg.height))
        };
        let feature = (seg.feature.index() as u32) & attrs::FEATURE_MASK;
        let attrs = feature
            | if travel { attrs::TRAVEL_BIT } else { 0 }
            | ((seg.tool as u32) << attrs::TOOL_SHIFT)
            | ((seg.fan as u32) << attrs::FAN_SHIFT);
        let fraction = if time_scale > 0.0 && seg.time.is_finite() {
            seg.time * time_scale
        } else {
            fallback_time
        };
        let speed = if seg.feedrate.is_finite() {
            seg.feedrate.max(0.0)
        } else {
            0.0
        };
        let scalars = f32_to_f16(speed) as u32 | ((unit_to_u16(fraction) as u32) << 16);
        Some(SegmentInstance {
            start: seg.start,
            width,
            end: seg.end,
            height,
            line: seg.line,
            layer: seg.layer,
            attrs,
            scalars,
        })
    }

    /// Feature encoded in the attributes.
    pub fn feature(&self) -> Feature {
        Feature::from_index((self.attrs & attrs::FEATURE_MASK) as usize).unwrap_or_default()
    }

    /// True for travel instances.
    pub fn is_travel(&self) -> bool {
        self.attrs & attrs::TRAVEL_BIT != 0
    }

    /// Tool index.
    pub fn tool(&self) -> u8 {
        (self.attrs >> attrs::TOOL_SHIFT) as u8
    }

    /// Fan speed 0..=255.
    pub fn fan(&self) -> u8 {
        (self.attrs >> attrs::FAN_SHIFT) as u8
    }

    /// Speed in mm/s (half precision).
    pub fn speed(&self) -> f32 {
        f16_to_f32(self.scalars as u16)
    }

    /// Start time as a fraction of the print, 0..=1.
    pub fn time_fraction(&self) -> f32 {
        (self.scalars >> 16) as f32 / 65535.0
    }
}

/// Retraction / prime marker instance (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct RetractionInstance {
    /// Nozzle position (mm).
    pub pos: [f32; 3],
    /// Bit 0: prime/unretract (else retract); bits 8..16: tool.
    pub attrs: u32,
    /// Layer index.
    pub layer: u32,
    /// Source line index.
    pub line: u32,
}

impl RetractionInstance {
    /// Size of one instance in bytes.
    pub const SIZE: usize = std::mem::size_of::<RetractionInstance>();
    /// Attribute bit set for primes / unretractions.
    pub const PRIME_BIT: u32 = 1;

    /// True for primes / unretractions.
    pub fn is_prime(&self) -> bool {
        self.attrs & Self::PRIME_BIT != 0
    }
}

/// Extrusion instances per cluster.
pub const CLUSTER_SIZE: usize = 2048;

/// Instance sub-ranges of one layer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerRanges {
    /// Range into [`SceneData::extrusions`].
    pub extrusions: Range<u32>,
    /// Range into [`SceneData::travels`].
    pub travels: Range<u32>,
    /// Range into [`SceneData::clusters`] covering `extrusions` (a cluster
    /// may also hold instances of neighbouring small layers, see
    /// [`Cluster::layers`]).
    pub clusters: Range<u32>,
}

/// A run of spatially close extrusion instances: part of one big layer, or
/// several consecutive small layers (a spiral vase has one layer per
/// segment) so the per-frame culling work stays proportional to the number
/// of instances / [`CLUSTER_SIZE`], not to the number of layers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cluster {
    /// Range into [`SceneData::extrusions`].
    pub instances: Range<u32>,
    /// Indices of the layers whose instances the cluster holds. A cluster
    /// spans more than one layer only when the layers' instance ranges are
    /// ordered and disjoint, so a layer sub-range of a cluster is a
    /// contiguous instance sub-range.
    pub layers: Range<u32>,
    /// Minimum corner of the beads' bounding box (mm).
    pub min: [f32; 3],
    /// Maximum corner of the beads' bounding box (mm).
    pub max: [f32; 3],
    /// Widest rendered bead in the cluster (mm).
    pub max_width: f32,
}

/// Everything the viewer derives from a toolpath on the CPU.
#[derive(Clone, Debug, Default)]
pub struct SceneData {
    /// Extrusion instances in file order.
    pub extrusions: Vec<SegmentInstance>,
    /// Travel instances in file order.
    pub travels: Vec<SegmentInstance>,
    /// Retraction markers.
    pub retractions: Vec<RetractionInstance>,
    /// Per-layer instance ranges (one entry per toolpath layer, or a single
    /// synthetic layer when the toolpath has segments but no layers).
    pub layers: Vec<LayerRanges>,
    /// Spatial clusters of extrusion instances, in layer order.
    pub clusters: Vec<Cluster>,
    /// Legend inputs (feature/tool tallies and scalar ranges).
    pub legend: LegendData,
    /// Bounds of the extrusions as rendered, ignoring start/end G-code
    /// (`Feature::Custom`, e.g. purge lines) unless nothing else is printed.
    /// `None` when there are no extrusions.
    pub bounds: Option<([f32; 3], [f32; 3])>,
}

/// Accumulates the box around rendered beads.
#[derive(Clone, Copy)]
struct BeadBox {
    min: [f32; 3],
    max: [f32; 3],
}

impl Default for BeadBox {
    fn default() -> Self {
        BeadBox {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
        }
    }
}

impl BeadBox {
    fn include(&mut self, inst: &SegmentInstance) {
        let hw = inst.width * 0.5;
        for k in 0..3 {
            let (lo, hi) = (
                inst.start[k].min(inst.end[k]),
                inst.start[k].max(inst.end[k]),
            );
            let (pad_lo, pad_hi) = if k == 2 { (inst.height, 0.0) } else { (hw, hw) };
            self.min[k] = self.min[k].min(lo - pad_lo);
            self.max[k] = self.max[k].max(hi + pad_hi);
        }
    }

    fn get(&self) -> Option<([f32; 3], [f32; 3])> {
        (self.min[0] <= self.max[0]).then_some((self.min, self.max))
    }
}

/// Clamp a reported width to something drawable.
pub fn render_width(w: f32) -> f32 {
    if w.is_finite() && w > 0.0 {
        w.clamp(WIDTH_LIMITS.0, WIDTH_LIMITS.1)
    } else {
        DEFAULT_WIDTH
    }
}

/// Clamp a reported height to something drawable.
pub fn render_height(h: f32) -> f32 {
    if h.is_finite() && h > 0.0 {
        h.clamp(HEIGHT_LIMITS.0, HEIGHT_LIMITS.1)
    } else {
        DEFAULT_HEIGHT
    }
}

fn valid_point(p: [f32; 3]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORD)
}

fn unit_to_u16(v: f32) -> u16 {
    if v.is_finite() {
        (v.clamp(0.0, 1.0) * 65535.0).round() as u16
    } else {
        0
    }
}

/// Layer segment ranges clamped to the segment array. A toolpath with
/// segments but no layers gets one synthetic layer covering everything.
pub fn layer_segment_ranges(toolpath: &Toolpath) -> Vec<Range<usize>> {
    let n = toolpath.segments.len();
    if toolpath.layers.is_empty() {
        return if n == 0 {
            Vec::new()
        } else {
            std::iter::once(0..n).collect()
        };
    }
    toolpath
        .layers
        .iter()
        .map(|l| {
            let start = l.segments.start.min(n);
            let end = l.segments.end.clamp(start, n);
            start..end
        })
        .collect()
}

/// Total print time used to normalize the time color mode: the estimate in
/// the stats, else the end of the last timed segment, else 0.
pub fn total_time(toolpath: &Toolpath) -> f32 {
    let est = toolpath.stats.estimated_time;
    if est.is_finite() && est > 0.0 {
        return est;
    }
    toolpath
        .segments
        .iter()
        .map(|s| s.time + s.duration)
        .filter(|t| t.is_finite())
        .fold(0.0f32, f32::max)
}

/// Build all CPU-side scene data. Returns `None` if `cancel` was set while
/// building. Never panics on malformed toolpaths.
pub fn build_scene(toolpath: &Toolpath, cancel: &AtomicBool) -> Option<SceneData> {
    const CHECK_EVERY: usize = 1 << 16;
    let segs = &toolpath.segments;
    let n = segs.len();
    let total = total_time(toolpath);
    let time_scale = if total > 0.0 { 1.0 / total } else { 0.0 };
    let inv_n = if n > 1 { 1.0 / (n - 1) as f32 } else { 0.0 };

    let ext_count = segs.iter().filter(|s| s.is_extrusion()).count();
    let mut extrusions = Vec::with_capacity(ext_count);
    let mut travels = Vec::with_capacity(n - ext_count);
    // Prefix counts at every segment index: instance index of the first
    // extrusion / travel at or after that segment.
    let mut ext_prefix = Vec::with_capacity(n + 1);
    let mut travel_prefix = Vec::with_capacity(n + 1);

    let mut ranges = ScalarRanges::default();
    let mut features = [FeatureTally::default(); Feature::COUNT];
    let mut tools = [0usize; 256];
    let mut object_box = BeadBox::default();
    let mut custom_box = BeadBox::default();

    for (i, seg) in segs.iter().enumerate() {
        if i % CHECK_EVERY == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        ext_prefix.push(extrusions.len() as u32);
        travel_prefix.push(travels.len() as u32);
        let Some(inst) = SegmentInstance::pack(seg, time_scale, i as f32 * inv_n) else {
            continue;
        };
        if inst.is_travel() {
            travels.push(inst);
            continue;
        }
        ranges.speed.include(seg.feedrate);
        ranges.width.include(inst.width);
        ranges.height.include(inst.height);
        ranges.fan.include(seg.fan as f32);
        let tally = &mut features[seg.feature.index().min(Feature::COUNT - 1)];
        tally.segments += 1;
        if seg.duration.is_finite() && seg.duration > 0.0 {
            tally.time += seg.duration;
        }
        tally.distance += seg.length();
        tools[seg.tool as usize] += 1;
        if seg.feature == Feature::Custom {
            custom_box.include(&inst);
        } else {
            object_box.include(&inst);
        }
        extrusions.push(inst);
    }
    ext_prefix.push(extrusions.len() as u32);
    travel_prefix.push(travels.len() as u32);

    let mut layers: Vec<LayerRanges> = layer_segment_ranges(toolpath)
        .into_iter()
        .map(|r| LayerRanges {
            extrusions: ext_prefix[r.start]..ext_prefix[r.end],
            travels: travel_prefix[r.start]..travel_prefix[r.end],
            clusters: 0..0,
        })
        .collect();
    drop(ext_prefix);
    drop(travel_prefix);
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let clusters = cluster_layers(&mut extrusions, &mut layers, cancel)?;

    let retractions = toolpath
        .retractions
        .iter()
        .filter(|r| valid_point(r.pos) && r.amount.is_finite())
        .map(|r| RetractionInstance {
            pos: r.pos,
            attrs: if r.amount > 0.0 {
                RetractionInstance::PRIME_BIT
            } else {
                0
            } | ((r.tool as u32) << 8),
            layer: r.layer,
            line: r.line,
        })
        .collect();

    ranges.layer_count = layers.len();
    ranges.total_time = total;

    let stat_times = (!toolpath.stats.features.is_empty()).then(|| {
        let mut t = [0.0f32; Feature::COUNT];
        for fs in &toolpath.stats.features {
            if let Some(slot) = t.get_mut(fs.feature.index())
                && fs.time.is_finite()
                && fs.time > 0.0
            {
                *slot += fs.time;
            }
        }
        t
    });
    let tools = tools
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(t, n)| (t as u8, *n))
        .collect();

    let bounds = object_box.get().or(custom_box.get());
    Some(SceneData {
        extrusions,
        travels,
        retractions,
        layers,
        clusters,
        legend: LegendData {
            features,
            stat_times,
            tools,
            ranges,
        },
        bounds,
    })
}

/// Cut the extrusions into [`Cluster`]s of at most [`CLUSTER_SIZE`]
/// instances. When the layer ranges are disjoint and ordered (as they are
/// for well-formed toolpaths), big layers are first sorted along a Z-order
/// curve, and runs of consecutive small layers share clusters.
fn cluster_layers(
    extrusions: &mut [SegmentInstance],
    layers: &mut [LayerRanges],
    cancel: &AtomicBool,
) -> Option<Vec<Cluster>> {
    const CHECK_EVERY: usize = 1024;
    let disjoint = layers
        .windows(2)
        .all(|w| w[0].extrusions.end <= w[1].extrusions.start);
    let mut clusters: Vec<Cluster> = Vec::with_capacity(extrusions.len() / CLUSTER_SIZE + 1);
    // Bead box of the last cluster while later small layers may still join it.
    let mut open: Option<BeadBox> = None;
    for (index, layer) in layers.iter_mut().enumerate() {
        if index % CHECK_EVERY == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let start = (layer.extrusions.start as usize).min(extrusions.len());
        let end = (layer.extrusions.end as usize).clamp(start, extrusions.len());
        let first = clusters.len() as u32;
        let Some(slice) = extrusions.get_mut(start..end).filter(|s| !s.is_empty()) else {
            layer.clusters = first..first;
            continue;
        };
        if disjoint && slice.len() <= CLUSTER_SIZE {
            if let (Some(bead_box), Some(last)) = (open.as_mut(), clusters.last_mut())
                && last.instances.end as usize == start
                && (last.instances.end - last.instances.start) as usize + slice.len()
                    <= CLUSTER_SIZE
            {
                for inst in slice.iter() {
                    bead_box.include(inst);
                    last.max_width = last.max_width.max(inst.width);
                }
                (last.min, last.max) = bead_box.get().unwrap_or((last.min, last.max));
                last.instances.end = end as u32;
                last.layers.end = index.saturating_add(1);
                layer.clusters = first.saturating_sub(1)..first;
                continue;
            }
            let (cluster, bead_box) = new_cluster(slice, start, index);
            clusters.push(cluster);
            open = Some(bead_box);
            layer.clusters = first..first + 1;
            continue;
        }
        open = None;
        if disjoint {
            sort_z_order(slice);
        }
        for (k, chunk) in slice.chunks(CLUSTER_SIZE).enumerate() {
            clusters.push(new_cluster(chunk, start + k * CLUSTER_SIZE, index).0);
        }
        layer.clusters = first..clusters.len() as u32;
    }
    Some(clusters)
}

/// A cluster of `chunk` (starting at instance `start`) on layer `layer`, and
/// the box around its beads.
fn new_cluster(chunk: &[SegmentInstance], start: usize, layer: u32) -> (Cluster, BeadBox) {
    let mut bead_box = BeadBox::default();
    let mut max_width = 0.0f32;
    for inst in chunk {
        bead_box.include(inst);
        max_width = max_width.max(inst.width);
    }
    let (min, max) = bead_box.get().unwrap_or(([0.0; 3], [0.0; 3]));
    let s = start as u32;
    let cluster = Cluster {
        instances: s..s + chunk.len() as u32,
        layers: layer..layer.saturating_add(1),
        min,
        max,
        max_width,
    };
    (cluster, bead_box)
}

/// Sort instances by the Morton code of their XY midpoint.
fn sort_z_order(slice: &mut [SegmentInstance]) {
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for inst in slice.iter() {
        for k in 0..2 {
            let m = (inst.start[k] + inst.end[k]) * 0.5;
            lo[k] = lo[k].min(m);
            hi[k] = hi[k].max(m);
        }
    }
    let scale = [
        65535.0 / (hi[0] - lo[0]).max(1e-3),
        65535.0 / (hi[1] - lo[1]).max(1e-3),
    ];
    slice.sort_by_cached_key(|inst| {
        let q = |k: usize| {
            let m = (inst.start[k] + inst.end[k]) * 0.5;
            ((m - lo[k]) * scale[k]).clamp(0.0, 65535.0) as u32
        };
        interleave_bits(q(0)) | (interleave_bits(q(1)) << 1)
    });
}

/// Spread the low 16 bits of `v` to the even bit positions.
fn interleave_bits(v: u32) -> u32 {
    let mut x = v & 0xFFFF;
    x = (x | (x << 8)) & 0x00FF_00FF;
    x = (x | (x << 4)) & 0x0F0F_0F0F;
    x = (x | (x << 2)) & 0x3333_3333;
    x = (x | (x << 1)) & 0x5555_5555;
    x
}

/// Speed range helper used by tests and callers that only need ranges.
pub fn extrusion_speed_range(toolpath: &Toolpath) -> ScalarRange {
    let mut r = ScalarRange::empty();
    for s in toolpath.segments.iter().filter(|s| s.is_extrusion()) {
        r.include(s.feedrate);
    }
    r
}

/// Convert an `f32` to IEEE 754 half-precision bits (round to nearest even;
/// overflow saturates to infinity; NaN stays NaN).
pub fn f32_to_f16(value: f32) -> u16 {
    let x = value.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xFF) as i32;
    let mant = x & 0x007F_FFFF;
    if exp == 0xFF {
        // Inf or NaN.
        return sign | 0x7C00 | if mant != 0 { 0x0200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1F {
        return sign | 0x7C00;
    }
    if e <= 0 {
        // Subnormal half (or zero).
        if e < -10 {
            return sign;
        }
        let m = mant | 0x0080_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        let round = rem > halfway || (rem == halfway && (half & 1) == 1);
        return sign | (half + round as u32) as u16;
    }
    let half = ((e as u32) << 10) | (mant >> 13);
    let rem = mant & 0x1FFF;
    let round = rem > 0x1000 || (rem == 0x1000 && (half & 1) == 1);
    // A carry out of the mantissa correctly bumps the exponent (up to inf).
    sign | (half + round as u32) as u16
}

/// Convert IEEE 754 half-precision bits to `f32`.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1F) as i32;
    let mant = (h & 0x03FF) as f32;
    match exp {
        0 => sign * mant * (2.0f32).powi(-24),
        0x1F => {
            if mant == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + mant / 1024.0) * (2.0f32).powi(exp - 15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_round_trips_exact_values() {
        for v in [0.0f32, 1.0, -2.5, 0.5, 65504.0, 1024.0, 0.000_061_035_156] {
            assert_eq!(f16_to_f32(f32_to_f16(v)), v, "value {v}");
        }
        assert_eq!(f32_to_f16(1.0e6), 0x7C00);
        assert!(f16_to_f32(f32_to_f16(f32::NAN)).is_nan());
        assert_eq!(f16_to_f32(f32_to_f16(f32::NEG_INFINITY)), f32::NEG_INFINITY);
        // Smallest subnormal.
        assert_eq!(f16_to_f32(1), 2.0f32.powi(-24));
        assert_eq!(f32_to_f16(2.0f32.powi(-24)), 1);
        assert_eq!(f32_to_f16(1.0e-10), 0);
    }

    #[test]
    fn f16_relative_error_is_small() {
        let mut v = 0.01f32;
        while v < 60000.0 {
            let back = f16_to_f32(f32_to_f16(v));
            assert!(((back - v) / v).abs() < 1.0e-3, "{v} -> {back}");
            v *= 1.37;
        }
    }

    #[test]
    fn f16_rounds_to_nearest_even() {
        // 1 + 2^-11 is halfway between 1 and the next half (1 + 2^-10).
        assert_eq!(f32_to_f16(1.0 + 2.0f32.powi(-11)), 0x3C00);
        // 1 + 3 * 2^-11 rounds up to 1 + 2^-9 (even mantissa).
        assert_eq!(f32_to_f16(1.0 + 3.0 * 2.0f32.powi(-11)), 0x3C02);
        // Largest finite rounds up to infinity past the halfway point.
        assert_eq!(f32_to_f16(65520.0), 0x7C00);
        assert_eq!(f32_to_f16(65519.0), 0x7BFF);
    }

    #[test]
    fn pack_preserves_attributes() {
        let seg = Segment {
            start: [1.0, 2.0, 0.3],
            end: [11.0, 2.0, 0.3],
            feedrate: 45.0,
            width: 0.42,
            height: 0.2,
            time: 50.0,
            duration: 0.2,
            line: 1234,
            layer: 7,
            kind: MoveKind::Extrude,
            feature: Feature::ExternalPerimeter,
            tool: 3,
            fan: 200,
            ..Default::default()
        };
        let inst = SegmentInstance::pack(&seg, 1.0 / 100.0, 0.0).expect("valid");
        assert_eq!(inst.feature(), Feature::ExternalPerimeter);
        assert_eq!(inst.tool(), 3);
        assert_eq!(inst.fan(), 200);
        assert!(!inst.is_travel());
        assert_eq!(inst.speed(), 45.0);
        assert!((inst.time_fraction() - 0.5).abs() < 1e-4);
        assert_eq!((inst.line, inst.layer), (1234, 7));
        assert_eq!(inst.width, 0.42);
    }

    #[test]
    fn pack_rejects_garbage_and_sanitizes() {
        let mut seg = Segment {
            start: [f32::NAN, 0.0, 0.0],
            kind: MoveKind::Extrude,
            ..Default::default()
        };
        assert!(SegmentInstance::pack(&seg, 0.0, 0.0).is_none());
        seg.start = [0.0, 0.0, 2.0e7];
        assert!(SegmentInstance::pack(&seg, 0.0, 0.0).is_none());
        seg.start = [0.0; 3];
        seg.width = f32::INFINITY;
        seg.height = -1.0;
        seg.feedrate = f32::NAN;
        let inst = SegmentInstance::pack(&seg, 0.0, 0.25).expect("valid positions");
        assert_eq!(inst.width, DEFAULT_WIDTH);
        assert_eq!(inst.height, DEFAULT_HEIGHT);
        assert_eq!(inst.speed(), 0.0);
        assert!((inst.time_fraction() - 0.25).abs() < 1e-4);
        seg.width = 80.0;
        let inst = SegmentInstance::pack(&seg, 0.0, 0.0).expect("valid");
        assert_eq!(inst.width, WIDTH_LIMITS.1);
    }
}
