//! Headless GPU rendering. Every test skips (with a message) when no wgpu
//! adapter is available, e.g. on CI machines without a GPU or WARP.

use std::sync::Arc;

use glam::{Vec2, Vec3};
use sprint_gcode::{Feature, Layer, MoveKind, Segment, Toolpath};
use sprint_viewer::{
    ColorMode, GcodeViewer, RenderStyle, ViewMode, ViewPreset, demo, headless_device, wgpu,
};

const W: u32 = 320;
const H: u32 = 240;
const BACKGROUND: [u8; 3] = [24, 26, 31];

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let d = headless_device();
    if d.is_none() {
        eprintln!("skipping GPU test: no wgpu adapter available");
    }
    d
}

/// A 20 x 20 mm block of external-perimeter zig-zags, 10 layers, centred on
/// the default bed.
fn block() -> Toolpath {
    let mut tp = Toolpath::default();
    let mut line = 0;
    for layer in 0..10u32 {
        let z = 0.2 + layer as f32 * 0.2;
        let start = tp.segments.len();
        for i in 0..45 {
            let y = 100.0 + i as f32 * 0.45;
            let (x0, x1) = if i % 2 == 0 {
                (100.0, 120.0)
            } else {
                (120.0, 100.0)
            };
            tp.segments.push(Segment {
                start: [x0, y, z],
                end: [x1, y, z],
                feedrate: 20.0 + i as f32,
                width: 0.45,
                height: 0.2,
                time: line as f32 * 0.1,
                duration: 0.1,
                line,
                layer,
                kind: MoveKind::Extrude,
                feature: Feature::ExternalPerimeter,
                tool: (layer % 2) as u8,
                fan: 255,
                ..Default::default()
            });
            line += 1;
        }
        tp.layers.push(Layer {
            z,
            height: 0.2,
            segments: start..tp.segments.len(),
            lines: start as u32..line,
            ..Default::default()
        });
        tp.bounds.include([100.0, 100.0, z]);
        tp.bounds.include([120.0, 120.0, z]);
    }
    tp.stats.estimated_time = line as f32 * 0.1;
    tp.line_count = line;
    tp
}

fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 3], tol: i32) -> bool {
    (0..3).all(|k| (a[k] as i32 - b[k] as i32).abs() <= tol)
}

/// Screen position (pixels) of the block's top centre for the viewer camera.
fn block_centre(viewer: &GcodeViewer) -> (u32, u32) {
    let p = viewer
        .camera()
        .project_to_screen(Vec3::new(110.0, 110.0, 2.0), Vec2::new(W as f32, H as f32))
        .expect("visible");
    (p.x as u32, p.y as u32)
}

fn loaded_viewer(device: &wgpu::Device, queue: &wgpu::Queue, tp: Toolpath) -> GcodeViewer {
    let mut viewer = GcodeViewer::new_headless(device, queue);
    viewer.set_toolpath(Arc::new(tp));
    viewer.wait_until_loaded();
    viewer
}

fn render(viewer: &mut GcodeViewer) -> Vec<u8> {
    let (w, h, img) = viewer.render_to_image(W, H).expect("rendered");
    assert_eq!((w, h), (W, H));
    assert_eq!(img.len(), (W * H * 4) as usize);
    img
}

#[test]
fn renders_beads_over_the_background() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, block());
    viewer.options_mut().show_bed = false;
    let img = render(&mut viewer);
    // Away from the block: plain background.
    assert!(near(px(&img, 2, 2), BACKGROUND, 3), "{:?}", px(&img, 2, 2));
    // The block is orange (external perimeter) and lit.
    let (cx, cy) = block_centre(&viewer);
    let c = px(&img, cx, cy);
    assert!(
        c[0] > 150 && c[0] > c[1] && c[1] > c[2],
        "centre pixel {c:?}"
    );
    assert_eq!(c[3], 255);
    let differing = img
        .chunks_exact(4)
        .filter(|p| !near([p[0], p[1], p[2], p[3]], BACKGROUND, 6))
        .count();
    assert!(
        differing > (W * H / 20) as usize,
        "{differing} non-background pixels"
    );
}

#[test]
fn colour_modes_and_options_change_the_image_without_reloading() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, block());
    let (cx, cy) = block_centre(&viewer);
    let feature = px(&render(&mut viewer), cx, cy);
    let mut seen = vec![feature];
    for mode in [
        ColorMode::Speed,
        ColorMode::Tool,
        ColorMode::Layer,
        ColorMode::Time,
    ] {
        viewer.options_mut().color_mode = mode;
        let c = px(&render(&mut viewer), cx, cy);
        assert!(
            seen.iter().all(|s| s != &c) || mode == ColorMode::Time,
            "{mode:?} gave {c:?}, already seen {seen:?}"
        );
        seen.push(c);
    }
    viewer.options_mut().color_mode = ColorMode::Feature;
    viewer.options_mut().highlight_line = Some(9 * 45 + 22);
    let highlighted = render(&mut viewer);
    viewer.options_mut().highlight_line = None;
    let plain = render(&mut viewer);
    assert_ne!(highlighted, plain);
    viewer.options_mut().progress_line = Some(0);
    let ghosted = px(&render(&mut viewer), cx, cy);
    assert!(
        (ghosted[0] as i32) < feature[0] as i32 - 60,
        "progress ghosting {ghosted:?} vs {feature:?}"
    );
}

#[test]
fn hiding_every_feature_leaves_only_the_bed() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, block());
    let (cx, cy) = block_centre(&viewer);
    viewer.options_mut().hidden_features = [true; Feature::COUNT];
    let hidden = px(&render(&mut viewer), cx, cy);
    // Same camera, empty layer range on an empty viewer would show the bed:
    // a dark grey, not orange.
    assert!(
        hidden[0] < 90 && (hidden[0] as i32 - hidden[2] as i32).abs() < 30,
        "{hidden:?}"
    );
}

#[test]
fn layer_range_limits_what_is_drawn() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, block());
    viewer.set_view(ViewPreset::Front);
    let full = render(&mut viewer);
    viewer.options_mut().layer_range = (0, 1);
    let low = render(&mut viewer);
    let count = |img: &[u8]| {
        img.chunks_exact(4)
            .filter(|p| p[0] > 150 && p[0] > p[2] + 60)
            .count()
    };
    assert!(
        count(&low) * 3 < count(&full),
        "2 of 10 layers should cover far fewer pixels ({} vs {})",
        count(&low),
        count(&full)
    );
}

#[test]
fn styles_views_and_overlays_render() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, demo::demo_toolpath());
    let o = viewer.options_mut();
    o.show_travel = true;
    o.show_retractions = true;
    o.show_build_volume = true;
    o.line_limit = Some(5000);
    o.progress_line = Some(9000);
    let base = render(&mut viewer);
    viewer.options_mut().style = RenderStyle::Lines;
    let lines = render(&mut viewer);
    assert_ne!(base, lines);
    viewer.options_mut().view_mode = ViewMode::LayerTop;
    viewer.options_mut().layer_range = (0, 20);
    let top = render(&mut viewer);
    assert_ne!(top, lines);
    for preset in ViewPreset::ALL {
        viewer.set_view(preset);
        let img = render(&mut viewer);
        assert!(
            img.chunks_exact(4)
                .any(|p| !near([p[0], p[1], p[2], 255], BACKGROUND, 6))
        );
    }
}

#[test]
fn ribbon_lod_matches_prisms_closely() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, block());
    viewer.set_lod_pixels(0.0);
    let prisms = render(&mut viewer);
    viewer.set_lod_pixels(f32::MAX);
    let ribbons = render(&mut viewer);
    let diff: u64 = prisms
        .iter()
        .zip(&ribbons)
        .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
        .sum();
    let mean = diff as f64 / prisms.len() as f64;
    assert!(mean < 6.0, "mean channel difference {mean:.2}");
    assert_ne!(prisms, ribbons, "LOD setting has an effect");
}

#[test]
fn odd_sizes_and_zero_sizes() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, block());
    assert!(viewer.render_to_image(0, 10).is_none());
    assert!(viewer.render_to_image(10, 0).is_none());
    let (w, h, img) = viewer.render_to_image(333, 77).expect("rendered");
    assert_eq!((w, h), (333, 77));
    assert_eq!(img.len(), 333 * 77 * 4);
    // Rows are un-padded correctly: the top row is sky on both ends.
    let first = &img[..4];
    let last = &img[(333 - 1) * 4..333 * 4];
    assert!(near([first[0], first[1], first[2], 255], BACKGROUND, 3));
    assert!(near([last[0], last[1], last[2], 255], BACKGROUND, 3));
    let (w, h, _) = viewer.render_to_image(1, 1).expect("1x1");
    assert_eq!((w, h), (1, 1));
    // Requests beyond the texture limit are clamped, not a crash.
    let (w, _, _) = viewer.render_to_image(100_000, 4).expect("clamped");
    assert!(w <= device.limits().max_texture_dimension_2d);
}

#[test]
fn empty_viewer_shows_the_bed() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    let img = render(&mut viewer);
    let centre = px(&img, W / 2, H / 2);
    assert!(
        !near(centre, BACKGROUND, 2),
        "bed plate expected, got {centre:?}"
    );
    viewer.options_mut().show_bed = false;
    viewer.options_mut().show_axes = false;
    let img = render(&mut viewer);
    assert!(
        img.chunks_exact(4)
            .all(|p| near([p[0], p[1], p[2], 255], BACKGROUND, 2))
    );
}

#[test]
fn picking_through_the_screen_finds_the_projected_bead() {
    let Some((device, queue)) = device() else {
        return;
    };
    let tp = block();
    let mut viewer = loaded_viewer(&device, &queue, tp.clone());
    viewer.set_view(ViewPreset::Top);
    viewer.render_to_image(W, H).expect("rendered");
    let size = Vec2::new(W as f32, H as f32);
    let top = &tp.segments[tp.layers[9].segments.clone()];
    for seg in top.iter().step_by(7) {
        let mid = (Vec3::from(seg.start) + Vec3::from(seg.end)) * 0.5;
        let s = viewer
            .camera()
            .project_to_screen(mid, size)
            .expect("visible");
        let hit = viewer
            .pick_screen([s.x, s.y], [W as f32, H as f32])
            .expect("hit");
        let picked = &tp.segments[hit.segment];
        assert_eq!(picked.layer, 9);
        assert!(
            (hit.point - mid).truncate().length() < 1.0,
            "{:?} vs {mid:?}",
            hit.point
        );
    }
    // Hidden layers are not pickable.
    viewer.options_mut().layer_range = (0, 3);
    let mid = Vec3::new(110.0, 110.0, 2.0);
    let s = viewer
        .camera()
        .project_to_screen(mid, size)
        .expect("visible");
    let hit = viewer
        .pick_screen([s.x, s.y], [W as f32, H as f32])
        .expect("hit");
    assert!(tp.segments[hit.segment].layer <= 3);
}

#[test]
fn replacing_the_toolpath_keeps_the_latest() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    viewer.set_toolpath(Arc::new(demo::stress_toolpath(300_000)));
    let mut b = block();
    for s in &mut b.segments {
        s.tool = 5;
    }
    viewer.set_toolpath(Arc::new(b));
    viewer.wait_until_loaded();
    assert!(!viewer.is_loading());
    assert_eq!(viewer.layer_count(), 10);
    viewer.options_mut().color_mode = ColorMode::Tool;
    match viewer.legend() {
        sprint_viewer::color::Legend::Tools(rows) => {
            assert_eq!(rows.iter().map(|r| r.tool).collect::<Vec<_>>(), vec![5]);
        }
        other => panic!("unexpected legend {other:?}"),
    }
    viewer.clear();
    assert!(viewer.toolpath().is_none());
    assert_eq!(viewer.layer_count(), 0);
    render(&mut viewer);
}

#[test]
fn large_toolpaths_stream_in_over_several_polls() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    let tp = Arc::new(demo::stress_toolpath(800_000));
    viewer.set_toolpath(Arc::clone(&tp));
    let mut polls = 0;
    while viewer.poll() {
        polls += 1;
        viewer.render_offscreen(W, H);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(polls < 100_000, "loading never finished");
    }
    assert!(polls > 1, "loading should be spread over frames");
    let img = render(&mut viewer);
    assert!(img.chunks_exact(4).any(|p| p[0] > 120 && p[0] > p[2] + 40));
}
