//! Picking: the accelerated index agrees with brute force, respects the
//! visibility filters, survives malformed data and stays fast.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use glam::{Vec2, Vec3};
use sprint_gcode::{Feature, Layer, MoveKind, Segment, Toolpath};
use sprint_viewer::ViewPreset;
use sprint_viewer::camera::{Camera, Projection, Ray};
use sprint_viewer::demo;
use sprint_viewer::pick::{PickFilter, PickIndex, pick_brute_force};

const SIZE: Vec2 = Vec2::new(640.0, 480.0);

fn build(tp: &Toolpath) -> PickIndex {
    PickIndex::build(tp, &AtomicBool::new(false)).expect("not cancelled")
}

fn extrude(start: [f32; 3], end: [f32; 3], line: u32, layer: u32, feature: Feature) -> Segment {
    Segment {
        start,
        end,
        feedrate: 40.0,
        width: 0.45,
        height: 0.2,
        line,
        layer,
        kind: MoveKind::Extrude,
        feature,
        ..Default::default()
    }
}

fn layered(segments: Vec<Segment>) -> Toolpath {
    let mut layers: Vec<Layer> = Vec::new();
    for (i, s) in segments.iter().enumerate() {
        let li = s.layer as usize;
        while layers.len() <= li {
            layers.push(Layer {
                z: s.end[2],
                height: 0.2,
                segments: i..i,
                ..Default::default()
            });
        }
        layers[li].segments.end = i + 1;
    }
    Toolpath {
        segments,
        layers,
        ..Default::default()
    }
}

/// Rays through a grid of pixels for several camera set-ups.
fn rays_for(tp: &Toolpath) -> Vec<Ray> {
    let b = tp.bounds;
    let (min, max) = (Vec3::from(b.min), Vec3::from(b.max));
    let mut rays = Vec::new();
    for projection in [Projection::Perspective, Projection::Orthographic] {
        for preset in [
            ViewPreset::Iso,
            ViewPreset::Top,
            ViewPreset::Front,
            ViewPreset::Left,
        ] {
            let mut cam = Camera {
                projection,
                ..Camera::default()
            };
            cam.set_preset(preset);
            cam.fit_bounds(min, max, SIZE.x / SIZE.y);
            for i in 0..24 {
                for j in 0..18 {
                    let p = Vec2::new((i as f32 + 0.37) / 24.0, (j as f32 + 0.61) / 18.0) * SIZE;
                    rays.push(cam.ray_from_screen(p, SIZE));
                }
            }
        }
    }
    rays
}

fn assert_same(tp: &Toolpath, index: &PickIndex, ray: &Ray, filter: &PickFilter, radius: f32) {
    let fast = index.pick(tp, ray, filter, radius);
    let slow = pick_brute_force(tp, ray, filter, radius);
    match (fast, slow) {
        (None, None) => {}
        (Some(a), Some(b)) => assert!(
            (a.t - b.t).abs() <= 1e-3 * b.t.max(1.0),
            "index hit {} at t={} but brute force hit {} at t={}",
            a.segment,
            a.t,
            b.segment,
            b.t
        ),
        (a, b) => panic!("index {a:?} vs brute force {b:?} for {ray:?}"),
    }
}

#[test]
fn index_matches_brute_force_on_the_demo_print() {
    let tp = demo::demo_toolpath();
    let index = build(&tp);
    assert_eq!(index.layer_count(), tp.layers.len());
    let filter = PickFilter::default();
    let rays = rays_for(&tp);
    let mut hits = 0;
    for ray in &rays {
        assert_same(&tp, &index, ray, &filter, 0.0);
        hits += usize::from(index.pick(&tp, ray, &filter, 0.0).is_some());
    }
    assert!(hits > 100, "only {hits} of {} rays hit", rays.len());
}

#[test]
fn index_matches_brute_force_with_filters_and_tolerance() {
    let tp = demo::demo_toolpath();
    let index = build(&tp);
    let mid = tp.layers.len() / 2;
    let filters = [
        PickFilter {
            layers: (0, mid),
            ..Default::default()
        },
        PickFilter {
            hidden_mask: 1 << Feature::ExternalPerimeter.index(),
            ..Default::default()
        },
        PickFilter {
            layers: (0, 30),
            line_limit: tp.layers.get(30).map(|l| (l.lines.start + 40, 30)),
            ..Default::default()
        },
    ];
    for filter in &filters {
        for ray in rays_for(&tp).iter().step_by(3) {
            assert_same(&tp, &index, ray, filter, 0.3);
        }
    }
}

#[test]
fn filters_hide_what_they_should() {
    let tp = demo::demo_toolpath();
    let index = build(&tp);
    let all = PickFilter::default();
    let no_ext = PickFilter {
        hidden_mask: 1 << Feature::ExternalPerimeter.index(),
        ..Default::default()
    };
    let low = PickFilter {
        layers: (0, 10),
        ..Default::default()
    };
    for ray in rays_for(&tp) {
        if let Some(hit) = index.pick(&tp, &ray, &no_ext, 0.0) {
            assert_ne!(tp.segments[hit.segment].feature, Feature::ExternalPerimeter);
        }
        if let Some(hit) = index.pick(&tp, &ray, &low, 0.0) {
            assert!(tp.segments[hit.segment].layer <= 10);
        }
        if let Some(hit) = index.pick(&tp, &ray, &all, 0.0) {
            assert!(tp.segments[hit.segment].is_extrusion());
        }
    }
}

#[test]
fn line_limit_only_applies_to_the_top_layers() {
    // Two stacked segments; the top one is beyond the line limit.
    let tp = layered(vec![
        extrude([0.0, 0.0, 0.2], [10.0, 0.0, 0.2], 10, 0, Feature::Perimeter),
        extrude([0.0, 0.0, 0.4], [10.0, 0.0, 0.4], 20, 1, Feature::Perimeter),
    ]);
    let index = build(&tp);
    let ray = Ray {
        origin: Vec3::new(5.0, 0.0, 50.0),
        dir: -Vec3::Z,
    };
    let hit = index
        .pick(&tp, &ray, &PickFilter::default(), 0.0)
        .expect("hit");
    assert_eq!(hit.segment, 1);
    let limited = PickFilter {
        layers: (0, 1),
        line_limit: Some((15, 1)),
        ..Default::default()
    };
    let hit = index.pick(&tp, &ray, &limited, 0.0).expect("hit below");
    assert_eq!(hit.segment, 0);
    // A limit below the top layer does not cut lower layers.
    let limited = PickFilter {
        layers: (0, 1),
        line_limit: Some((5, 1)),
        ..Default::default()
    };
    assert_eq!(
        index.pick(&tp, &ray, &limited, 0.0).map(|h| h.segment),
        Some(0)
    );
}

#[test]
fn pick_tolerance_above_a_bead_is_inside_the_index() {
    // A wide pixel tolerance makes the bead's pick volume round: its
    // vertical radius grows to the (wider) horizontal one. The index boxes
    // must contain that volume, or a side-on ray grazing the top of the
    // print hits for brute force but not for the index.
    let mut segments = vec![extrude(
        [0.0, 0.0, 0.2],
        [10.0, 0.0, 0.2],
        0,
        0,
        Feature::Perimeter,
    )];
    // A second, dense layer exercises the grid path too.
    for i in 0..400u32 {
        let y = 5.0 + i as f32 * 0.45;
        segments.push(extrude(
            [0.0, y, 0.4],
            [10.0, y, 0.4],
            i + 1,
            1,
            Feature::SolidInfill,
        ));
    }
    let tp = layered(segments);
    let index = build(&tp);
    for (z, y) in [(0.1 + 0.9, 0.0), (0.3 + 0.9, 50.0)] {
        // Axis of the bead is at nozzle Z - height / 2.
        let ray = Ray {
            origin: Vec3::new(5.0, y - 30.0, z),
            dir: Vec3::Y,
        };
        let filter = PickFilter::default();
        let slow = pick_brute_force(&tp, &ray, &filter, 2.0);
        assert!(slow.is_some(), "z {z}: within the tolerance");
        assert_same(&tp, &index, &ray, &filter, 2.0);
    }
}

#[test]
fn hit_point_lies_on_the_bead_surface() {
    let tp = layered(vec![extrude(
        [0.0, 0.0, 0.2],
        [10.0, 0.0, 0.2],
        0,
        0,
        Feature::Perimeter,
    )]);
    let index = build(&tp);
    let ray = Ray {
        origin: Vec3::new(4.0, 0.0, 30.0),
        dir: -Vec3::Z,
    };
    let hit = index
        .pick(&tp, &ray, &PickFilter::default(), 0.0)
        .expect("hit");
    // Bead top is at the nozzle height.
    assert!((hit.point.z - 0.2).abs() < 1e-3, "{:?}", hit.point);
    assert!((hit.t - 29.8).abs() < 1e-3);
}

/// A spiral vase with `n` segments of `step` radians, each its own layer
/// (vase mode as a Z-change layer detector reports it).
fn vase(n: u32, step: f32) -> Toolpath {
    let mut segments = Vec::new();
    let mut prev = [20.0f32, 0.0, 0.2];
    for i in 1..=n {
        let a = i as f32 * step;
        let z = 0.2 + a / std::f32::consts::TAU * 0.2;
        let p = [20.0 * a.cos(), 20.0 * a.sin(), z];
        segments.push(extrude(prev, p, i, i - 1, Feature::ExternalPerimeter));
        prev = p;
    }
    let mut tp = layered(segments);
    for s in &tp.segments {
        tp.bounds.include(s.start);
        tp.bounds.include(s.end);
    }
    tp
}

#[test]
fn spiral_vase_with_one_layer_per_segment() {
    // Regression: vase mode can produce a "layer" per segment.
    let tp = vase(4000, 0.06);
    assert_eq!(tp.layers.len(), tp.segments.len());
    let index = build(&tp);
    let filters = [
        PickFilter::default(),
        PickFilter {
            layers: (1000, 2777),
            ..Default::default()
        },
        PickFilter {
            layers: (37, 38),
            ..Default::default()
        },
    ];
    let mut hits = 0;
    for filter in &filters {
        for ray in rays_for(&tp).iter().step_by(2) {
            assert_same(&tp, &index, ray, filter, 0.0);
            hits += usize::from(index.pick(&tp, ray, filter, 0.0).is_some());
        }
    }
    assert!(hits > 0);
}

#[test]
fn picking_stays_fast_with_one_layer_per_segment() {
    // 300k layers: testing every layer's box per pick took ~3.5 ms on
    // average (25 ms worst) even in release builds.
    let tp = vase(300_000, std::f32::consts::TAU / 200.0);
    let index = build(&tp);
    let (avg, worst, hits) = timed_picks(&tp, &index);
    println!("vase, 300k layers: avg {avg:.3} ms, worst {worst:.3} ms, {hits} hits");
    assert!(hits > 0);
    assert!(avg < 1.0, "average pick {avg:.2} ms");
    let filter = PickFilter {
        layers: (100_000, 200_000),
        ..Default::default()
    };
    for ray in rays_for(&tp).iter().step_by(23) {
        assert_same(&tp, &index, ray, &filter, 0.2);
    }
}

#[test]
fn malformed_toolpaths_do_not_panic() {
    let good = extrude([0.0, 0.0, 0.2], [10.0, 0.0, 0.2], 1, 0, Feature::Perimeter);
    let nan = extrude(
        [f32::NAN, 0.0, 0.2],
        [10.0, 0.0, 0.2],
        2,
        0,
        Feature::Perimeter,
    );
    let huge = extrude(
        [0.0, 0.0, 0.2],
        [f32::MAX, 0.0, 0.2],
        3,
        0,
        Feature::Perimeter,
    );
    let mut weird_width = good;
    weird_width.width = f32::INFINITY;
    weird_width.height = -3.0;
    let cases = [
        // No layers at all.
        Toolpath {
            segments: vec![good, nan, huge, weird_width],
            ..Default::default()
        },
        // Layer ranges out of bounds and inverted.
        Toolpath {
            segments: vec![good, nan],
            layers: vec![
                Layer {
                    segments: 5..50,
                    ..Default::default()
                },
                #[allow(clippy::reversed_empty_ranges)]
                Layer {
                    segments: 2..0,
                    ..Default::default()
                },
                Layer {
                    segments: 0..2,
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        // Overlapping layers.
        Toolpath {
            segments: vec![good, weird_width, good],
            layers: vec![
                Layer {
                    segments: 0..2,
                    ..Default::default()
                },
                Layer {
                    segments: 1..3,
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        Toolpath::default(),
    ];
    let ray = Ray {
        origin: Vec3::new(5.0, 0.0, 30.0),
        dir: -Vec3::Z,
    };
    for tp in &cases {
        let index = build(tp);
        for filter in [
            PickFilter::default(),
            PickFilter {
                layers: (7, 2),
                line_limit: Some((0, 99)),
                hidden_mask: u32::MAX,
            },
        ] {
            let _ = index.pick(tp, &ray, &filter, f32::NAN);
            let _ = pick_brute_force(tp, &ray, &filter, 1.0);
            let bad_ray = Ray {
                origin: Vec3::splat(f32::NAN),
                dir: Vec3::Z,
            };
            let _ = index.pick(tp, &bad_ray, &filter, 0.0);
        }
    }
    // The first case still finds the valid bead.
    let index = build(&cases[0]);
    assert!(
        index
            .pick(&cases[0], &ray, &PickFilter::default(), 0.0)
            .is_some()
    );
}

#[test]
fn an_index_never_answers_for_another_toolpath() {
    let tp = demo::demo_toolpath();
    let index = build(&tp);
    let other = layered(vec![extrude(
        [0.0, 0.0, 0.2],
        [1.0, 0.0, 0.2],
        0,
        0,
        Feature::Perimeter,
    )]);
    let ray = Ray {
        origin: Vec3::new(0.5, 0.0, 10.0),
        dir: -Vec3::Z,
    };
    assert!(
        index
            .pick(&other, &ray, &PickFilter::default(), 0.0)
            .is_none()
    );
}

#[test]
fn building_can_be_cancelled() {
    let tp = demo::demo_toolpath();
    assert!(PickIndex::build(&tp, &AtomicBool::new(true)).is_none());
}

fn timed_picks(tp: &Toolpath, index: &PickIndex) -> (f64, f64, usize) {
    let filter = PickFilter::default();
    let rays = rays_for(tp);
    let mut worst = 0.0f64;
    let mut total = 0.0f64;
    let mut hits = 0;
    for ray in &rays {
        let t = Instant::now();
        hits += usize::from(index.pick(tp, ray, &filter, 0.2).is_some());
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst = worst.max(ms);
        total += ms;
    }
    (total / rays.len() as f64, worst, hits)
}

#[test]
fn picking_stays_fast_on_a_million_segments() {
    let tp = demo::stress_toolpath(1_000_000);
    let index = build(&tp);
    let (avg, worst, hits) = timed_picks(&tp, &index);
    println!("1M segments: avg {avg:.3} ms, worst {worst:.3} ms, {hits} hits");
    assert!(hits > 0);
    // Generous bounds so unoptimised debug builds pass; release is ~100x
    // faster (see the bench example).
    assert!(avg < 5.0, "average pick {avg:.2} ms");
}

#[test]
#[ignore = "slow in debug builds; run with --release -- --ignored"]
fn picking_stays_fast_on_five_million_segments() {
    let tp = demo::stress_toolpath(5_000_000);
    let index = build(&tp);
    let (avg, worst, hits) = timed_picks(&tp, &index);
    println!("5M segments: avg {avg:.3} ms, worst {worst:.3} ms, {hits} hits");
    assert!(avg < 5.0 && worst < 20.0);
}
