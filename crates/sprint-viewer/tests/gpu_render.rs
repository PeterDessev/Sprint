//! Headless GPU rendering. Every test skips (with a message) when no wgpu
//! adapter is available, e.g. on CI machines without a GPU or WARP.

use std::sync::Arc;

use glam::{Vec2, Vec3};
use sprint_gcode::{Feature, Layer, MoveKind, Segment, Toolpath};
use sprint_viewer::{
    BedSpec, ColorMode, GcodeViewer, GhostStyle, RenderStyle, ViewMode, ViewPreset, demo,
    headless_device, wgpu,
};

const W: u32 = 320;
const H: u32 = 240;
const BACKGROUND: [u8; 3] = [24, 26, 31];

/// `SPRINT_TEST_NO_GPU` (set, any value) makes the GPU tests run as on a
/// machine without an adapter: CI sets it, as software rasterizers on
/// runners vary and some crash.
fn gpu_disabled() -> bool {
    std::env::var_os("SPRINT_TEST_NO_GPU").is_some()
}

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let d = headless_device().filter(|_| !gpu_disabled());
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

/// A device whose `max_buffer_size` is only `max_buffer` bytes.
fn small_buffer_device(max_buffer: u64) -> Option<(wgpu::Device, wgpu::Queue)> {
    if gpu_disabled() {
        return None;
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = [false, true].into_iter().find_map(|fallback| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: fallback,
            ..Default::default()
        }))
        .ok()
    })?;
    let base = if adapter.get_info().backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    let limits = wgpu::Limits {
        max_buffer_size: max_buffer,
        ..base.using_resolution(adapter.limits())
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits,
        ..Default::default()
    }))
    .ok()
}

#[test]
fn images_beyond_the_read_back_limit_are_scaled_down() {
    // Regression: the read-back buffer exceeded `max_buffer_size` and wgpu
    // panicked (e.g. 9000 x 9000 with the default 256 MiB limit).
    const MAX: u64 = 4 << 20;
    let Some((device, queue)) = small_buffer_device(MAX) else {
        eprintln!("skipping GPU test: no wgpu adapter available");
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, block());
    for (w, h) in [(2000, 1500), (4000, 300), (1, 3000)] {
        let (rw, rh, img) = viewer.render_to_image(w, h).expect("scaled down");
        assert_eq!(img.len(), (rw * rh * 4) as usize);
        assert!(rw <= w && rh <= h && rw >= 1 && rh >= 1, "{rw}x{rh}");
        let padded = (rw as u64 * 4).div_ceil(256) * 256;
        assert!(padded * rh as u64 <= MAX, "{rw}x{rh} does not fit");
        // The aspect ratio is kept (within rounding).
        let want = w as f64 / h as f64;
        let got = rw as f64 / rh as f64;
        assert!(
            (got / want - 1.0).abs() < 0.02 || rw == 1,
            "{rw}x{rh} for {w}x{h}"
        );
        assert!(
            img.chunks_exact(4)
                .any(|p| !near([p[0], p[1], p[2], 255], BACKGROUND, 6))
        );
    }
    // Small requests are untouched.
    let (w, h, _) = viewer.render_to_image(320, 240).expect("rendered");
    assert_eq!((w, h), (320, 240));
}

#[test]
fn building_and_drawing_raise_no_gpu_errors() {
    let Some((device, queue)) = device() else {
        return;
    };
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut viewer = loaded_viewer(&device, &queue, demo::demo_toolpath());
    let o = viewer.options_mut();
    o.show_travel = true;
    o.show_retractions = true;
    o.show_build_volume = true;
    o.progress_line = Some(5000);
    render(&mut viewer);
    viewer.options_mut().style = RenderStyle::Lines;
    viewer.options_mut().view_mode = ViewMode::LayerTop;
    render(&mut viewer);
    drop(viewer);
    let error = pollster::block_on(validation.pop());
    assert!(error.is_none(), "{error:?}");
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
fn fitting_while_the_scene_builds_uses_the_scene_bounds() {
    // Regression: fit_to_view / set_view right after set_toolpath animate
    // towards the toolpath's raw bounds, which include start G-code such as
    // a purge line. The scene's own bounds (which ignore it) arrived during
    // the animation and were dropped, so the camera ended up zoomed out.
    let Some((device, queue)) = device() else {
        return;
    };
    let mut tp = block();
    let purge = Segment {
        start: [0.0, 5.0, 0.2],
        end: [210.0, 5.0, 0.2],
        width: 0.6,
        height: 0.2,
        line: tp.line_count,
        layer: 9,
        kind: MoveKind::Extrude,
        feature: Feature::Custom,
        ..Default::default()
    };
    tp.segments.push(purge);
    if let Some(last) = tp.layers.last_mut() {
        last.segments.end = tp.segments.len();
    }
    tp.bounds.include(purge.start);
    tp.bounds.include(purge.end);
    tp.line_count += 1;
    let tp = Arc::new(tp);

    let fresh = |tp: &Arc<Toolpath>| {
        let mut viewer = GcodeViewer::new_headless(&device, &queue);
        // A running UI has drawn frames of this size before the file arrives.
        viewer.render_offscreen(W, H);
        viewer.set_toolpath(Arc::clone(tp));
        viewer
    };
    let mut reference = fresh(&tp);
    reference.wait_until_loaded();
    reference.render_offscreen(W, H);
    let want = *reference.camera();
    assert!(want.target.x > 105.0, "the purge line is ignored: {want:?}");

    for preset in [None, Some(ViewPreset::Iso)] {
        let mut viewer = fresh(&tp);
        match preset {
            None => viewer.fit_to_view(),
            Some(p) => viewer.set_view(p),
        }
        viewer.wait_until_loaded();
        viewer.render_offscreen(W, H);
        let got = *viewer.camera();
        assert!(
            (got.target - want.target).length() < 0.5
                && (got.distance / want.distance - 1.0).abs() < 0.02,
            "{preset:?}: camera {got:?}, expected {want:?}"
        );
    }
}

#[test]
fn clearing_during_a_camera_animation_frames_the_bed() {
    let Some((device, queue)) = device() else {
        return;
    };
    // The animation's orientation is kept, framing the bed.
    let mut empty = GcodeViewer::new_headless(&device, &queue);
    empty.render_offscreen(W, H);
    empty.set_view(ViewPreset::Front);
    empty.render_offscreen(W, H);
    let want = *empty.camera();

    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    viewer.render_offscreen(W, H);
    viewer.set_toolpath(Arc::new(block()));
    viewer.wait_until_loaded();
    viewer.set_view(ViewPreset::Front);
    viewer.clear();
    viewer.render_offscreen(W, H);
    let got = *viewer.camera();
    assert!(
        (got.target - want.target).length() < 0.5
            && (got.distance / want.distance - 1.0).abs() < 0.02
            && (got.yaw - want.yaw).abs() < 1e-4
            && (got.pitch - want.pitch).abs() < 1e-4,
        "camera {got:?}, expected the empty bed's {want:?}"
    );

    // Same for a bed change while a preset animation runs.
    let bed = BedSpec {
        size: [350.0, 350.0],
        max_z: 300.0,
        origin_center: true,
        round: false,
    };
    let mut reference = GcodeViewer::new_headless(&device, &queue);
    reference.render_offscreen(W, H);
    reference.set_bed(bed);
    reference.set_view(ViewPreset::Right);
    reference.render_offscreen(W, H);
    let want = *reference.camera();
    viewer.set_view(ViewPreset::Right);
    viewer.set_bed(bed);
    viewer.render_offscreen(W, H);
    let got = *viewer.camera();
    assert!(
        (got.target - want.target).length() < 0.5
            && (got.distance / want.distance - 1.0).abs() < 0.02,
        "camera {got:?}, expected the new bed's {want:?}"
    );
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
fn a_new_analysis_keeps_the_layers_and_both_cameras() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    viewer.render_offscreen(W, H);
    viewer.set_toolpath(Arc::new(block()));
    viewer.wait_until_loaded();
    // The user looks at layers 3..=6 in the layer view, framed off a card
    // on the left, then orbits the 3D view (fits are no longer automatic)
    // and the card goes away.
    viewer.options_mut().layer_range = (2, 5);
    viewer.options_mut().view_mode = ViewMode::LayerTop;
    viewer.fit_to_view_within(sprint_viewer::ViewInsets {
        left: 100.0,
        ..Default::default()
    });
    viewer.render_offscreen(W, H);
    viewer.camera_mut().orbit(0.4, -0.2);
    viewer.set_fit_insets(sprint_viewer::ViewInsets::NONE);
    let camera = *viewer.camera();
    let top_before = render(&mut viewer);
    // The same file analyzed again (other times, same geometry).
    let mut again = block();
    for s in &mut again.segments {
        s.time *= 1.5;
    }
    viewer.replace_toolpath(Arc::new(again));
    viewer.wait_until_loaded();
    assert_eq!(viewer.options().layer_range, (2, 5));
    assert_eq!(*viewer.camera(), camera, "the 3D camera moved");
    assert_eq!(render(&mut viewer), top_before, "the layer view moved");
    // Fewer layers: the range is clamped.
    let mut short = block();
    short.layers.truncate(4);
    let end = short.layers.last().map_or(0, |l| l.segments.end);
    short.segments.truncate(end);
    viewer.replace_toolpath(Arc::new(short));
    viewer.wait_until_loaded();
    assert_eq!(viewer.options().layer_range, (2, 3));
    // A new file (set_toolpath) shows every layer and fits again.
    viewer.set_toolpath(Arc::new(block()));
    assert_eq!(viewer.options().layer_range, (0, 9));
}

#[test]
fn fit_insets_keep_the_part_clear_of_the_cards() {
    let Some((device, queue)) = device() else {
        return;
    };
    let size = Vec2::new(W as f32, H as f32);
    let centre_x = |viewer: &mut GcodeViewer| {
        viewer.render_offscreen(W, H);
        viewer
            .camera()
            .project_to_screen(Vec3::new(110.0, 110.0, 1.0), size)
            .expect("visible")
            .x
    };
    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    viewer.render_offscreen(W, H);
    viewer.set_toolpath(Arc::new(block()));
    viewer.wait_until_loaded();
    let plain = centre_x(&mut viewer);
    assert!((plain - W as f32 / 2.0).abs() < 4.0, "{plain}");
    // A card over the left 120 points: the part moves right, into the
    // middle of what is left, and is drawn there.
    let insets = sprint_viewer::ViewInsets {
        left: 120.0,
        ..Default::default()
    };
    viewer.set_fit_insets(insets);
    assert_eq!(viewer.fit_insets(), insets);
    let shifted = centre_x(&mut viewer);
    let want = 120.0 + (W as f32 - 120.0) / 2.0;
    assert!((shifted - want).abs() < 6.0, "{shifted}, want {want}");
    viewer.options_mut().show_bed = false;
    viewer.options_mut().show_axes = false;
    let img = render(&mut viewer);
    assert!(
        (0..110).all(|x| near(px(&img, x, H / 2), BACKGROUND, 12)),
        "the card's area stays clear"
    );
    assert!(
        !near(px(&img, want as u32, H / 2), BACKGROUND, 12),
        "the part is drawn"
    );
    // The layer view frames it the same way.
    viewer.options_mut().view_mode = ViewMode::LayerTop;
    viewer.fit_to_view();
    assert!((centre_x(&mut viewer) - want).abs() < 6.0);
    // Back to no insets with fit_to_view_within.
    viewer.options_mut().view_mode = ViewMode::Perspective;
    viewer.fit_to_view_within(sprint_viewer::ViewInsets::NONE);
    assert!((centre_x(&mut viewer) - W as f32 / 2.0).abs() < 4.0);
    // A camera the user moved stays put when the insets change.
    viewer.camera_mut().orbit(0.3, 0.0);
    let moved = *viewer.camera();
    viewer.set_fit_insets(insets);
    viewer.render_offscreen(W, H);
    assert_eq!(*viewer.camera(), moved);
}

#[test]
fn points_project_where_the_active_camera_draws_them() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = loaded_viewer(&device, &queue, block());
    viewer.render_offscreen(W, H);
    let size = [W as f32, H as f32];
    let target = viewer.camera().target;
    let at = viewer.project(target.into(), size).expect("in front");
    assert!((at[0] - W as f32 / 2.0).abs() < 0.5 && (at[1] - H as f32 / 2.0).abs() < 0.5);
    // The layer view's camera, once active.
    viewer.options_mut().view_mode = ViewMode::LayerTop;
    viewer.render_offscreen(W, H);
    let above = viewer.project([110.0, 110.0, 2.0], size).expect("in front");
    let screen = viewer
        .camera()
        .project_to_screen(Vec3::new(110.0, 110.0, 2.0), Vec2::from(size));
    assert!(above[0].is_finite() && above[1].is_finite());
    // The 3D camera looks from elsewhere: another position.
    assert!(screen.is_some_and(|s| (s.x - above[0]).abs() + (s.y - above[1]).abs() > 1.0));
    // Behind the camera: nothing.
    viewer.options_mut().view_mode = ViewMode::Perspective;
    let cam = *viewer.camera();
    let behind = cam.eye() - cam.forward() * 50.0;
    assert_eq!(viewer.project(behind.into(), size), None);
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

/// A hollow 20 x 20 mm square tower of external-perimeter loops, `layers`
/// layers of 0.2 mm, wound counter-clockwise like a slicer's outer walls.
fn tower(layers: u32) -> Toolpath {
    let mut tp = Toolpath::default();
    let mut line = 0;
    let corners = [
        [100.0f32, 100.0],
        [120.0, 100.0],
        [120.0, 120.0],
        [100.0, 120.0],
        [100.0, 100.0],
    ];
    for layer in 0..layers {
        let z = 0.2 + layer as f32 * 0.2;
        let start = tp.segments.len();
        for side in corners.windows(2) {
            for k in 0..10 {
                let lerp = |t: f32| {
                    [
                        side[0][0] + (side[1][0] - side[0][0]) * t,
                        side[0][1] + (side[1][1] - side[0][1]) * t,
                        z,
                    ]
                };
                tp.segments.push(Segment {
                    start: lerp(k as f32 / 10.0),
                    end: lerp((k + 1) as f32 / 10.0),
                    feedrate: 30.0,
                    width: 0.45,
                    height: 0.2,
                    line,
                    layer,
                    kind: MoveKind::Extrude,
                    feature: Feature::ExternalPerimeter,
                    ..Default::default()
                });
                line += 1;
            }
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
    tp.line_count = line;
    tp
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// Mean luminance of a small square of pixels around a projected point.
fn patch_luma(viewer: &GcodeViewer, img: &[u8], p: Vec3) -> f32 {
    let s = viewer
        .camera()
        .project_to_screen(p, Vec2::new(W as f32, H as f32))
        .expect("visible");
    let (cx, cy) = (s.x as i32, s.y as i32);
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in cy - 1..=cy + 1 {
        for x in cx - 1..=cx + 1 {
            sum += luma(px(img, x as u32, y as u32));
            n += 1.0;
        }
    }
    sum / n
}

/// Iso view of the tower from `distance` mm.
fn tower_view(device: &wgpu::Device, queue: &wgpu::Queue, distance: f32) -> GcodeViewer {
    let mut viewer = loaded_viewer(device, queue, tower(100));
    viewer.options_mut().show_bed = false;
    viewer.options_mut().show_axes = false;
    let cam = viewer.camera_mut();
    cam.target = Vec3::new(110.0, 110.0, 10.0);
    cam.distance = distance;
    viewer
}

#[test]
fn distant_walls_shade_by_the_way_they_face() {
    // Far away every layer is thinner than a pixel. The wall facing the
    // key light (upper left) must still read clearly brighter than the one
    // facing right, as on a solid part.
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = tower_view(&device, &queue, 160.0);
    let img = render(&mut viewer);
    let left = patch_luma(&viewer, &img, Vec3::new(100.0, 110.0, 10.0));
    let front = patch_luma(&viewer, &img, Vec3::new(110.0, 100.0, 10.0));
    assert!(
        left > front * 1.2 && front > 25.0,
        "left wall {left:.1} vs front wall {front:.1}"
    );
}

#[test]
fn distant_layers_do_not_shimmer() {
    // Layers a fraction of a pixel thick: neighbouring pixels down a wall
    // must not alternate between bright and dark (moire).
    let Some((device, queue)) = device() else {
        return;
    };
    for distance in [110.0, 160.0, 260.0] {
        let mut viewer = tower_view(&device, &queue, distance);
        let img = render(&mut viewer);
        let size = Vec2::new(W as f32, H as f32);
        for face in [
            [Vec3::new(100.0, 104.0, 3.0), Vec3::new(100.0, 104.0, 17.0)],
            [Vec3::new(116.0, 100.0, 3.0), Vec3::new(116.0, 100.0, 17.0)],
        ] {
            let a = viewer.camera().project_to_screen(face[0], size).expect("a");
            let b = viewer.camera().project_to_screen(face[1], size).expect("b");
            let x = ((a.x + b.x) * 0.5) as u32;
            let (y0, y1) = (b.y.min(a.y) as u32, b.y.max(a.y) as u32);
            let column: Vec<f32> = (y0..=y1).map(|y| luma(px(&img, x, y))).collect();
            assert!(column.len() > 8, "face too small at {distance} mm");
            let steps: f32 = column.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
            let mean = steps / (column.len() - 1) as f32;
            assert!(
                mean < 2.5,
                "{distance} mm: mean step {mean:.2} down the wall at x {x}: {column:?}"
            );
        }
    }
}

#[test]
fn prisms_and_ribbons_agree_at_middle_distances() {
    // Where beads are a few pixels thick (the LOD switch), both paths use
    // the same shading model, so switching must not visibly pop.
    let Some((device, queue)) = device() else {
        return;
    };
    for distance in [45.0, 70.0, 110.0] {
        let mut viewer = tower_view(&device, &queue, distance);
        viewer.set_lod_pixels(0.0);
        let prisms = render(&mut viewer);
        viewer.set_lod_pixels(f32::MAX);
        let ribbons = render(&mut viewer);
        let lit =
            |img: &[u8], i: usize| !near([img[i], img[i + 1], img[i + 2], 255], BACKGROUND, 6);
        let (mut diff, mut n) = (0.0f64, 0usize);
        for i in (0..prisms.len()).step_by(4) {
            if lit(&prisms, i) && lit(&ribbons, i) {
                diff += (luma([prisms[i], prisms[i + 1], prisms[i + 2], 255])
                    - luma([ribbons[i], ribbons[i + 1], ribbons[i + 2], 255]))
                .abs() as f64;
                n += 1;
            }
        }
        assert!(n > 1000, "{distance} mm: only {n} pixels covered");
        let mean = diff / n as f64;
        assert!(
            mean < 9.0,
            "{distance} mm: mean luminance difference {mean:.2}"
        );
    }
}

#[test]
fn translucent_ghosts_let_the_printed_part_show_through() {
    let Some((device, queue)) = device() else {
        return;
    };
    let tp = block();
    // Layers 0..=4 printed, 5..=9 still to come.
    let progress = tp.layers[5].lines.start - 1;
    let mut viewer = loaded_viewer(&device, &queue, tp);
    viewer.set_view(ViewPreset::Top);
    viewer.options_mut().show_bed = false;
    let (cx, cy) = block_centre(&viewer);
    let printed = px(&render(&mut viewer), cx, cy);

    viewer.options_mut().progress_line = Some(progress);
    let solid = px(&render(&mut viewer), cx, cy);
    assert_eq!(viewer.options().ghost_style, GhostStyle::Opaque);

    viewer.options_mut().ghost_style = GhostStyle::Translucent;
    let glass = render(&mut viewer);
    let through = px(&glass, cx, cy);
    let warmth = |p: [u8; 4]| p[0] as i32 - p[2] as i32;
    // Solid ghosts hide the orange part; translucent ones show it, muted.
    assert!(warmth(solid) < 30, "solid ghost {solid:?}");
    assert!(
        warmth(through) > warmth(solid) + 25 && warmth(through) < warmth(printed),
        "printed {printed:?}, solid ghost {solid:?}, translucent ghost {through:?}"
    );
    // Without any printed beads behind, the ghost is a light veil over the
    // background, not the background itself.
    viewer.options_mut().progress_line = Some(0);
    let veil = px(&render(&mut viewer), cx, cy);
    assert!(
        !near(veil, BACKGROUND, 6) && warmth(veil) < warmth(through),
        "{veil:?}"
    );
    // Lines style has a ghost pass too.
    viewer.options_mut().style = RenderStyle::Lines;
    viewer.options_mut().progress_line = Some(progress);
    let lines = render(&mut viewer);
    assert!(
        lines
            .chunks_exact(4)
            .any(|p| p[0] > 120 && p[0] as i32 > p[2] as i32 + 60)
    );
}

#[test]
fn translucent_ghosts_in_the_layer_view_raise_no_gpu_errors() {
    let Some((device, queue)) = device() else {
        return;
    };
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut viewer = loaded_viewer(&device, &queue, demo::demo_toolpath());
    let o = viewer.options_mut();
    o.ghost_style = GhostStyle::Translucent;
    o.view_mode = ViewMode::LayerTop;
    o.layer_range = (0, 30);
    o.progress_line = Some(9000);
    let img = render(&mut viewer);
    assert!(
        img.chunks_exact(4)
            .any(|p| !near([p[0], p[1], p[2], 255], BACKGROUND, 6))
    );
    for style in RenderStyle::ALL {
        viewer.options_mut().style = style;
        viewer.options_mut().view_mode = ViewMode::Perspective;
        render(&mut viewer);
    }
    drop(viewer);
    let error = pollster::block_on(validation.pop());
    assert!(error.is_none(), "{error:?}");
}

#[test]
fn a_highlighted_object_stands_out() {
    let Some((device, queue)) = device() else {
        return;
    };
    // The block's front half is one object, its back half another.
    let mut tp = block();
    for s in &mut tp.segments {
        s.object = if s.start[1] < 110.0 { 0 } else { 1 };
    }
    for name in ["front", "back"] {
        tp.objects.push(sprint_gcode::ObjectInfo {
            name: name.into(),
            ..Default::default()
        });
    }
    let mut viewer = loaded_viewer(&device, &queue, tp);
    viewer.options_mut().show_bed = false;
    viewer.set_view(ViewPreset::Top);
    let plain = render(&mut viewer);
    let front = screen_of(&viewer, Vec3::new(110.0, 104.0, 2.0));
    let back = screen_of(&viewer, Vec3::new(110.0, 116.0, 2.0));
    let lum = |img: &[u8], (x, y): (u32, u32)| luma(px(img, x, y));
    assert!(viewer.can_highlight_object(1));
    assert!(!viewer.can_highlight_object(2), "no such object");
    viewer.options_mut().highlight_object = Some(1);
    let lit = render(&mut viewer);
    assert!(
        lum(&lit, back) > lum(&plain, back) * 1.05,
        "the back object is brighter: {} vs {}",
        lum(&lit, back),
        lum(&plain, back)
    );
    assert!(
        lum(&lit, front) < lum(&plain, front) * 0.85,
        "the rest steps back: {} vs {}",
        lum(&lit, front),
        lum(&plain, front)
    );
    // An object the toolpath does not have highlights nothing.
    viewer.options_mut().highlight_object = Some(7);
    assert_eq!(render(&mut viewer), plain);
    viewer.options_mut().highlight_object = None;
    assert_eq!(render(&mut viewer), plain);
}

/// Screen position of a world point in the viewer's 3D camera.
fn screen_of(viewer: &GcodeViewer, p: Vec3) -> (u32, u32) {
    let s = viewer
        .camera()
        .project_to_screen(p, Vec2::new(W as f32, H as f32))
        .expect("visible");
    (s.x as u32, s.y as u32)
}

#[test]
fn the_nozzle_casts_a_soft_shadow_on_the_part() {
    let Some((device, queue)) = device() else {
        return;
    };
    let tp = block();
    // Scrub to the middle of the top layer: the nozzle sits over its
    // middle bead (layer 9, line 9 * 45 + 22 runs along y = 109.9).
    let line = 9 * 45 + 22;
    let tip = Vec3::from(tp.segments[line as usize].end);
    let mut viewer = loaded_viewer(&device, &queue, tp);
    viewer.options_mut().show_bed = false;
    viewer.options_mut().line_limit = Some(line);
    let cam = viewer.camera_mut();
    cam.target = tip;
    cam.distance = 40.0;
    cam.yaw = 0.3;
    cam.pitch = 0.9;
    viewer.options_mut().show_tool_head = false;
    let bare = render(&mut viewer);
    viewer.options_mut().show_tool_head = true;
    let with_head = render(&mut viewer);
    // Just in front of the tip (towards the camera, on the layer below's
    // beads) and well away from it.
    let near_tip = screen_of(&viewer, tip + Vec3::new(-0.6, -0.9, -0.2));
    let far = screen_of(&viewer, tip + Vec3::new(-6.0, -4.0, -0.2));
    let lum = |img: &[u8], (x, y): (u32, u32)| luma(px(img, x, y));
    let shade = lum(&with_head, near_tip) / lum(&bare, near_tip).max(1.0);
    let away = lum(&with_head, far) / lum(&bare, far).max(1.0);
    assert!(
        shade < 0.85 && (away - 1.0).abs() < 0.02,
        "near the tip {shade:.3}, away {away:.3}"
    );
    // The marker itself is drawn above the tip.
    let above = screen_of(&viewer, tip + Vec3::Z * 3.0);
    assert_ne!(
        px(&bare, above.0, above.1),
        px(&with_head, above.0, above.1)
    );
}

#[test]
fn the_nozzle_keeps_its_size_in_logical_points() {
    // Known issue K18: the head was sized in physical pixels, so on a 2x
    // display it looked half as big as on a 1x one (while the pick
    // tolerance is in points).
    let Some((device, queue)) = device() else {
        return;
    };
    let tp = block();
    let line = 9 * 45 + 22;
    let tip = Vec3::from(tp.segments[line as usize].end);
    let mut viewer = loaded_viewer(&device, &queue, tp);
    viewer.options_mut().show_bed = false;
    viewer.options_mut().line_limit = Some(line);
    let cam = viewer.camera_mut();
    cam.target = tip;
    cam.distance = 120.0;
    cam.yaw = 0.3;
    cam.pitch = 0.9;
    // Pixels the head (and its shadow) changes at a scale and image size.
    let mut footprint = |ppp: f32, scale: u32| {
        viewer.set_pixels_per_point(ppp);
        let (w, h) = (W * scale, H * scale);
        viewer.options_mut().show_tool_head = false;
        let (_, _, bare) = viewer.render_to_image(w, h).expect("rendered");
        viewer.options_mut().show_tool_head = true;
        let (_, _, head) = viewer.render_to_image(w, h).expect("rendered");
        bare.chunks_exact(4)
            .zip(head.chunks_exact(4))
            .filter(|(a, b)| (0..3).any(|k| (a[k] as i32 - b[k] as i32).abs() > 24))
            .count() as f32
    };
    let one = footprint(1.0, 1);
    let two = footprint(2.0, 2);
    assert!(one > 100.0, "the head is drawn: {one} pixels");
    // Twice the pixels each way at 2x: the same size in points.
    let ratio = two / one;
    assert!(
        (3.2..4.8).contains(&ratio),
        "{one} -> {two} pixels ({ratio:.2}x)"
    );
    assert_eq!(viewer.pixels_per_point(), 2.0);
    viewer.set_pixels_per_point(f32::NAN);
    assert_eq!(viewer.pixels_per_point(), 1.0);
    viewer.set_pixels_per_point(100.0);
    assert_eq!(viewer.pixels_per_point(), 8.0);
}

#[test]
fn the_layer_being_printed_is_highlighted() {
    let Some((device, queue)) = device() else {
        return;
    };
    let tp = block();
    let mut viewer = loaded_viewer(&device, &queue, tp);
    viewer.set_view(ViewPreset::Top);
    viewer.options_mut().show_tool_head = false;
    let (cx, cy) = block_centre(&viewer);
    // Everything printed: no current layer.
    let plain = px(&render(&mut viewer), cx, cy);
    assert_eq!(viewer.current_layer(), None);
    // Layer 9 being printed, most of it done.
    viewer.options_mut().progress_line = Some(9 * 45 + 40);
    assert_eq!(viewer.current_layer(), Some(9));
    let current = px(&render(&mut viewer), cx, cy);
    assert!(
        luma(current) > luma(plain) + 8.0,
        "current layer {current:?} vs plain {plain:?}"
    );
    // A layer below the one being printed is not highlighted.
    viewer.options_mut().progress_line = None;
    viewer.options_mut().layer_range = (0, 8);
    let lower = px(&render(&mut viewer), cx, cy);
    viewer.options_mut().progress_line = Some(9 * 45 + 40);
    let lower_while_printing = px(&render(&mut viewer), cx, cy);
    assert!(
        (luma(lower) - luma(lower_while_printing)).abs() < 2.0,
        "{lower:?} vs {lower_while_printing:?}"
    );
}

#[test]
fn a_round_bed_is_a_disc() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    viewer.options_mut().show_axes = false;
    viewer.set_bed(BedSpec::round(200.0, 250.0));
    viewer.set_view(ViewPreset::Top);
    viewer.fit_to_view();
    // Settle any camera animation.
    for _ in 0..3 {
        render(&mut viewer);
    }
    let img = render(&mut viewer);
    let at = |x: f32, y: f32| {
        let [sx, sy] = viewer
            .project([x, y, 0.0], [W as f32, H as f32])
            .expect("on screen");
        px(&img, sx.round() as u32, sy.round() as u32)
    };
    // The plate inside the circle, nothing in the square's corners.
    let centre = at(0.0, 0.0);
    assert!(
        !near(centre, BACKGROUND, 2),
        "plate expected, got {centre:?}"
    );
    let inside = at(60.0, 60.0);
    assert!(
        !near(inside, BACKGROUND, 2),
        "plate expected, got {inside:?}"
    );
    for (x, y) in [(-92.0, -92.0), (92.0, -92.0), (92.0, 92.0), (-92.0, 92.0)] {
        let corner = at(x, y);
        assert!(
            near(corner, BACKGROUND, 2),
            "background expected at ({x}, {y}), got {corner:?}"
        );
    }
}
