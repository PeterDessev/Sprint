//! CPU picking: ray versus extrusion beads and travel moves.
//!
//! Beads are tested as capsules around their centre line with the vertical
//! axis scaled so the cross-section matches the rendered flattened profile.
//! Travel moves are drawn as one-pixel lines, so they are hit when the ray
//! passes within a (pixel-derived) radius of the nozzle path. A
//! [`PickIndex`] accelerates queries with one bounding box per layer and a
//! uniform XY grid inside layers with many segments, so a pick stays well
//! below a millisecond even on multi-million-segment toolpaths.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use glam::{Vec2, Vec3};
use sprint_gcode::{MoveKind, Segment, Toolpath};

use crate::camera::Ray;
use crate::instance::{layer_segment_ranges, render_height, render_width};

/// Layers with more extrusion segments than this get a grid.
const GRID_THRESHOLD: usize = 192;
/// Desired average number of segments per grid cell.
const TARGET_PER_CELL: usize = 6;
/// Upper bound on grid cells per axis.
const MAX_CELLS_PER_AXIS: usize = 768;
/// Extra radius (mm) baked into the grid and layer boxes so callers may ask
/// for a pixel-based pick tolerance up to this size.
pub const PICK_SLACK: f32 = 0.75;
/// Largest pick radius (mm) around travel moves; baked into the travel
/// index's grid and layer boxes.
pub const TRAVEL_SLACK: f32 = 2.0;

/// Which segments a [`PickIndex`] holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PickKind {
    /// Extrusion beads, tested as flattened capsules.
    #[default]
    Extrusions,
    /// Travel moves, tested by their distance to the ray.
    Travels,
}

impl PickKind {
    /// Whether `seg` belongs to this kind.
    pub fn includes(self, seg: &Segment) -> bool {
        match self {
            PickKind::Extrusions => seg.is_extrusion(),
            PickKind::Travels => seg.kind == MoveKind::Travel,
        }
    }

    /// The segment's pick axis and the half extents of the box around it
    /// that covers every allowed pick radius: `(a, b, radius_xy, radius_z)`.
    /// `None` for segments of another kind or with non-finite positions.
    fn footprint(self, seg: &Segment) -> Option<(Vec3, Vec3, f32, f32)> {
        if !self.includes(seg) {
            return None;
        }
        let (a, b, r, rz) = match self {
            PickKind::Extrusions => {
                let (hw, hh) = bead_radii(seg);
                let (a, b) = bead_axis(seg, hh);
                // With a pixel tolerance the vertical radius can grow up to
                // the horizontal one (see `ray_segment`).
                (a, b, hw + PICK_SLACK, hh.max(hw) + PICK_SLACK)
            }
            PickKind::Travels => (
                Vec3::from(seg.start),
                Vec3::from(seg.end),
                TRAVEL_SLACK,
                TRAVEL_SLACK,
            ),
        };
        (a.is_finite() && b.is_finite()).then_some((a, b, r, rz))
    }

    /// Ray parameter where `ray` picks `seg` with the given tolerance.
    fn test(self, ray: &Ray, seg: &Segment, radius: f32) -> Option<f32> {
        match self {
            PickKind::Extrusions => ray_segment(ray, seg, radius),
            PickKind::Travels => ray_travel(ray, seg, radius),
        }
    }
}

/// Axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    /// Minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
}

impl Aabb {
    /// The empty box.
    pub const EMPTY: Aabb = Aabb {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    /// True when nothing was included.
    pub fn is_empty(&self) -> bool {
        self.min.cmpgt(self.max).any()
    }

    /// The box with every side moved inwards by `d` (at most down to its
    /// centre); boxes built around segments padded by at least `d` still
    /// contain the segments padded by the remainder.
    pub fn shrunk(&self, d: f32) -> Aabb {
        if self.is_empty() || d.is_nan() || d <= 0.0 {
            return *self;
        }
        let half = (self.max - self.min) * 0.5;
        let d = Vec3::splat(d).min(half);
        Aabb {
            min: self.min + d,
            max: self.max - d,
        }
    }

    /// Grow to include `p`.
    pub fn include(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    /// Parameter interval `[t0, t1]` (clipped to `t >= 0`) where the ray is
    /// inside the box.
    pub fn ray_interval(&self, ray: &Ray) -> Option<(f32, f32)> {
        if self.is_empty() {
            return None;
        }
        let mut t0 = 0.0f32;
        let mut t1 = f32::INFINITY;
        for axis in 0..3 {
            let o = ray.origin[axis];
            let d = ray.dir[axis];
            let (lo, hi) = (self.min[axis], self.max[axis]);
            if d.abs() < 1e-12 {
                if o < lo || o > hi {
                    return None;
                }
                continue;
            }
            let inv = 1.0 / d;
            let (mut a, mut b) = ((lo - o) * inv, (hi - o) * inv);
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            t0 = t0.max(a);
            t1 = t1.min(b);
            if t0 > t1 {
                return None;
            }
        }
        Some((t0, t1))
    }
}

/// Which segments may be picked.
#[derive(Clone, Debug, PartialEq)]
pub struct PickFilter {
    /// Inclusive layer index range.
    pub layers: (usize, usize),
    /// Hidden feature bit mask (bit = feature index).
    pub hidden_mask: u32,
    /// `(line, top_layer)`: on layers `>= top_layer` only segments with
    /// `line <= line` are pickable (layer scrubbing).
    pub line_limit: Option<(u32, usize)>,
}

impl Default for PickFilter {
    fn default() -> Self {
        PickFilter {
            layers: (0, usize::MAX),
            hidden_mask: 0,
            line_limit: None,
        }
    }
}

impl PickFilter {
    /// Hidden features only apply to extrusions: travels are drawn whatever
    /// feature they were annotated with.
    fn accepts(&self, kind: PickKind, seg: &Segment, layer: usize) -> bool {
        if !kind.includes(seg) {
            return false;
        }
        if kind == PickKind::Extrusions
            && (self.hidden_mask >> (seg.feature.index() as u32 & 31)) & 1 != 0
        {
            return false;
        }
        if let Some((line, top)) = self.line_limit
            && layer >= top
            && seg.line > line
        {
            return false;
        }
        true
    }
}

/// Result of a successful pick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickHit {
    /// Index into `Toolpath::segments`.
    pub segment: usize,
    /// Ray parameter of the hit.
    pub t: f32,
    /// World position of the hit.
    pub point: Vec3,
}

/// How far along a unit ray it can skip before a shape whose bounding
/// sphere has the given centre and radius without passing any of it.
/// Starting the intersection there keeps f32 precision: from far away the
/// quadratics cancel badly (a bead 3 m from the camera came out about a
/// millimetre early).
fn skip_to(ro: Vec3, rd: Vec3, center: Vec3, reach: f32) -> f32 {
    let skip = (center - ro).dot(rd) - reach - 1.0;
    if skip.is_finite() { skip.max(0.0) } else { 0.0 }
}

/// Ray vs capsule (segment `pa..pb`, radius `r`); `rd` must be unit length.
/// Returns the nearest non-negative ray parameter.
pub fn ray_capsule(ro: Vec3, rd: Vec3, pa: Vec3, pb: Vec3, r: f32) -> Option<f32> {
    let skip = skip_to(ro, rd, (pa + pb) * 0.5, (pb - pa).length() * 0.5 + r);
    ray_capsule_near(ro + rd * skip, rd, pa, pb, r).map(|t| t + skip)
}

/// [`ray_capsule`] for a ray starting close to the capsule.
fn ray_capsule_near(ro: Vec3, rd: Vec3, pa: Vec3, pb: Vec3, r: f32) -> Option<f32> {
    let ba = pb - pa;
    let oa = ro - pa;
    let baba = ba.dot(ba);
    let bard = ba.dot(rd);
    let baoa = ba.dot(oa);
    let rdoa = rd.dot(oa);
    let oaoa = oa.dot(oa);
    let a = baba - bard * bard;
    if a > baba * 1e-7 && baba > 1e-12 {
        let b = baba * rdoa - baoa * bard;
        let c = baba * oaoa - baoa * baoa - r * r * baba;
        let h = b * b - a * c;
        if h < 0.0 {
            return None;
        }
        let t = (-b - h.sqrt()) / a;
        let y = baoa + t * bard;
        if y > 0.0 && y < baba && t >= 0.0 {
            return Some(t);
        }
        let center = if y <= 0.0 { pa } else { pb };
        return ray_sphere_near(ro, rd, center, r);
    }
    // Ray parallel to the axis or degenerate segment: the end spheres decide.
    match (
        ray_sphere_near(ro, rd, pa, r),
        ray_sphere_near(ro, rd, pb, r),
    ) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

/// Ray vs sphere; `rd` must be unit length.
pub fn ray_sphere(ro: Vec3, rd: Vec3, center: Vec3, r: f32) -> Option<f32> {
    let skip = skip_to(ro, rd, center, r);
    ray_sphere_near(ro + rd * skip, rd, center, r).map(|t| t + skip)
}

/// [`ray_sphere`] for a ray starting close to the sphere.
fn ray_sphere_near(ro: Vec3, rd: Vec3, center: Vec3, r: f32) -> Option<f32> {
    let oc = ro - center;
    let b = oc.dot(rd);
    let c = oc.dot(oc) - r * r;
    let h = b * b - c;
    if h < 0.0 {
        return None;
    }
    let s = h.sqrt();
    let t = -b - s;
    if t >= 0.0 {
        Some(t)
    } else if -b + s >= 0.0 {
        Some(0.0)
    } else {
        None
    }
}

/// Radii of the rendered bead: `(half width, half height)` in mm.
fn bead_radii(seg: &Segment) -> (f32, f32) {
    (
        render_width(seg.width) * 0.5,
        render_height(seg.height) * 0.5,
    )
}

/// Bead centre line: the nozzle path lowered by half the layer height.
fn bead_axis(seg: &Segment, hh: f32) -> (Vec3, Vec3) {
    let drop = Vec3::new(0.0, 0.0, hh);
    (Vec3::from(seg.start) - drop, Vec3::from(seg.end) - drop)
}

/// Ray vs one rendered bead, with an optional minimum radius (pixel
/// tolerance). Returns the ray parameter.
pub fn ray_segment(ray: &Ray, seg: &Segment, min_radius: f32) -> Option<f32> {
    let (hw, hh) = bead_radii(seg);
    let (a, b) = bead_axis(seg, hh);
    if !a.is_finite() || !b.is_finite() {
        return None;
    }
    let min_radius = if min_radius.is_finite() {
        min_radius.clamp(0.0, hw + PICK_SLACK)
    } else {
        0.0
    };
    let r = hw.max(min_radius);
    let rv = hh.max(min_radius.min(r)).max(1e-4);
    // Scale Z so the elliptic cross-section becomes a circle of radius r.
    let scale = Vec3::new(1.0, 1.0, r / rv);
    let d = ray.dir * scale;
    let len = d.length();
    if len <= 1e-12 || !len.is_finite() {
        return None;
    }
    let t = ray_capsule(ray.origin * scale, d / len, a * scale, b * scale, r)?;
    Some(t / len)
}

/// Ray parameter and distance of the ray's closest approach to the segment
/// `a..b` (`dir` must be unit length; the ray starts at `t = 0`).
pub fn ray_segment_closest(origin: Vec3, dir: Vec3, a: Vec3, b: Vec3) -> (f32, f32) {
    let u = b - a;
    let r = a - origin;
    let len2 = u.dot(u);
    let du = dir.dot(u);
    let dr = dir.dot(r);
    let ur = u.dot(r);
    let mut s = 0.0;
    if len2 > 1e-12 {
        let denom = len2 - du * du;
        if denom > 1e-9 * len2 {
            s = ((du * dr - ur) / denom).clamp(0.0, 1.0);
        }
    }
    let mut t = du * s + dr;
    if t < 0.0 {
        t = 0.0;
        s = if len2 > 1e-12 {
            (-ur / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
    let gap = origin + dir * t - (a + u * s);
    (t, gap.length())
}

/// Ray vs one travel move: a hit when the ray passes within `radius` mm
/// (capped at [`TRAVEL_SLACK`]) of the nozzle path. Returns the ray
/// parameter of the closest approach.
pub fn ray_travel(ray: &Ray, seg: &Segment, radius: f32) -> Option<f32> {
    let (a, b) = (Vec3::from(seg.start), Vec3::from(seg.end));
    if !a.is_finite() || !b.is_finite() || !ray.origin.is_finite() || !ray.dir.is_finite() {
        return None;
    }
    let radius = if radius.is_finite() {
        radius.clamp(0.0, TRAVEL_SLACK)
    } else {
        0.0
    };
    let (t, dist) = ray_segment_closest(ray.origin, ray.dir, a, b);
    (dist <= radius && t.is_finite()).then_some(t)
}

/// Uniform XY grid over one layer.
#[derive(Clone, Debug)]
struct LayerGrid {
    origin: Vec2,
    cell: f32,
    nx: usize,
    ny: usize,
    /// Offset of this grid's `nx * ny + 1` entries in `PickIndex::cell_starts`.
    starts_offset: usize,
}

impl LayerGrid {
    fn cell_coord(&self, v: f32, origin: f32, n: usize) -> usize {
        let c = ((v - origin) / self.cell).floor();
        if c.is_nan() || c < 0.0 {
            0
        } else {
            (c as usize).min(n - 1)
        }
    }

    /// Call `f(cell_index)` for every cell overlapped by the segment's XY
    /// footprint widened by `expand`. Long segments are walked piecewise so
    /// diagonal lines touch only the cells along them.
    fn for_each_cell(&self, a: Vec2, b: Vec2, expand: f32, mut f: impl FnMut(usize)) {
        let len = (b - a).length();
        let pieces = if len.is_finite() {
            ((len / self.cell).ceil() as usize).clamp(1, 4096)
        } else {
            1
        };
        let mut prev: Option<(usize, usize, usize, usize)> = None;
        for k in 0..pieces {
            let p = a.lerp(b, k as f32 / pieces as f32);
            let q = a.lerp(b, (k + 1) as f32 / pieces as f32);
            let lo = p.min(q) - Vec2::splat(expand);
            let hi = p.max(q) + Vec2::splat(expand);
            let x0 = self.cell_coord(lo.x, self.origin.x, self.nx);
            let x1 = self.cell_coord(hi.x, self.origin.x, self.nx);
            let y0 = self.cell_coord(lo.y, self.origin.y, self.ny);
            let y1 = self.cell_coord(hi.y, self.origin.y, self.ny);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if let Some((px0, px1, py0, py1)) = prev
                        && (px0..=px1).contains(&x)
                        && (py0..=py1).contains(&y)
                    {
                        continue;
                    }
                    f(y * self.nx + x);
                }
            }
            prev = Some((x0, x1, y0, y1));
        }
    }
}

#[derive(Clone, Debug)]
struct LayerEntry {
    aabb: Aabb,
    segments: Range<usize>,
    grid: Option<LayerGrid>,
}

/// Consecutive layers per [`PickIndex`] group. Groups let a pick skip most
/// layers with one box test per group, which matters when a file has a
/// layer per segment (spiral vase mode).
const LAYERS_PER_GROUP: usize = 32;

/// Spatial index over the extrusion (or travel) segments of one toolpath.
#[derive(Clone, Debug, Default)]
pub struct PickIndex {
    kind: PickKind,
    layers: Vec<LayerEntry>,
    /// Union box of each run of [`LAYERS_PER_GROUP`] layers.
    groups: Vec<Aabb>,
    cell_starts: Vec<u32>,
    cell_items: Vec<u32>,
    segment_count: usize,
}

impl PickIndex {
    /// Build the index over the extrusions. Returns `None` if `cancel` is
    /// set meanwhile.
    pub fn build(toolpath: &Toolpath, cancel: &AtomicBool) -> Option<PickIndex> {
        PickIndex::build_kind(toolpath, PickKind::Extrusions, cancel)
    }

    /// Build the index over the travel moves. Returns `None` if `cancel` is
    /// set meanwhile.
    pub fn build_travels(toolpath: &Toolpath, cancel: &AtomicBool) -> Option<PickIndex> {
        PickIndex::build_kind(toolpath, PickKind::Travels, cancel)
    }

    /// Build the index over the segments of `kind`. Returns `None` if
    /// `cancel` is set meanwhile.
    pub fn build_kind(
        toolpath: &Toolpath,
        kind: PickKind,
        cancel: &AtomicBool,
    ) -> Option<PickIndex> {
        let segs = &toolpath.segments;
        let mut index = PickIndex {
            kind,
            segment_count: segs.len(),
            ..Default::default()
        };
        for range in layer_segment_ranges(toolpath) {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let mut aabb = Aabb::EMPTY;
            let mut count = 0usize;
            for seg in &segs[range.clone()] {
                let Some((a, b, r, rz)) = kind.footprint(seg) else {
                    continue;
                };
                let e = Vec3::new(r, r, rz);
                aabb.include(a.min(b) - e);
                aabb.include(a.max(b) + e);
                count += 1;
            }
            let grid = if count > GRID_THRESHOLD && !aabb.is_empty() {
                Some(index.build_grid(segs, range.clone(), &aabb, count))
            } else {
                None
            };
            index.layers.push(LayerEntry {
                aabb,
                segments: range,
                grid,
            });
        }
        index.groups = index
            .layers
            .chunks(LAYERS_PER_GROUP)
            .map(|group| {
                group.iter().fold(Aabb::EMPTY, |mut acc, l| {
                    if !l.aabb.is_empty() {
                        acc.include(l.aabb.min);
                        acc.include(l.aabb.max);
                    }
                    acc
                })
            })
            .collect();
        Some(index)
    }

    fn build_grid(
        &mut self,
        segs: &[Segment],
        range: Range<usize>,
        aabb: &Aabb,
        count: usize,
    ) -> LayerGrid {
        let size = (aabb.max - aabb.min).truncate().max(Vec2::splat(1e-3));
        let cells = (count / TARGET_PER_CELL).max(1) as f32;
        let cell = ((size.x * size.y) / cells)
            .sqrt()
            .max(size.x / MAX_CELLS_PER_AXIS as f32)
            .max(size.y / MAX_CELLS_PER_AXIS as f32)
            .max(0.05);
        let nx = ((size.x / cell).ceil() as usize).clamp(1, MAX_CELLS_PER_AXIS);
        let ny = ((size.y / cell).ceil() as usize).clamp(1, MAX_CELLS_PER_AXIS);
        let grid = LayerGrid {
            origin: aabb.min.truncate(),
            cell,
            nx,
            ny,
            starts_offset: self.cell_starts.len(),
        };
        let ncells = nx * ny;
        let mut counts = vec![0u32; ncells];
        let kind = self.kind;
        let footprint = |seg: &Segment| -> Option<(Vec2, Vec2, f32)> {
            let (a, b, r, _) = kind.footprint(seg)?;
            Some((a.truncate(), b.truncate(), r))
        };
        for seg in &segs[range.clone()] {
            if let Some((a, b, e)) = footprint(seg) {
                grid.for_each_cell(a, b, e, |c| counts[c] += 1);
            }
        }
        let base = self.cell_items.len() as u32;
        let mut running = base;
        for c in &mut counts {
            self.cell_starts.push(running);
            running += *c;
            *c = 0;
        }
        self.cell_starts.push(running);
        self.cell_items.resize(running as usize, 0);
        for (i, seg) in segs[range.clone()].iter().enumerate() {
            if let Some((a, b, e)) = footprint(seg) {
                let seg_index = (range.start + i) as u32;
                grid.for_each_cell(a, b, e, |c| {
                    let slot = self.cell_starts[grid.starts_offset + c] + counts[c];
                    self.cell_items[slot as usize] = seg_index;
                    counts[c] += 1;
                });
            }
        }
        grid
    }

    /// Which segments the index holds.
    pub fn kind(&self) -> PickKind {
        self.kind
    }

    /// Number of layers indexed.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Bounding box of a layer's pickable segments (grown by the largest
    /// pick tolerance).
    pub fn layer_bounds(&self, layer: usize) -> Option<Aabb> {
        self.layers
            .get(layer)
            .map(|l| l.aabb)
            .filter(|b| !b.is_empty())
    }

    /// Nearest visible segment hit by `ray`. For extrusions `min_radius`
    /// widens thin beads (pixel tolerance, capped at [`PICK_SLACK`] beyond
    /// the bead); for travels it is the distance from the nozzle path that
    /// still counts as a hit (capped at [`TRAVEL_SLACK`]). Returns `None`
    /// when the index does not belong to `toolpath`.
    pub fn pick(
        &self,
        toolpath: &Toolpath,
        ray: &Ray,
        filter: &PickFilter,
        min_radius: f32,
    ) -> Option<PickHit> {
        self.pick_within(toolpath, ray, filter, min_radius, f32::INFINITY)
    }

    /// Like [`PickIndex::pick`], but only hits nearer than `max_t` along the
    /// ray count, e.g. travels in front of an already picked bead. The
    /// search stops early beyond `max_t`.
    pub fn pick_within(
        &self,
        toolpath: &Toolpath,
        ray: &Ray,
        filter: &PickFilter,
        min_radius: f32,
        max_t: f32,
    ) -> Option<PickHit> {
        let segs = &toolpath.segments;
        if segs.len() != self.segment_count || self.layers.is_empty() {
            return None;
        }
        let last = self.layers.len() - 1;
        let lo = filter.layers.0.min(last);
        let hi = filter.layers.1.min(last).max(lo);
        // Travel boxes are padded for the largest radius: cull with the
        // radius actually asked for.
        let shrink = match self.kind {
            PickKind::Extrusions => 0.0,
            PickKind::Travels if min_radius.is_finite() => {
                TRAVEL_SLACK - min_radius.clamp(0.0, TRAVEL_SLACK)
            }
            PickKind::Travels => TRAVEL_SLACK,
        };
        // Groups hit by the ray, nearest entry first.
        let mut groups: Vec<(f32, usize)> = (lo / LAYERS_PER_GROUP..=hi / LAYERS_PER_GROUP)
            .filter_map(|g| {
                let aabb = self.groups.get(g)?.shrunk(shrink);
                aabb.ray_interval(ray).map(|(t0, _)| (t0, g))
            })
            .collect();
        groups.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut best: Option<PickHit> = None;
        let mut best_t = if max_t.is_nan() { f32::INFINITY } else { max_t };
        let mut consider = |seg_index: usize, layer: usize, best_t: &mut f32| {
            let Some(seg) = segs.get(seg_index) else {
                return;
            };
            if !filter.accepts(self.kind, seg, layer) {
                return;
            }
            if let Some(t) = self.kind.test(ray, seg, min_radius)
                && t < *best_t
            {
                *best_t = t;
                best = Some(PickHit {
                    segment: seg_index,
                    t,
                    point: ray.at(t),
                });
            }
        };
        let mut candidates: Vec<(f32, f32, usize)> = Vec::with_capacity(LAYERS_PER_GROUP);
        for (group_t0, group) in groups {
            // A group's box holds all its layers: nothing in it (or in the
            // groups after it) can beat a hit in front of it.
            if group_t0 > best_t {
                break;
            }
            let first = (group * LAYERS_PER_GROUP).max(lo);
            let end = ((group + 1) * LAYERS_PER_GROUP).min(hi + 1);
            candidates.clear();
            candidates.extend((first..end).filter_map(|i| {
                let entry = self.layers.get(i)?;
                entry
                    .aabb
                    .shrunk(shrink)
                    .ray_interval(ray)
                    .map(|(t0, t1)| (t0, t1, i))
            }));
            candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
            for &(t0, t1, layer) in &candidates {
                if t0 > best_t {
                    break;
                }
                let Some(entry) = self.layers.get(layer) else {
                    continue;
                };
                match &entry.grid {
                    None => {
                        for i in entry.segments.clone() {
                            consider(i, layer, &mut best_t);
                        }
                    }
                    Some(grid) => {
                        self.walk_grid(grid, ray, t0, t1, &mut best_t, |i, bt| {
                            consider(i, layer, bt);
                        });
                    }
                }
            }
        }
        best
    }

    /// Visit the grid cells crossed by the ray between `t0` and `t1` in
    /// order (2D DDA), stopping once a cell starts beyond `best_t`.
    fn walk_grid(
        &self,
        grid: &LayerGrid,
        ray: &Ray,
        t0: f32,
        t1: f32,
        best_t: &mut f32,
        mut visit: impl FnMut(usize, &mut f32),
    ) {
        let p0 = ray.at(t0).truncate();
        let d = ray.dir.truncate();
        let mut x = grid.cell_coord(p0.x, grid.origin.x, grid.nx) as i64;
        let mut y = grid.cell_coord(p0.y, grid.origin.y, grid.ny) as i64;
        let axis = |d: f32, p: f32, origin: f32, c: i64| -> (i64, f32, f32) {
            if d.abs() < 1e-12 {
                return (0, f32::INFINITY, f32::INFINITY);
            }
            let step = if d > 0.0 { 1 } else { -1 };
            let boundary = origin + (c + i64::from(d > 0.0)) as f32 * grid.cell;
            let t_max = t0 + (boundary - p) / d;
            (step, t_max.max(t0), grid.cell / d.abs())
        };
        let (sx, mut tmx, dtx) = axis(d.x, p0.x, grid.origin.x, x);
        let (sy, mut tmy, dty) = axis(d.y, p0.y, grid.origin.y, y);
        let mut t_entry = t0;
        let limit = grid.nx + grid.ny + 2;
        for _ in 0..limit {
            if t_entry > *best_t || t_entry > t1 {
                break;
            }
            let cell = grid.starts_offset + y as usize * grid.nx + x as usize;
            if let (Some(&s), Some(&e)) =
                (self.cell_starts.get(cell), self.cell_starts.get(cell + 1))
            {
                for k in s..e {
                    if let Some(&item) = self.cell_items.get(k as usize) {
                        visit(item as usize, best_t);
                    }
                }
            }
            if tmx < tmy {
                t_entry = tmx;
                tmx += dtx;
                x += sx;
            } else {
                t_entry = tmy;
                tmy += dty;
                y += sy;
            }
            if !t_entry.is_finite() || x < 0 || y < 0 || x >= grid.nx as i64 || y >= grid.ny as i64
            {
                break;
            }
        }
    }
}

/// Reference implementation without acceleration (tests, tiny files).
pub fn pick_brute_force(
    toolpath: &Toolpath,
    ray: &Ray,
    filter: &PickFilter,
    min_radius: f32,
) -> Option<PickHit> {
    pick_brute_force_kind(toolpath, PickKind::Extrusions, ray, filter, min_radius)
}

/// Reference implementation without acceleration for segments of `kind`.
pub fn pick_brute_force_kind(
    toolpath: &Toolpath,
    kind: PickKind,
    ray: &Ray,
    filter: &PickFilter,
    min_radius: f32,
) -> Option<PickHit> {
    let ranges = layer_segment_ranges(toolpath);
    if ranges.is_empty() {
        return None;
    }
    let last = ranges.len() - 1;
    let lo = filter.layers.0.min(last);
    let hi = filter.layers.1.min(last).max(lo);
    let mut best: Option<PickHit> = None;
    for (layer, range) in ranges.iter().enumerate().take(hi + 1).skip(lo) {
        for i in range.clone() {
            let seg = &toolpath.segments[i];
            if !filter.accepts(kind, seg, layer) {
                continue;
            }
            if let Some(t) = kind.test(ray, seg, min_radius)
                && best.is_none_or(|b| t < b.t)
            {
                best = Some(PickHit {
                    segment: i,
                    t,
                    point: ray.at(t),
                });
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down_ray(x: f32, y: f32) -> Ray {
        Ray {
            origin: Vec3::new(x, y, 100.0),
            dir: -Vec3::Z,
        }
    }

    #[test]
    fn capsule_hits_body_and_caps() {
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(10.0, 0.0, 0.0);
        let r = 1.0;
        let t = ray_capsule(Vec3::new(5.0, 0.0, 10.0), -Vec3::Z, a, b, r).expect("body");
        assert!((t - 9.0).abs() < 1e-4);
        let t = ray_capsule(Vec3::new(-0.5, 0.0, 10.0), -Vec3::Z, a, b, r).expect("cap");
        assert!((t - (10.0 - (1.0f32 - 0.25).sqrt())).abs() < 1e-4);
        assert!(ray_capsule(Vec3::new(5.0, 1.5, 10.0), -Vec3::Z, a, b, r).is_none());
        assert!(ray_capsule(Vec3::new(-1.5, 0.0, 10.0), -Vec3::Z, a, b, r).is_none());
        // Parallel to the axis.
        let t = ray_capsule(Vec3::new(-20.0, 0.0, 0.0), Vec3::X, a, b, r).expect("end on");
        assert!((t - 19.0).abs() < 1e-4);
        // Degenerate segment = sphere.
        let t = ray_capsule(Vec3::new(0.0, 0.0, 5.0), -Vec3::Z, a, a, r).expect("sphere");
        assert!((t - 4.0).abs() < 1e-4);
        // Behind the origin.
        assert!(ray_capsule(Vec3::new(5.0, 0.0, -10.0), -Vec3::Z, a, b, r).is_none());
    }

    #[test]
    fn far_rays_hit_beads_where_they_are() {
        // A bead's top seen from up to 3 m away, straight at its crest and
        // just inside its edge: the hit lies on the surface, never in front
        // of it (f32 cancellation used to put it up to 1.4 mm early).
        let seg = Segment {
            start: [100.0, 110.0, 1.2],
            end: [120.0, 110.0, 1.2],
            width: 0.5,
            height: 0.2,
            kind: sprint_gcode::MoveKind::Extrude,
            ..Default::default()
        };
        for dist in [50.0f32, 300.0, 1000.0, 3000.0] {
            for elev in [0.3f32, 0.6, 1.0, 1.5] {
                for yaw in [0.0f32, 0.7, 1.4] {
                    for off in [0.0f32, 0.1, 0.2, 0.24] {
                        let q = Vec3::new(110.0 + 3.0 * yaw, 110.0 + off, 1.2);
                        let dir =
                            -Vec3::new(elev.cos() * yaw.cos(), elev.cos() * yaw.sin(), elev.sin());
                        let ray = Ray {
                            origin: q - dir * dist,
                            dir,
                        };
                        if let Some(t) = ray_segment(&ray, &seg, 0.0) {
                            assert!(
                                t > dist - 0.005,
                                "{dist} mm, elevation {elev}, yaw {yaw}, {off} off the \
                                 crest: hit {} mm early",
                                dist - t
                            );
                        }
                    }
                }
            }
            // Straight down onto the crest: a hit, on the surface.
            let ray = Ray {
                origin: Vec3::new(110.0, 110.0, 1.2 + dist),
                dir: -Vec3::Z,
            };
            let t = ray_segment(&ray, &seg, 0.0).expect("crest");
            assert!((t - dist).abs() < 0.005, "{dist}: {}", t - dist);
        }
        // The same for spheres.
        let t = ray_sphere(Vec3::new(0.0, 0.0, 3000.0), -Vec3::Z, Vec3::ZERO, 0.1).expect("sphere");
        assert!((t - 2999.9).abs() < 0.005, "{t}");
    }

    #[test]
    fn aabb_interval() {
        let b = Aabb {
            min: Vec3::ZERO,
            max: Vec3::ONE,
        };
        let (t0, t1) = b.ray_interval(&down_ray(0.5, 0.5)).expect("hit");
        assert!((t0 - 99.0).abs() < 1e-4 && (t1 - 100.0).abs() < 1e-4);
        assert!(b.ray_interval(&down_ray(1.5, 0.5)).is_none());
        assert!(Aabb::EMPTY.ray_interval(&down_ray(0.5, 0.5)).is_none());
    }

    #[test]
    fn shrinking_boxes_stops_at_the_centre() {
        let b = Aabb {
            min: Vec3::ZERO,
            max: Vec3::new(4.0, 10.0, 1.0),
        };
        let s = b.shrunk(1.0);
        assert_eq!(s.min, Vec3::new(1.0, 1.0, 0.5));
        assert_eq!(s.max, Vec3::new(3.0, 9.0, 0.5));
        assert_eq!(b.shrunk(-1.0), b);
        assert_eq!(b.shrunk(f32::NAN), b);
        assert!(Aabb::EMPTY.shrunk(1.0).is_empty());
    }

    #[test]
    fn closest_approach_of_a_ray_and_a_segment() {
        let (a, b) = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0));
        // Straight down onto the middle: touches at t = 10.
        let (t, d) = ray_segment_closest(Vec3::new(5.0, 0.0, 10.0), -Vec3::Z, a, b);
        assert!((t - 10.0).abs() < 1e-4 && d < 1e-4, "{t} {d}");
        // Passing 0.5 mm beside it.
        let (t, d) = ray_segment_closest(Vec3::new(5.0, 0.5, 10.0), -Vec3::Z, a, b);
        assert!((t - 10.0).abs() < 1e-4 && (d - 0.5).abs() < 1e-4);
        // Beyond the end: the end point is closest.
        let (t, d) = ray_segment_closest(Vec3::new(12.0, 0.0, 10.0), -Vec3::Z, a, b);
        assert!((t - 10.0).abs() < 1e-4 && (d - 2.0).abs() < 1e-4);
        // Segment behind the ray origin: the origin is the closest ray point.
        let (t, d) = ray_segment_closest(Vec3::new(5.0, 0.0, -1.0), -Vec3::Z, a, b);
        assert!(t == 0.0 && (d - 1.0).abs() < 1e-4);
        // Parallel ray above the segment.
        let (_, d) = ray_segment_closest(Vec3::new(-5.0, 0.0, 1.0), Vec3::X, a, b);
        assert!((d - 1.0).abs() < 1e-4);
        // Degenerate segment: a point.
        let (t, d) = ray_segment_closest(Vec3::new(0.0, 3.0, 4.0), -Vec3::Z, a, a);
        assert!((t - 4.0).abs() < 1e-4 && (d - 3.0).abs() < 1e-4);
    }

    #[test]
    fn travels_are_hit_within_the_radius_only() {
        let seg = Segment {
            start: [0.0, 0.0, 0.4],
            end: [20.0, 0.0, 0.4],
            kind: sprint_gcode::MoveKind::Travel,
            ..Default::default()
        };
        let t = ray_travel(&down_ray(10.0, 0.3), &seg, 0.5).expect("within 0.5 mm");
        assert!((t - 99.6).abs() < 1e-3);
        assert!(ray_travel(&down_ray(10.0, 0.7), &seg, 0.5).is_none());
        // The radius is capped.
        assert!(ray_travel(&down_ray(10.0, TRAVEL_SLACK + 0.5), &seg, 100.0).is_none());
        assert!(ray_travel(&down_ray(10.0, 0.1), &seg, f32::NAN).is_none());
        let broken = Segment {
            start: [f32::NAN, 0.0, 0.0],
            ..seg
        };
        assert!(ray_travel(&down_ray(10.0, 0.0), &broken, 1.0).is_none());
        // Kinds pick their own segments only.
        assert!(PickKind::Travels.includes(&seg));
        assert!(!PickKind::Extrusions.includes(&seg));
    }

    #[test]
    fn flattened_bead_is_tested_elliptically() {
        let seg = Segment {
            start: [0.0, 0.0, 0.2],
            end: [10.0, 0.0, 0.2],
            width: 0.4,
            height: 0.2,
            kind: sprint_gcode::MoveKind::Extrude,
            ..Default::default()
        };
        // From the top the bead is 0.4 wide.
        assert!(ray_segment(&down_ray(5.0, 0.19), &seg, 0.0).is_some());
        assert!(ray_segment(&down_ray(5.0, 0.21), &seg, 0.0).is_none());
        // From the side it is only 0.2 tall (centre at z = 0.1).
        let side = |z: f32| Ray {
            origin: Vec3::new(5.0, -50.0, z),
            dir: Vec3::Y,
        };
        assert!(ray_segment(&side(0.19), &seg, 0.0).is_some());
        assert!(ray_segment(&side(0.25), &seg, 0.0).is_none());
        let t = ray_segment(&side(0.1), &seg, 0.0).expect("hit");
        assert!((t - 49.8).abs() < 1e-3);
        // Pixel tolerance widens it.
        assert!(ray_segment(&down_ray(5.0, 0.35), &seg, 0.4).is_some());
        assert!(ray_segment(&down_ray(5.0, 0.45), &seg, 0.4).is_none());
        // ...but never beyond PICK_SLACK around the bead.
        assert!(ray_segment(&down_ray(5.0, 1.5), &seg, 50.0).is_none());
    }
}
