//! Procedural demo toolpaths for examples, tests and benchmarks.
//!
//! [`demo_toolpath`] builds a small but realistic print without any G-code:
//! a twisted rounded-square tower (40 layers with external and inner
//! perimeters, solid bottom/top skins, connected zig-zag infill), a lobed
//! spiral vase printed with a second tool, and a skirt, with travels,
//! retractions, fan ramps and timing. [`stress_toolpath`] generates
//! millions of segments quickly for performance tests.

use std::f32::consts::{PI, TAU};

use sprint_gcode::{
    Bounds, Feature, FeatureStat, Layer, MoveKind, Retraction, Segment, Stats, Toolpath,
};

/// Filament cross-section for 1.75 mm filament (mm^2).
const FILAMENT_AREA: f32 = 1.75 * 1.75 * PI / 4.0;
/// Travel feedrate (mm/s).
const TRAVEL_SPEED: f32 = 150.0;
/// Retraction length (mm).
const RETRACT: f32 = 0.8;

/// Incremental toolpath writer that keeps lines, time, layers and
/// statistics consistent, the way the G-code analyzer would.
struct Builder {
    tp: Toolpath,
    pos: [f32; 3],
    line: u32,
    time: f32,
    tool: u8,
    fan: u8,
    feature: Feature,
    retracted: bool,
    layer_open: bool,
    layer_z: f32,
    layer_height: f32,
    layer_seg_start: usize,
    layer_line_start: u32,
    layer_time_start: f32,
    layer_extruded: f32,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            tp: Toolpath::default(),
            pos: [0.0, 0.0, 5.0],
            // A short start G-code block precedes the first move.
            line: 24,
            time: 0.0,
            tool: 0,
            fan: 0,
            feature: Feature::Custom,
            retracted: false,
            layer_open: false,
            layer_z: 0.0,
            layer_height: 0.0,
            layer_seg_start: 0,
            layer_line_start: 0,
            layer_time_start: 0.0,
            layer_extruded: 0.0,
        }
    }

    fn layer_index(&self) -> u32 {
        self.tp.layers.len() as u32
    }

    fn begin_layer(&mut self, z: f32, height: f32) {
        self.end_layer();
        self.line += 2; // ;LAYER_CHANGE / ;Z:
        self.layer_open = true;
        self.layer_z = z;
        self.layer_height = height;
        self.layer_seg_start = self.tp.segments.len();
        self.layer_line_start = self.line;
        self.layer_time_start = self.time;
        self.layer_extruded = 0.0;
    }

    fn end_layer(&mut self) {
        if !self.layer_open {
            return;
        }
        self.layer_open = false;
        self.tp.layers.push(Layer {
            z: self.layer_z,
            height: self.layer_height,
            segments: self.layer_seg_start..self.tp.segments.len(),
            lines: self.layer_line_start..self.line,
            time_start: self.layer_time_start,
            duration: self.time - self.layer_time_start,
            extruded: self.layer_extruded,
        });
    }

    fn set_feature(&mut self, feature: Feature) {
        if feature != self.feature {
            self.feature = feature;
            self.line += 1; // ;TYPE:...
        }
    }

    fn set_tool(&mut self, tool: u8) {
        if tool != self.tool {
            self.tool = tool;
            self.line += 3; // T<n> + wipe/prime commands
            self.time += 4.0;
        }
    }

    fn set_fan(&mut self, fan: u8) {
        if fan != self.fan {
            self.fan = fan;
            self.line += 1; // M106 / M107
        }
    }

    fn push(&mut self, to: [f32; 3], speed: f32, kind: MoveKind, width: f32, height: f32) {
        let mut seg = Segment {
            start: self.pos,
            end: to,
            feedrate: speed,
            width,
            height,
            time: self.time,
            line: self.line,
            layer: self.layer_index(),
            kind,
            feature: self.feature,
            tool: self.tool,
            fan: self.fan,
            ..Default::default()
        };
        let len = seg.length();
        seg.duration = len / speed.max(1.0);
        if kind == MoveKind::Extrude {
            seg.extruded = width * height * len / FILAMENT_AREA;
            self.layer_extruded += seg.extruded;
        }
        self.time += seg.duration;
        self.line += 1;
        self.pos = to;
        self.tp.segments.push(seg);
    }

    fn retraction(&mut self, amount: f32) {
        self.tp.retractions.push(Retraction {
            pos: self.pos,
            amount,
            line: self.line,
            layer: self.layer_index(),
            tool: self.tool,
        });
        self.line += 1;
        self.time += amount.abs() / 35.0;
    }

    /// Travel with retraction when the move is long.
    fn travel_to(&mut self, to: [f32; 3]) {
        let d = dist(self.pos, to);
        if d < 1e-4 {
            return;
        }
        let long = d > 2.0;
        if long && !self.retracted {
            self.retraction(-RETRACT);
            self.retracted = true;
        }
        self.push(to, TRAVEL_SPEED, MoveKind::Travel, 0.0, 0.0);
        if self.retracted {
            self.retraction(RETRACT);
            self.retracted = false;
        }
    }

    fn extrude_to(&mut self, to: [f32; 3], width: f32, height: f32, speed: f32) {
        if self.retracted {
            self.retraction(RETRACT);
            self.retracted = false;
        }
        self.push(to, speed, MoveKind::Extrude, width, height);
    }

    /// Extrude a closed loop starting at `points[0]` (travel there first).
    fn loop_path(&mut self, points: &[[f32; 3]], width: f32, height: f32, speed: f32) {
        let Some(&first) = points.first() else {
            return;
        };
        self.travel_to(first);
        for &p in points.iter().skip(1).chain(std::iter::once(&first)) {
            self.extrude_to(p, width, height, speed);
        }
    }

    fn finish(mut self) -> Toolpath {
        self.end_layer();
        let mut tp = self.tp;
        tp.line_count = self.line + 20; // end G-code
        let mut bounds = Bounds::empty();
        let mut travel_bounds = Bounds::empty();
        let mut stats = Stats {
            estimated_time: self.time,
            ..Default::default()
        };
        let mut per_feature = [FeatureStat::default(); Feature::COUNT];
        let mut speed_sum = 0.0f32;
        let mut speed_time = 0.0f32;
        let mut filament_per_tool = vec![0.0f32; 2];
        for s in &tp.segments {
            travel_bounds.include(s.start);
            travel_bounds.include(s.end);
            if s.is_extrusion() {
                bounds.include(s.start);
                bounds.include(s.end);
                stats.extrude_moves += 1;
                stats.extrude_distance += s.length();
                stats.filament_length += s.extruded;
                if let Some(slot) = filament_per_tool.get_mut(s.tool as usize) {
                    *slot += s.extruded;
                }
                stats.max_print_speed = stats.max_print_speed.max(s.feedrate);
                stats.min_print_speed = if stats.min_print_speed == 0.0 {
                    s.feedrate
                } else {
                    stats.min_print_speed.min(s.feedrate)
                };
                speed_sum += s.feedrate * s.duration;
                speed_time += s.duration;
                let f = &mut per_feature[s.feature.index()];
                f.feature = s.feature;
                f.time += s.duration;
                f.extruded += s.extruded;
                f.distance += s.length();
                f.segments += 1;
            } else {
                stats.travel_moves += 1;
                stats.travel_distance += s.length();
                stats.max_travel_speed = stats.max_travel_speed.max(s.feedrate);
            }
        }
        stats.avg_print_speed = if speed_time > 0.0 {
            speed_sum / speed_time
        } else {
            0.0
        };
        stats.filament_per_tool = filament_per_tool;
        stats.filament_volume = stats.filament_length * FILAMENT_AREA;
        stats.filament_weight = stats.filament_volume * 1.24e-3;
        stats.retractions = tp.retractions.iter().filter(|r| r.amount < 0.0).count();
        stats.layer_count = tp.layers.len();
        stats.first_layer_height = tp.layers.first().map(|l| l.height).unwrap_or(0.0);
        stats.layer_height = tp.layers.get(1).map(|l| l.height).unwrap_or(0.0);
        stats.features = per_feature.into_iter().filter(|f| f.segments > 0).collect();
        let mut tools: Vec<u8> = tp.segments.iter().map(|s| s.tool).collect();
        tools.sort_unstable();
        tools.dedup();
        stats.tool_changes = tools.len().saturating_sub(1);
        stats.tools_used = tools;
        stats.hotend_temps = vec![(0, 215.0), (1, 230.0)];
        stats.bed_temps = vec![60.0];
        stats.heat_waits = 2;
        tp.bounds = bounds;
        tp.travel_bounds = travel_bounds;
        tp.stats = stats;
        tp
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Rounded square of half-size `half` and corner radius `radius`, rotated
/// by `angle` around `center`, as a closed polyline (first point not
/// repeated) starting mid-way along the front edge.
fn rounded_square(center: [f32; 2], half: f32, radius: f32, angle: f32, z: f32) -> Vec<[f32; 3]> {
    let r = radius.clamp(0.05, half);
    let inner = half - r;
    let (s, c) = angle.sin_cos();
    let mut pts = Vec::with_capacity(44);
    let corners = [
        (inner, -inner),
        (inner, inner),
        (-inner, inner),
        (-inner, -inner),
    ];
    pts.push([0.0, -half]);
    for (k, (cx, cy)) in corners.iter().enumerate() {
        let a0 = -PI / 2.0 + k as f32 * PI / 2.0;
        for i in 0..=8 {
            let a = a0 + i as f32 / 8.0 * PI / 2.0;
            pts.push([cx + r * a.cos(), cy + r * a.sin()]);
        }
    }
    pts.into_iter()
        .map(|[x, y]| [center[0] + x * c - y * s, center[1] + x * s + y * c, z])
        .collect()
}

/// Half extent along X of a rounded square (local frame) at height `y`.
fn rounded_square_extent(half: f32, radius: f32, y: f32) -> Option<f32> {
    let y = y.abs();
    if y > half {
        return None;
    }
    let inner = half - radius;
    if y <= inner {
        Some(half)
    } else {
        let dy = y - inner;
        Some(inner + (radius * radius - dy * dy).max(0.0).sqrt())
    }
}

/// Connected zig-zag lines clipped to a shape given by `extent(y)` (half
/// width at local `y`), in a frame rotated by `angle` around `center`.
#[allow(clippy::too_many_arguments)]
fn zigzag(
    b: &mut Builder,
    center: [f32; 2],
    angle: f32,
    half_y: f32,
    spacing: f32,
    extent: &dyn Fn(f32) -> Option<f32>,
    z: f32,
    width: f32,
    height: f32,
    speed: f32,
) {
    let (s, c) = angle.sin_cos();
    let to_world = |x: f32, y: f32| [center[0] + x * c - y * s, center[1] + x * s + y * c, z];
    let n = ((2.0 * half_y) / spacing).floor() as i32;
    let mut first = true;
    let mut flip = false;
    for i in 0..=n {
        let y = -half_y + i as f32 * spacing;
        let Some(e) = extent(y) else {
            continue;
        };
        if e <= 0.1 {
            continue;
        }
        let (x0, x1) = if flip { (e, -e) } else { (-e, e) };
        let p0 = to_world(x0, y);
        if first {
            b.travel_to(p0);
            first = false;
        } else {
            b.extrude_to(p0, width, height, speed);
        }
        b.extrude_to(to_world(x1, y), width, height, speed);
        flip = !flip;
    }
}

/// Points of one spiral-vase revolution (lobed, gently bulging profile)
/// rising from `z0` to `z1`.
fn vase_revolution(center: [f32; 2], z0: f32, z1: f32, steps: usize) -> Vec<[f32; 3]> {
    (1..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let theta = t * TAU;
            let z = z0 + (z1 - z0) * t;
            let r = vase_radius(theta, z);
            [center[0] + r * theta.cos(), center[1] + r * theta.sin(), z]
        })
        .collect()
}

fn vase_radius(theta: f32, z: f32) -> f32 {
    let profile = 12.0 + 4.0 * (z / 30.0 * PI * 1.1).sin() - 1.5 * (z / 30.0).powi(2);
    profile * (1.0 + 0.06 * (6.0 * theta + z * 0.12).cos())
}

/// A realistic demo print (about 30k segments): a twisted rounded-square
/// tower with perimeters, skins and zig-zag infill (40 layers), a lobed
/// spiral vase on tool 1 and a skirt, with travels and retractions.
pub fn demo_toolpath() -> Toolpath {
    let mut b = Builder::new();
    let tower = [82.0f32, 108.0];
    let vase = [138.0f32, 112.0];
    let first_h = 0.3f32;
    let h = 0.25f32;
    let tower_layers = 40;
    let vase_top = 30.0f32;
    let vase_solid_layers = 3;
    let layer_count = ((vase_top - first_h) / h).ceil() as usize + 1;

    for k in 0..layer_count {
        let z = first_h + k as f32 * h;
        let height = if k == 0 { first_h } else { h };
        b.begin_layer(z, height);
        let first = k == 0;
        b.set_fan(match k {
            0 => 0,
            1 => 128,
            _ => 255,
        });
        let speed = |normal: f32| if first { 20.0 } else { normal };
        let width = |normal: f32| if first { 0.5 } else { normal };

        if first {
            b.set_tool(0);
            b.set_feature(Feature::Skirt);
            let skirt: Vec<[f32; 3]> = (0..96)
                .map(|i| {
                    let a = i as f32 / 96.0 * TAU;
                    let (ca, sa) = (a.cos(), a.sin());
                    let x = 110.0 + 52.0 * ca.signum() * ca.abs().powf(0.35);
                    let y = 110.0 + 28.0 * sa.signum() * sa.abs().powf(0.35);
                    [x, y, z]
                })
                .collect();
            b.loop_path(&skirt, 0.5, height, 25.0);
        }

        if k < tower_layers {
            b.set_tool(0);
            let twist = (k as f32 * 0.6).to_radians();
            let half = 12.5;
            let radius = 4.0;
            let ext_w = width(0.45);
            let peri_w = width(0.45);
            let spacing = 0.42;
            // Inner perimeter first, then the external one (PrusaSlicer order).
            b.set_feature(Feature::Perimeter);
            let inner = rounded_square(tower, half - spacing, radius - spacing, twist, z);
            b.loop_path(&inner, peri_w, height, speed(45.0));
            b.set_feature(Feature::ExternalPerimeter);
            let outer = rounded_square(tower, half, radius, twist, z);
            b.loop_path(&outer, ext_w, height, speed(25.0));

            let inset = 2.0 * spacing + 0.15;
            let ih = half - inset;
            let ir = (radius - inset).max(0.5);
            let extent = move |y: f32| rounded_square_extent(ih, ir, y).map(|e| e - 0.1);
            let skin = k < 3 || k >= tower_layers - 3;
            let dir = if k % 2 == 0 { 0.0 } else { PI / 2.0 };
            if skin {
                let top = k == tower_layers - 1;
                b.set_feature(if top {
                    Feature::TopSolidInfill
                } else {
                    Feature::SolidInfill
                });
                let w = width(0.45);
                zigzag(
                    &mut b,
                    tower,
                    twist + PI / 4.0 + dir,
                    ih,
                    w * 0.95,
                    &extent,
                    z,
                    w,
                    height,
                    speed(if top { 40.0 } else { 60.0 }),
                );
            } else {
                b.set_feature(Feature::InternalInfill);
                zigzag(
                    &mut b,
                    tower,
                    twist + PI / 4.0 + dir,
                    ih,
                    2.2,
                    &extent,
                    z,
                    0.5,
                    height,
                    90.0,
                );
            }
        }

        // The vase: solid bottom, then one spiral revolution per layer.
        b.set_tool(1);
        if k < vase_solid_layers {
            b.set_feature(Feature::ExternalPerimeter);
            let ring: Vec<[f32; 3]> = (0..120)
                .map(|i| {
                    let theta = i as f32 / 120.0 * TAU;
                    let r = vase_radius(theta, z);
                    [vase[0] + r * theta.cos(), vase[1] + r * theta.sin(), z]
                })
                .collect();
            b.loop_path(&ring, width(0.45), height, speed(25.0));
            b.set_feature(Feature::SolidInfill);
            let rmin = (0..360)
                .map(|i| vase_radius(i as f32 / 360.0 * TAU, z))
                .fold(f32::INFINITY, f32::min)
                - 0.55;
            let extent =
                move |y: f32| (y.abs() < rmin).then(|| (rmin * rmin - y * y).max(0.0).sqrt() - 0.1);
            let dir = if k % 2 == 0 { PI / 4.0 } else { -PI / 4.0 };
            zigzag(
                &mut b,
                vase,
                dir,
                rmin,
                0.43,
                &extent,
                z,
                width(0.45),
                height,
                speed(50.0),
            );
        } else if z <= vase_top + 1e-3 {
            b.set_feature(Feature::ExternalPerimeter);
            let pts = vase_revolution(vase, z - h, z, 110);
            let start = [vase[0] + vase_radius(0.0, z - h), vase[1], z - h];
            b.travel_to(start);
            for p in pts {
                b.extrude_to(p, 0.48, h, 30.0);
            }
        }
    }
    b.set_feature(Feature::Custom);
    let end = [b.pos[0], b.pos[1], b.pos[2] + 10.0];
    b.travel_to(end);
    b.finish()
}

/// A large synthetic toolpath with roughly `target_segments` segments:
/// stacked layers of a 150 x 150 mm block filled with dense zig-zag lines
/// split into short segments, perimeters and travels. Fast to generate.
pub fn stress_toolpath(target_segments: usize) -> Toolpath {
    let per_line_segments = 24usize;
    let spacing = 0.45f32;
    let size = 150.0f32;
    let lines_per_layer = (size / spacing) as usize;
    let per_layer = lines_per_layer * (per_line_segments + 1) + 64;
    let layers = (target_segments / per_layer).max(1);
    let mut tp = Toolpath {
        segments: Vec::with_capacity(layers * per_layer),
        ..Default::default()
    };
    let (x0, y0) = (35.0f32, 35.0f32);
    let mut line = 0u32;
    let mut time = 0.0f32;
    let h = 0.2f32;
    let features = [
        Feature::SolidInfill,
        Feature::InternalInfill,
        Feature::TopSolidInfill,
    ];
    for k in 0..layers {
        let z = 0.2 + k as f32 * h;
        let start = tp.segments.len();
        let line_start = line;
        let t0 = time;
        let feature = features[k % features.len()];
        let mut prev = [x0, y0, z];
        // Perimeter.
        let corners = [
            [x0, y0, z],
            [x0 + size, y0, z],
            [x0 + size, y0 + size, z],
            [x0, y0 + size, z],
            [x0, y0, z],
        ];
        for pair in corners.windows(2) {
            for s in 0..16 {
                let a = pair[0];
                let b = pair[1];
                let t1 = (s + 1) as f32 / 16.0;
                let p = [a[0] + (b[0] - a[0]) * t1, a[1] + (b[1] - a[1]) * t1, z];
                tp.segments.push(Segment {
                    start: prev,
                    end: p,
                    feedrate: 30.0,
                    width: 0.45,
                    height: h,
                    time,
                    duration: 0.0,
                    line,
                    layer: k as u32,
                    kind: MoveKind::Extrude,
                    feature: Feature::ExternalPerimeter,
                    tool: 0,
                    fan: 255,
                    extruded: 0.0,
                });
                line += 1;
                time += 0.31;
                prev = p;
            }
        }
        for i in 0..lines_per_layer {
            let y = y0 + 0.5 + i as f32 * spacing;
            let (xa, xb) = if i % 2 == 0 {
                (x0 + 0.5, x0 + size - 0.5)
            } else {
                (x0 + size - 0.5, x0 + 0.5)
            };
            let travel_to = [xa, y, z];
            tp.segments.push(Segment {
                start: prev,
                end: travel_to,
                feedrate: 150.0,
                time,
                line,
                layer: k as u32,
                kind: MoveKind::Travel,
                feature,
                ..Default::default()
            });
            line += 1;
            prev = travel_to;
            for s in 0..per_line_segments {
                let t1 = (s + 1) as f32 / per_line_segments as f32;
                let p = [xa + (xb - xa) * t1, y, z];
                tp.segments.push(Segment {
                    start: prev,
                    end: p,
                    feedrate: 60.0 + (i % 7) as f32 * 10.0,
                    width: 0.45,
                    height: h,
                    time,
                    duration: 0.1,
                    line,
                    layer: k as u32,
                    kind: MoveKind::Extrude,
                    feature,
                    tool: 0,
                    fan: 255,
                    extruded: 0.01,
                });
                line += 1;
                time += 0.1;
                prev = p;
            }
        }
        tp.layers.push(Layer {
            z,
            height: h,
            segments: start..tp.segments.len(),
            lines: line_start..line,
            time_start: t0,
            duration: time - t0,
            extruded: 0.0,
        });
        tp.bounds.include([x0, y0, z]);
        tp.bounds.include([x0 + size, y0 + size, z]);
    }
    tp.travel_bounds = tp.bounds;
    tp.line_count = line;
    tp.stats.estimated_time = time;
    tp.stats.layer_count = layers;
    tp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_is_consistent() {
        let tp = demo_toolpath();
        assert!(tp.segments.len() > 10_000, "{}", tp.segments.len());
        assert!(tp.layers.len() >= 100);
        // Layers tile the segment array in order.
        let mut next = 0;
        for l in &tp.layers {
            assert_eq!(l.segments.start, next);
            next = l.segments.end;
            assert!(l.lines.start <= l.lines.end);
        }
        assert_eq!(next, tp.segments.len());
        // Lines never decrease and every segment's layer matches its range.
        for w in tp.segments.windows(2) {
            assert!(w[0].line <= w[1].line);
        }
        for (i, l) in tp.layers.iter().enumerate() {
            for s in &tp.segments[l.segments.clone()] {
                assert_eq!(s.layer as usize, i);
            }
        }
        assert!(!tp.retractions.is_empty());
        assert!(tp.stats.estimated_time > 60.0);
        assert_eq!(tp.stats.tools_used, vec![0, 1]);
        let features: Vec<Feature> = tp.stats.features.iter().map(|f| f.feature).collect();
        for f in [
            Feature::Skirt,
            Feature::ExternalPerimeter,
            Feature::Perimeter,
            Feature::InternalInfill,
            Feature::SolidInfill,
            Feature::TopSolidInfill,
        ] {
            assert!(features.contains(&f), "missing {f:?}");
        }
        assert!(!tp.bounds.is_empty());
        assert!(tp.bounds.max[2] > 29.0);
    }

    #[test]
    fn stress_reaches_target() {
        let tp = stress_toolpath(200_000);
        assert!(tp.segments.len() >= 150_000);
        assert!(tp.segments.len() <= 260_000);
        assert_eq!(
            tp.layers.last().map(|l| l.segments.end),
            Some(tp.segments.len())
        );
    }
}
