//! Instance packing, layer/cluster ranges, scalar ranges and legends built
//! from toolpaths (well-formed and malformed).

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use sprint_gcode::{Feature, FeatureStat, Layer, MoveKind, Retraction, Segment, Toolpath};
use sprint_viewer::ColorMode;
use sprint_viewer::color::{Legend, build_legend};
use sprint_viewer::demo;
use sprint_viewer::instance::{
    CLUSTER_SIZE, RetractionInstance, SceneData, SegmentInstance, build_scene,
    extrusion_speed_range, layer_segment_ranges, total_time,
};

fn scene(tp: &Toolpath) -> SceneData {
    build_scene(tp, &AtomicBool::new(false)).expect("not cancelled")
}

#[test]
fn instance_layouts_are_compact() {
    assert_eq!(SegmentInstance::SIZE, 48);
    assert_eq!(std::mem::align_of::<SegmentInstance>(), 4);
    assert_eq!(RetractionInstance::SIZE, 24);
    let v = vec![SegmentInstance::default(); 3];
    assert_eq!(bytemuck::cast_slice::<_, u8>(&v).len(), 144);
}

#[test]
fn demo_scene_partitions_every_segment() {
    let tp = demo::demo_toolpath();
    let s = scene(&tp);
    assert_eq!(s.layers.len(), tp.layers.len());
    let extrusions = tp.segments.iter().filter(|x| x.is_extrusion()).count();
    assert_eq!(s.extrusions.len(), extrusions);
    assert_eq!(s.travels.len(), tp.segments.len() - extrusions);
    assert_eq!(s.retractions.len(), tp.retractions.len());

    // Layer ranges tile the instance arrays in order.
    let mut next_ext = 0;
    let mut next_travel = 0;
    for (i, layer) in s.layers.iter().enumerate() {
        assert_eq!(layer.extrusions.start, next_ext, "layer {i}");
        assert_eq!(layer.travels.start, next_travel, "layer {i}");
        next_ext = layer.extrusions.end;
        next_travel = layer.travels.end;
        // Every instance of the layer belongs to it, and the multiset of
        // source lines matches the toolpath's (order inside a layer may
        // change for spatial clustering).
        let mut expected: BTreeMap<u32, usize> = BTreeMap::new();
        for seg in &tp.segments[tp.layers[i].segments.clone()] {
            if seg.is_extrusion() {
                *expected.entry(seg.line).or_default() += 1;
            }
        }
        let mut got: BTreeMap<u32, usize> = BTreeMap::new();
        for inst in &s.extrusions[layer.extrusions.start as usize..layer.extrusions.end as usize] {
            assert_eq!(inst.layer as usize, i);
            *got.entry(inst.line).or_default() += 1;
        }
        assert_eq!(got, expected, "layer {i}");

        // Clusters cover the layer exactly, in order, and bound their beads.
        let clusters = &s.clusters[layer.clusters.start as usize..layer.clusters.end as usize];
        let mut cursor = layer.extrusions.start;
        for c in clusters {
            assert_eq!(c.instances.start, cursor);
            assert!(c.instances.end > c.instances.start);
            assert!((c.instances.end - c.instances.start) as usize <= CLUSTER_SIZE);
            cursor = c.instances.end;
            for inst in &s.extrusions[c.instances.start as usize..c.instances.end as usize] {
                for k in 0..3 {
                    for p in [inst.start[k], inst.end[k]] {
                        assert!(p >= c.min[k] - 1e-4 && p <= c.max[k] + 1e-4);
                    }
                }
                assert!(inst.width <= c.max_width);
            }
        }
        assert_eq!(cursor, layer.extrusions.end, "layer {i} cluster coverage");
    }
    assert_eq!(next_ext as usize, s.extrusions.len());
    assert_eq!(next_travel as usize, s.travels.len());
}

#[test]
fn big_layers_are_split_into_spatially_compact_clusters() {
    let tp = demo::stress_toolpath(60_000);
    let s = scene(&tp);
    let layer = &s.layers[0];
    let clusters = &s.clusters[layer.clusters.start as usize..layer.clusters.end as usize];
    let n = (layer.extrusions.end - layer.extrusions.start) as usize;
    assert_eq!(clusters.len(), n.div_ceil(CLUSTER_SIZE));
    assert!(clusters.len() >= 3);
    let layer_area = 150.0f32 * 150.0;
    let avg_area: f32 = clusters
        .iter()
        .map(|c| (c.max[0] - c.min[0]) * (c.max[1] - c.min[1]))
        .sum::<f32>()
        / clusters.len() as f32;
    assert!(
        avg_area < layer_area * 0.5,
        "clusters should cover a fraction of the layer ({avg_area} vs {layer_area})"
    );
}

#[test]
fn scalar_ranges_match_the_toolpath() {
    let tp = demo::demo_toolpath();
    let s = scene(&tp);
    let r = &s.legend.ranges;
    assert_eq!(r.speed, extrusion_speed_range(&tp));
    assert_eq!(r.layer_count, tp.layers.len());
    assert_eq!(r.total_time, tp.stats.estimated_time);
    assert_eq!(total_time(&tp), tp.stats.estimated_time);
    let widths: Vec<f32> = tp
        .segments
        .iter()
        .filter(|x| x.is_extrusion())
        .map(|x| x.width)
        .collect();
    let wmin = widths.iter().copied().fold(f32::INFINITY, f32::min);
    let wmax = widths.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    assert_eq!((r.width.min, r.width.max), (wmin, wmax));
    assert_eq!((r.fan.min, r.fan.max), (0.0, 255.0));
    assert!(r.height.min > 0.0 && r.height.max >= r.height.min);
}

#[test]
fn legends_for_every_mode() {
    let tp = demo::demo_toolpath();
    let s = scene(&tp);
    for mode in ColorMode::ALL {
        match build_legend(&s.legend, mode) {
            Legend::Features(rows) => {
                assert_eq!(mode, ColorMode::Feature);
                let total: f32 = rows.iter().filter_map(|r| r.time_share).sum();
                assert!((total - 1.0).abs() < 1e-3, "shares sum to {total}");
                assert!(rows.iter().any(|r| r.feature == Feature::Skirt));
                assert!(rows.iter().all(|r| r.segments > 0));
            }
            Legend::Tools(rows) => {
                assert_eq!(mode, ColorMode::Tool);
                assert_eq!(rows.iter().map(|r| r.tool).collect::<Vec<_>>(), vec![0, 1]);
            }
            Legend::Gradient(g) => {
                assert!(mode.is_gradient());
                assert!(g.max >= g.min);
                assert!(!g.ticks.is_empty());
                assert!(g.ticks.windows(2).all(|w| w[0].0 < w[1].0));
            }
            Legend::Empty => panic!("{mode:?} has data"),
        }
    }
}

#[test]
fn time_fraction_and_speed_survive_packing() {
    let tp = demo::demo_toolpath();
    let s = scene(&tp);
    let total = tp.stats.estimated_time;
    let by_line: BTreeMap<u32, &Segment> = tp
        .segments
        .iter()
        .filter(|x| x.is_extrusion())
        .map(|x| (x.line, x))
        .collect();
    for inst in s.extrusions.iter().step_by(97) {
        let seg = by_line[&inst.line];
        assert!((inst.speed() - seg.feedrate).abs() <= seg.feedrate * 1e-3);
        assert!((inst.time_fraction() - seg.time / total).abs() < 1e-4);
        assert_eq!(inst.feature(), seg.feature);
        assert_eq!((inst.tool(), inst.fan()), (seg.tool, seg.fan));
    }
}

#[test]
fn start_gcode_does_not_widen_the_bounds() {
    let mut segments = vec![Segment {
        start: [0.0, 0.0, 0.2],
        end: [200.0, 0.0, 0.2],
        width: 0.6,
        height: 0.2,
        kind: MoveKind::Extrude,
        feature: Feature::Custom,
        ..Default::default()
    }];
    segments.push(Segment {
        start: [100.0, 100.0, 0.2],
        end: [110.0, 100.0, 0.2],
        width: 0.4,
        height: 0.2,
        line: 1,
        kind: MoveKind::Extrude,
        feature: Feature::Perimeter,
        ..Default::default()
    });
    let tp = Toolpath {
        segments,
        ..Default::default()
    };
    let (min, max) = scene(&tp).bounds.expect("bounds");
    assert!(min[0] > 99.0 && max[0] < 111.0, "{min:?} {max:?}");
    // Only custom extrusions: then they define the bounds.
    let only_custom = Toolpath {
        segments: vec![tp.segments[0]],
        ..Default::default()
    };
    let (min, max) = scene(&only_custom).bounds.expect("bounds");
    assert!(min[0] < 0.0 && max[0] > 199.0);
    assert_eq!(scene(&Toolpath::default()).bounds, None);
}

#[test]
fn malformed_toolpaths_produce_sane_scenes() {
    let ok = Segment {
        start: [0.0, 0.0, 0.2],
        end: [5.0, 0.0, 0.2],
        width: 0.4,
        height: 0.2,
        kind: MoveKind::Extrude,
        ..Default::default()
    };
    let mut nan = ok;
    nan.end = [f32::NAN, 0.0, 0.0];
    let mut inf_time = ok;
    inf_time.time = f32::INFINITY;
    inf_time.duration = f32::NAN;
    inf_time.feedrate = f32::NEG_INFINITY;
    let travel = Segment {
        kind: MoveKind::Travel,
        ..ok
    };
    #[allow(clippy::reversed_empty_ranges)]
    let tp = Toolpath {
        segments: vec![ok, nan, inf_time, travel, ok],
        layers: vec![
            Layer {
                segments: 3..1,
                ..Default::default()
            },
            Layer {
                segments: 0..4,
                ..Default::default()
            },
            Layer {
                segments: 2..99,
                ..Default::default()
            },
        ],
        retractions: vec![
            Retraction {
                pos: [f32::NAN, 0.0, 0.0],
                amount: -1.0,
                ..Default::default()
            },
            Retraction {
                pos: [1.0, 1.0, 1.0],
                amount: f32::NAN,
                ..Default::default()
            },
            Retraction {
                pos: [1.0, 1.0, 1.0],
                amount: 0.8,
                ..Default::default()
            },
        ],
        stats: sprint_gcode::Stats {
            estimated_time: f32::NAN,
            features: vec![FeatureStat {
                feature: Feature::Perimeter,
                time: f32::INFINITY,
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let ranges = layer_segment_ranges(&tp);
    assert!(
        ranges
            .iter()
            .all(|r| r.start <= r.end && r.end <= tp.segments.len())
    );
    let s = scene(&tp);
    assert_eq!(s.layers.len(), 3);
    assert_eq!(s.extrusions.len(), 3, "the NaN segment is dropped");
    assert_eq!(s.retractions.len(), 1);
    assert!(s.retractions[0].is_prime());
    for layer in &s.layers {
        assert!(layer.extrusions.start <= layer.extrusions.end);
        assert!(layer.extrusions.end as usize <= s.extrusions.len());
        assert!(layer.clusters.end as usize <= s.clusters.len());
    }
    for mode in ColorMode::ALL {
        let _ = build_legend(&s.legend, mode);
    }
    assert!(s.legend.ranges.total_time.is_finite());
}

#[test]
fn toolpaths_without_layers_get_one_synthetic_layer() {
    let tp = Toolpath {
        segments: vec![Segment {
            start: [0.0; 3],
            end: [1.0, 0.0, 0.0],
            kind: MoveKind::Extrude,
            ..Default::default()
        }],
        ..Default::default()
    };
    let s = scene(&tp);
    assert_eq!(s.layers.len(), 1);
    assert_eq!(s.layers[0].extrusions, 0..1);
    assert_eq!(scene(&Toolpath::default()).layers.len(), 0);
}

#[test]
fn building_can_be_cancelled() {
    let tp = demo::demo_toolpath();
    assert!(build_scene(&tp, &AtomicBool::new(true)).is_none());
}
