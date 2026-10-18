//! Integration tests over the committed sample files in `samples/`.

mod common;

#[allow(dead_code)]
#[path = "../examples/gen_samples.rs"]
mod gen_samples;

use std::collections::BTreeSet;
use std::f32::consts::TAU;

use common::{check_toolpath, close};
use sprint_gcode::{
    AnalyzeOptions, Feature, GcodeFile, MoveKind, ResumeOptions, ThumbnailFormat, Toolpath,
    analyze, analyze_simple, clean_line, parse_line, read_metadata, resume_preamble, state_at_line,
};

fn load(name: &str) -> GcodeFile {
    let path = gen_samples::samples_dir().join(name);
    GcodeFile::load(&path).unwrap_or_else(|e| panic!("cannot load {}: {e}", path.display()))
}

fn analyzed(name: &str) -> (GcodeFile, Toolpath) {
    let file = load(name);
    let tp = analyze_simple(&file, &AnalyzeOptions::default());
    check_toolpath(&tp);
    (file, tp)
}

fn features(tp: &Toolpath, layers: std::ops::Range<usize>) -> BTreeSet<Feature> {
    tp.layers[layers]
        .iter()
        .flat_map(|l| tp.segments[l.segments.clone()].iter())
        .filter(|s| s.is_extrusion())
        .map(|s| s.feature)
        .collect()
}

#[test]
fn samples_are_up_to_date_and_small() {
    for (name, generated) in gen_samples::samples() {
        let path = gen_samples::samples_dir().join(name);
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
            .replace("\r\n", "\n");
        assert!(
            committed == generated,
            "{name} is stale: run `cargo run -p sprint-gcode --example gen_samples`"
        );
        assert!(committed.len() < 4 * 1024 * 1024, "{name} is too large");
    }
}

#[test]
fn every_sample_line_round_trips_through_clean_line() {
    for name in [
        "calibration-cube.gcode",
        "twisted-vase.gcode",
        "arcs-test.gcode",
    ] {
        let file = load(name);
        let mut commands = 0;
        for line in file.lines() {
            let original = parse_line(line);
            match clean_line(line) {
                Some(cleaned) => {
                    commands += 1;
                    assert!(!cleaned.contains(';'), "{line}");
                    let again = parse_line(&cleaned);
                    assert_eq!(original.code, again.code, "{line}");
                    assert_eq!(original.text, again.text, "{line}");
                    assert_eq!(original.words.len(), again.words.len(), "{line}");
                    for (a, b) in original.words.iter().zip(&again.words) {
                        assert_eq!(a.letter, b.letter, "{line}");
                        assert!(
                            a.value == b.value || a.value.is_nan() && b.value.is_nan(),
                            "{line}"
                        );
                    }
                }
                None => assert!(original.is_empty() && original.text.is_none(), "{line}"),
            }
        }
        assert_eq!(commands, file.command_count(), "{name}");
    }
}

#[test]
fn calibration_cube_metadata() {
    let file = load("calibration-cube.gcode");
    let m = read_metadata(&file);
    assert_eq!(m.slicer.as_deref(), Some("PrusaSlicer 2.7.1"));
    assert_eq!(m.flavor.as_deref(), Some("marlin2"));
    assert_eq!(m.layer_height, Some(0.2));
    assert_eq!(m.first_layer_height, Some(0.2));
    assert_eq!(m.nozzle_diameter, Some(0.4));
    assert_eq!(m.filament_diameter, Some(1.75));
    assert_eq!(m.filament_density, Some(1.24));
    assert_eq!(m.filament_type.as_deref(), Some("PLA"));
    assert_eq!(m.hotend_temp, Some(215.0));
    assert_eq!(m.bed_temp, Some(60.0));
    assert!(m.estimated_time.is_some_and(|t| t > 60.0));
    assert!(m.filament_used_g.is_some_and(|g| g > 1.0));
    assert!(
        m.settings
            .contains(&("perimeters".to_string(), "2".to_string()))
    );
    assert!(
        m.settings
            .contains(&("fill_density".to_string(), "20%".to_string()))
    );
    assert_eq!(
        m.settings.first().map(|s| s.0.as_str()),
        Some("avoid_crossing_perimeters")
    );

    // Two valid PNG thumbnails.
    let sizes: Vec<(u32, u32)> = m.thumbnails.iter().map(|t| (t.width, t.height)).collect();
    assert_eq!(sizes, vec![(16, 16), (220, 124)]);
    for t in &m.thumbnails {
        assert_eq!(t.format, ThumbnailFormat::Png);
        let img = image::load_from_memory_with_format(&t.data, image::ImageFormat::Png)
            .unwrap_or_else(|e| panic!("thumbnail {}x{} does not decode: {e}", t.width, t.height));
        assert_eq!((img.width(), img.height()), (t.width, t.height));
        let rgba = img.to_rgba8();
        // Transparent corner, opaque orange centre.
        assert_eq!(rgba.get_pixel(0, 0)[3], 0);
        let c = rgba.get_pixel(t.width / 2, t.height / 2);
        assert_eq!(c[3], 255);
        assert!(c[0] > c[2], "orange-ish centre");
    }
}

#[test]
fn calibration_cube_toolpath() {
    let (file, tp) = analyzed("calibration-cube.gcode");
    let m = &tp.metadata;
    assert_eq!(tp.layers.len(), 100);
    for (i, layer) in tp.layers.iter().enumerate() {
        assert!(
            (layer.z - 0.2 * (i + 1) as f32).abs() < 1e-4,
            "layer {i} z {}",
            layer.z
        );
        assert!((layer.height - 0.2).abs() < 1e-4);
        assert!(layer.extruded > 0.0 && layer.duration > 0.0);
    }
    assert!((tp.stats.layer_height - 0.2).abs() < 1e-6);
    assert!((tp.stats.first_layer_height - 0.2).abs() < 1e-6);

    // Features per layer.
    let first = features(&tp, 0..1);
    for f in [
        Feature::Custom,
        Feature::Skirt,
        Feature::Perimeter,
        Feature::ExternalPerimeter,
        Feature::SolidInfill,
    ] {
        assert!(first.contains(&f), "{f:?} missing on layer 1");
    }
    assert_eq!(
        features(&tp, 1..3),
        [
            Feature::ExternalPerimeter,
            Feature::Perimeter,
            Feature::SolidInfill
        ]
        .into()
    );
    assert_eq!(
        features(&tp, 3..97),
        [
            Feature::ExternalPerimeter,
            Feature::Perimeter,
            Feature::InternalInfill
        ]
        .into()
    );
    assert_eq!(
        features(&tp, 97..99),
        [
            Feature::ExternalPerimeter,
            Feature::Perimeter,
            Feature::SolidInfill
        ]
        .into()
    );
    assert_eq!(
        features(&tp, 99..100),
        [
            Feature::ExternalPerimeter,
            Feature::Perimeter,
            Feature::TopSolidInfill
        ]
        .into()
    );
    assert!(!features(&tp, 1..100).contains(&Feature::Skirt));

    // Widths from ;WIDTH:, heights from ;HEIGHT:.
    for s in tp
        .segments
        .iter()
        .filter(|s| s.is_extrusion() && s.feature != Feature::Custom)
    {
        assert!((s.height - 0.2).abs() < 1e-6);
        let expected = match (s.layer, s.feature) {
            (0, _) => 0.42,
            (_, Feature::TopSolidInfill) => 0.40,
            _ => 0.45,
        };
        assert!((s.width - expected).abs() < 1e-6, "{s:?}");
    }
    // The purge line has no ;WIDTH: and gets a computed width.
    let purge = tp
        .segments
        .iter()
        .find(|s| s.is_extrusion() && s.feature == Feature::Custom)
        .map(|s| s.width);
    assert!(purge.is_some_and(|w| w > 0.5 && w < 2.0), "{purge:?}");

    // The cube's external perimeter is 20 mm wide minus half a line.
    let ext: Vec<_> = tp
        .segments
        .iter()
        .filter(|s| s.feature == Feature::ExternalPerimeter)
        .collect();
    let min_x = ext
        .iter()
        .map(|s| s.start[0].min(s.end[0]))
        .fold(f32::MAX, f32::min);
    let max_x = ext
        .iter()
        .map(|s| s.start[0].max(s.end[0]))
        .fold(f32::MIN, f32::max);
    assert!((max_x - min_x - 19.55).abs() < 1e-3, "{min_x} {max_x}");
    assert!((tp.bounds.max[2] - 20.0).abs() < 1e-4);
    assert!(tp.travel_bounds.max[2] >= 50.0, "end G-code lifts the head");

    // Retractions with Z hop.
    assert!(tp.stats.retractions > 100);
    assert!(
        tp.retractions
            .iter()
            .all(|r| (r.amount.abs() - 0.8).abs() < 1e-6)
    );
    let hops = tp
        .segments
        .iter()
        .filter(|s| s.kind == MoveKind::Travel && s.end[2] > s.start[2] + 0.3 && s.end[2] < 21.0)
        .count();
    assert!(hops >= 100, "{hops} z hops");
    // No layer is created by the hops.
    assert!(
        tp.layers
            .iter()
            .all(|l| ((l.z / 0.2).round() * 0.2 - l.z).abs() < 1e-4)
    );

    // Fan off on the first layer, full afterwards.
    assert!(
        tp.segments[tp.layers[0].segments.clone()]
            .iter()
            .all(|s| s.fan == 0)
    );
    assert!(
        tp.segments[tp.layers[1].segments.end..]
            .iter()
            .filter(|s| s.is_extrusion())
            .all(|s| s.fan == 255)
    );

    // Stats.
    let st = &tp.stats;
    assert!(st.uses_relative_e && !st.uses_arcs && !st.uses_firmware_retraction);
    assert_eq!(st.heat_waits, 2);
    assert_eq!(st.hotend_temps, vec![(0, 215.0)]);
    assert_eq!(st.bed_temps, vec![60.0]);
    assert_eq!(st.tools_used, vec![0]);
    assert_eq!(st.max_print_speed, 80.0);
    assert!(
        (st.min_print_speed - 1000.0 / 60.0).abs() < 1e-3,
        "purge line speed"
    );
    assert_eq!(st.max_travel_speed, 180.0);
    // Header figures were produced by Sprint's own estimator.
    let header_time = m.estimated_time.unwrap_or_default();
    assert!(
        (header_time - st.estimated_time).abs() <= 1.0,
        "{header_time} vs {}",
        st.estimated_time
    );
    let header_mm = m.filament_used_mm.unwrap_or_default();
    assert!(close(header_mm, st.filament_length, 1e-4));
    assert!(st.estimated_time > 600.0 && st.estimated_time < 3600.0);

    // Progress helpers.
    let mid = tp.layers[50].lines.start;
    let p = tp.progress_at_line(mid);
    assert!(p > 0.3 && p < 0.7, "{p}");
    assert_eq!(tp.layer_at_line(mid), Some(50));
    assert_eq!(tp.layer_at_z(10.0), Some(49));
    assert!(close(
        tp.time_at_line(tp.line_count),
        st.estimated_time,
        1e-3
    ));

    // State and resume in the middle of the print.
    let state = state_at_line(&file, mid as usize);
    // The line right after the last extrusion of layer 50 (Z 10).
    assert!(
        (state.position[2] - 10.0).abs() < 1e-9,
        "{}",
        state.position[2]
    );
    assert_eq!(state.fan, 255);
    assert!(state.relative_e_mode);
    let pre = resume_preamble(&file, mid as usize, &ResumeOptions::default());
    for expected in [
        "M190 S60",
        "M109 S215",
        "G28 X Y",
        "M420 S1",
        "M106 S255",
        "M83",
    ] {
        assert!(pre.contains(&expected.to_string()), "{expected}: {pre:?}");
    }
    assert!(!pre.iter().any(|l| l == "G28"));
}

#[test]
fn twisted_vase_metadata() {
    let file = load("twisted-vase.gcode");
    let m = read_metadata(&file);
    assert_eq!(m.slicer.as_deref(), Some("Cura_SteamEngine 5.6.0"));
    assert_eq!(m.flavor.as_deref(), Some("Marlin"));
    assert_eq!(m.layer_count, Some(300));
    assert_eq!(m.layer_height, Some(0.2));
    assert_eq!(m.object_names, vec!["twisted-vase.stl".to_string()]);
    assert_eq!(m.hotend_temp, Some(210.0));
    assert_eq!(m.bed_temp, Some(60.0));
    assert!(m.thumbnails.is_empty());
    assert!(m.settings.is_empty());
}

#[test]
fn twisted_vase_toolpath() {
    let (file, tp) = analyzed("twisted-vase.gcode");
    assert_eq!(tp.layers.len(), 300);
    for (i, layer) in tp.layers.iter().enumerate() {
        assert!(
            (layer.z - 0.2 * (i + 1) as f32).abs() < 2e-3,
            "layer {i} z {}",
            layer.z
        );
        assert!(
            (layer.height - 0.2).abs() < 2e-3,
            "layer {i} height {}",
            layer.height
        );
    }
    // The purge line (Z 0.28, before ;LAYER:0) belongs to the first layer.
    let l0 = &tp.layers[0];
    assert_eq!(l0.segments.start, 0);
    assert!(
        tp.segments[l0.segments.clone()]
            .iter()
            .any(|s| s.is_extrusion()
                && s.feature == Feature::Custom
                && (s.end[2] - 0.28).abs() < 1e-5)
    );
    // Bottom layers: wall and skin. Spiral layers: rising outer wall only.
    assert_eq!(
        features(&tp, 1..3),
        [Feature::ExternalPerimeter, Feature::SolidInfill].into()
    );
    for layer in &tp.layers[3..] {
        let ext: Vec<_> = tp.segments[layer.segments.clone()]
            .iter()
            .filter(|s| s.is_extrusion())
            .collect();
        assert!(ext.len() >= 140);
        assert!(ext.iter().all(|s| s.feature == Feature::ExternalPerimeter));
        assert!(ext.iter().all(|s| s.end[2] > s.start[2]));
    }
    assert!((tp.bounds.max[2] - 60.0).abs() < 1e-4);
    let size = tp.bounds.size();
    assert!(size[2] > 59.0);
    // Cura header and per-layer elapsed times agree with the analysis.
    let m = &tp.metadata;
    assert!(
        m.estimated_time
            .is_some_and(|t| (t - tp.stats.estimated_time).abs() <= 1.0)
    );
    assert!(
        m.filament_used_mm
            .is_some_and(|mm| close(mm, tp.stats.filament_length, 1e-4))
    );
    let elapsed: Vec<f32> = file
        .lines()
        .filter_map(|l| l.strip_prefix(";TIME_ELAPSED:"))
        .filter_map(|v| v.parse().ok())
        .collect();
    assert_eq!(elapsed.len(), 300);
    for (layer, t) in tp.layers.iter().zip(&elapsed) {
        assert!(close(layer.time_start + layer.duration, *t, 1e-4));
    }
    // Absolute E, a 5 mm retraction before the end G-code.
    assert!(!tp.stats.uses_relative_e);
    assert!(tp.retractions.iter().any(|r| (r.amount + 5.0).abs() < 1e-4));
    // Fan ramps up over the first layers (Cura style).
    let fans: Vec<u8> = (0..4)
        .map(|i| {
            tp.segments[tp.layers[i].segments.clone()]
                .iter()
                .rev()
                .find(|s| s.is_extrusion())
                .map_or(0, |s| s.fan)
        })
        .collect();
    assert_eq!(fans, vec![0, 85, 170, 255]);
}

#[test]
fn arcs_test_toolpath() {
    let (file, tp) = analyzed("arcs-test.gcode");
    let st = &tp.stats;
    assert!(st.uses_arcs);
    assert_eq!(tp.metadata.slicer, None);
    // Full circle A: every point on radius 15 around (40, 40).
    let line_a = file
        .lines()
        .position(|l| l.starts_with("G2 X55 Y40 I-15 J0"))
        .unwrap_or(usize::MAX) as u32;
    let a: Vec<_> = tp.segments.iter().filter(|s| s.line == line_a).collect();
    assert!(a.len() >= 40, "{}", a.len());
    for s in &a {
        let r = (s.end[0] - 40.0).hypot(s.end[1] - 40.0);
        assert!((r - 15.0).abs() < 1e-3);
        assert!(s.is_extrusion());
    }
    let len: f32 = a.iter().map(|s| s.length()).sum();
    assert!(len > TAU * 15.0 * 0.999 && len <= TAU * 15.0 + 1e-3);
    // Inch-mode circle: radius 0.4 in around (3.6 in, 6 in).
    let line_g = file
        .lines()
        .position(|l| l.starts_with("G2 X4 Y6 I-0.4"))
        .unwrap_or(usize::MAX) as u32;
    let g: Vec<_> = tp.segments.iter().filter(|s| s.line == line_g).collect();
    assert!(!g.is_empty());
    for s in &g {
        let r = (s.end[0] - 91.44).hypot(s.end[1] - 152.4);
        assert!((r - 10.16).abs() < 1e-3, "{r}");
    }
    // Relative-mode bumps stay on their row and extrude.
    let rel: Vec<_> = tp
        .segments
        .iter()
        .filter(|s| s.feature == Feature::SolidInfill && s.is_extrusion())
        .collect();
    assert_eq!(rel.first().map(|s| s.start), Some([20.0, 80.0, 0.2]));
    assert!(rel.iter().all(|s| (s.end[1] - 80.0).abs() <= 4.001));
    let rel_max_x = rel.iter().map(|s| s.end[0]).fold(f32::MIN, f32::max);
    assert!((rel_max_x - 60.0).abs() < 1e-3);
    // The 270 degree arc (negative R) goes the long way round.
    let line_d = file
        .lines()
        .position(|l| l.starts_with("G2 X175 Y45 R-15"))
        .unwrap_or(usize::MAX) as u32;
    let d_len: f32 = tp
        .segments
        .iter()
        .filter(|s| s.line == line_d)
        .map(|s| s.length())
        .sum();
    assert!((d_len - 0.75 * TAU * 15.0).abs() < 0.1, "{d_len}");
    // The arc travel does not extrude.
    let line_t = file
        .lines()
        .position(|l| l.starts_with("G3 X175 Y75 I0 J15"))
        .unwrap_or(usize::MAX) as u32;
    assert!(
        tp.segments
            .iter()
            .filter(|s| s.line == line_t)
            .all(|s| s.kind == MoveKind::Travel)
    );
    // Tube: 10 planar layers, then the 50-turn helix rises 10 mm in ~0.2 mm
    // buckets (no layer markers in this file) instead of one layer per
    // segment.
    assert!(
        tp.layers.len() >= 55 && tp.layers.len() <= 65,
        "{}",
        tp.layers.len()
    );
    assert!((tp.bounds.max[2] - 12.2).abs() < 1e-4);
    for layer in &tp.layers[10..tp.layers.len() - 1] {
        assert!(
            layer.height > 0.15 && layer.height < 0.3,
            "{}",
            layer.height
        );
    }
    // Extrusion follows the arc lengths: E per mm is constant.
    for s in tp
        .segments
        .iter()
        .filter(|s| s.is_extrusion() && s.feature == Feature::ExternalPerimeter)
    {
        let per_mm = s.extruded / s.length();
        assert!(
            (per_mm - 0.4 * 0.2 / (std::f32::consts::PI * 1.75 * 1.75 / 4.0)).abs() < 2e-3,
            "{per_mm}"
        );
    }
}

#[test]
fn coarser_arc_tolerance_means_fewer_segments() {
    let file = load("arcs-test.gcode");
    let fine = analyze_simple(
        &file,
        &AnalyzeOptions {
            arc_tolerance: 0.005,
            ..AnalyzeOptions::default()
        },
    );
    let coarse = analyze_simple(
        &file,
        &AnalyzeOptions {
            arc_tolerance: 0.1,
            ..AnalyzeOptions::default()
        },
    );
    assert!(fine.segments.len() > coarse.segments.len() * 2);
    assert!(close(
        fine.stats.filament_length,
        coarse.stats.filament_length,
        1e-4
    ));
}

#[test]
fn analysis_can_be_cancelled_on_a_real_file() {
    let file = load("twisted-vase.gcode");
    let mut calls = 0;
    let result = analyze(&file, &AnalyzeOptions::default(), &mut |p| {
        calls += 1;
        p < 0.5
    });
    assert!(result.is_none());
    assert!(calls > 40 && calls < 60, "{calls}");
}
