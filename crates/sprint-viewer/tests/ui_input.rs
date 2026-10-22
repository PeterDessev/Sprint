//! The egui widget's input handling, driven with synthetic egui input
//! (headless viewer: no egui renderer, but a real wgpu device for the
//! renderer). Skips when no wgpu adapter is available.

use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, vec2};
use glam::{Vec2, Vec3};
use sprint_gcode::{Feature, Layer, MoveKind, Segment, Toolpath};
use sprint_viewer::{GcodeViewer, ViewMode, ViewPreset, headless_device};

const SCREEN: egui::Vec2 = vec2(800.0, 600.0);

struct Harness {
    ctx: egui::Context,
    viewer: GcodeViewer,
    time: f64,
    rect: egui::Rect,
    screen: egui::Vec2,
}

#[derive(Default, Debug)]
struct Frame {
    clicked: Option<u32>,
    hovered: Option<u32>,
}

impl Harness {
    fn new() -> Option<Harness> {
        Harness::with_toolpath(block())
    }

    fn with_toolpath(toolpath: Toolpath) -> Option<Harness> {
        // `SPRINT_TEST_NO_GPU`: as without an adapter (CI; see gpu_render.rs).
        let device = headless_device().filter(|_| std::env::var_os("SPRINT_TEST_NO_GPU").is_none());
        let Some((device, queue)) = device else {
            eprintln!("skipping UI test: no wgpu adapter available");
            return None;
        };
        let mut viewer = GcodeViewer::new_headless(&device, &queue);
        viewer.set_toolpath(Arc::new(toolpath));
        viewer.wait_until_loaded();
        let mut h = Harness {
            ctx: egui::Context::default(),
            viewer,
            time: 0.0,
            rect: egui::Rect::NOTHING,
            screen: SCREEN,
        };
        h.frame(vec![]);
        Some(h)
    }

    fn frame(&mut self, events: Vec<Event>) -> Frame {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, self.screen)),
            time: Some(self.time),
            events,
            focused: true,
            ..Default::default()
        };
        let mut out = Frame::default();
        let viewer = &mut self.viewer;
        let rect = &mut self.rect;
        let mut output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::no_frame().show(ui, |ui| {
                let r = viewer.ui(ui);
                *rect = r.response.rect;
                out.clicked = r.clicked_line;
                out.hovered = r.hovered_line;
            });
        });
        // No painter here: discard the font atlas uploads.
        output.textures_delta.clear();
        out
    }

    fn idle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(vec![]);
        }
    }

    fn button(
        &mut self,
        pos: Pos2,
        button: PointerButton,
        pressed: bool,
        modifiers: Modifiers,
    ) -> Frame {
        self.frame(vec![Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        }])
    }

    fn drag(&mut self, from: Pos2, to: Pos2, button: PointerButton, modifiers: Modifiers) {
        self.frame(vec![
            Event::ModifiersChanged(modifiers),
            Event::PointerMoved(from),
        ]);
        self.button(from, button, true, modifiers);
        for i in 1..=10 {
            let p = from + (to - from) * (i as f32 / 10.0);
            self.frame(vec![Event::PointerMoved(p)]);
        }
        self.button(to, button, false, modifiers);
        self.frame(vec![Event::ModifiersChanged(Modifiers::NONE)]);
    }

    /// Screen position (points) of a world point in the active 3D view.
    fn screen_of(&self, p: Vec3) -> Pos2 {
        let size = Vec2::new(self.rect.width(), self.rect.height());
        let s = self
            .viewer
            .camera()
            .project_to_screen(p, size)
            .expect("visible");
        self.rect.min + vec2(s.x, s.y)
    }
}

fn block() -> Toolpath {
    let mut tp = Toolpath::default();
    let mut line = 0;
    for layer in 0..6u32 {
        let z = 0.2 + layer as f32 * 0.2;
        let start = tp.segments.len();
        for i in 0..40 {
            let y = 100.0 + i as f32 * 0.5;
            tp.segments.push(Segment {
                start: [100.0, y, z],
                end: [120.0, y, z],
                feedrate: 30.0,
                width: 0.5,
                height: 0.2,
                line,
                layer,
                kind: MoveKind::Extrude,
                feature: Feature::SolidInfill,
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
    tp.line_count = line;
    tp
}

#[test]
fn left_drag_orbits_and_shift_drag_pans() {
    let Some(mut h) = Harness::new() else {
        return;
    };
    let before = *h.viewer.camera();
    let c = h.rect.center();
    h.drag(
        c,
        c + vec2(120.0, 40.0),
        PointerButton::Primary,
        Modifiers::NONE,
    );
    let after = *h.viewer.camera();
    assert!(
        after.yaw < before.yaw - 0.3,
        "yaw {} -> {}",
        before.yaw,
        after.yaw
    );
    assert!(
        after.pitch > before.pitch,
        "pitch {} -> {}",
        before.pitch,
        after.pitch
    );
    assert_eq!(after.target, before.target);

    h.drag(
        c,
        c + vec2(-80.0, 0.0),
        PointerButton::Primary,
        Modifiers::SHIFT,
    );
    let panned = *h.viewer.camera();
    assert_eq!(panned.yaw, after.yaw);
    assert!((panned.target - after.target).length() > 1.0);

    h.drag(
        c,
        c + vec2(0.0, 60.0),
        PointerButton::Secondary,
        Modifiers::NONE,
    );
    assert!((h.viewer.camera().target - panned.target).length() > 1.0);
}

#[test]
fn wheel_zooms_towards_the_cursor() {
    let Some(mut h) = Harness::new() else {
        return;
    };
    let before = *h.viewer.camera();
    let p = h.rect.center();
    h.frame(vec![Event::PointerMoved(p)]);
    h.frame(vec![Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: vec2(0.0, 200.0),
        phase: TouchPhase::Move,
        modifiers: Modifiers::NONE,
    }]);
    h.idle(30);
    let after = *h.viewer.camera();
    assert!(
        after.distance < before.distance * 0.9,
        "{} -> {}",
        before.distance,
        after.distance
    );
    h.frame(vec![Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: vec2(0.0, -400.0),
        phase: TouchPhase::Move,
        modifiers: Modifiers::NONE,
    }]);
    h.idle(30);
    assert!(h.viewer.camera().distance > after.distance);
}

#[test]
fn clicking_and_hovering_a_bead_reports_its_line() {
    let Some(mut h) = Harness::new() else {
        return;
    };
    h.viewer.set_view(ViewPreset::Top);
    h.idle(40);
    // A bead in the top layer (layer 5 holds lines 200..240).
    let target = Vec3::new(110.0, 110.0, 1.1);
    let p = h.screen_of(target);
    let hover = h.frame(vec![Event::PointerMoved(p)]);
    let line = hover.hovered.expect("hovering a bead");
    assert!((200..240).contains(&line), "line {line}");
    h.button(p, PointerButton::Primary, true, Modifiers::NONE);
    let click = h.button(p, PointerButton::Primary, false, Modifiers::NONE);
    assert_eq!(click.clicked, Some(line));

    // Off the print: nothing.
    let off = h.screen_of(Vec3::new(60.0, 60.0, 0.0));
    let hover = h.frame(vec![Event::PointerMoved(off)]);
    assert_eq!(hover.hovered, None);
}

#[test]
fn number_keys_select_presets_and_z_fits() {
    let Some(mut h) = Harness::new() else {
        return;
    };
    let c = h.rect.center();
    h.frame(vec![Event::PointerMoved(c)]);
    let key = |key: Key| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    h.frame(vec![key(Key::Num1)]);
    h.idle(60);
    assert!((h.viewer.camera().pitch - FRAC_PI_2).abs() < 1e-3);
    h.frame(vec![key(Key::Num3)]);
    h.idle(60);
    assert!(h.viewer.camera().pitch.abs() < 1e-3);
    // Zoom far out, then Z brings the block back into view.
    h.viewer.camera_mut().distance = 5000.0;
    h.frame(vec![key(Key::Z)]);
    h.idle(60);
    assert!(h.viewer.camera().distance < 500.0);
}

#[test]
fn double_click_focuses_on_the_point_under_the_cursor() {
    let Some(mut h) = Harness::new() else {
        return;
    };
    let target = Vec3::new(104.0, 103.0, 1.1);
    let p = h.screen_of(target);
    h.frame(vec![Event::PointerMoved(p)]);
    for _ in 0..2 {
        h.button(p, PointerButton::Primary, true, Modifiers::NONE);
        h.button(p, PointerButton::Primary, false, Modifiers::NONE);
    }
    h.idle(60);
    let t = h.viewer.camera().target;
    assert!((t - target).length() < 1.5, "focused on {t:?}");
}

#[test]
fn layer_view_pans_with_the_left_button() {
    let Some(mut h) = Harness::new() else {
        return;
    };
    h.viewer.options_mut().view_mode = ViewMode::LayerTop;
    h.idle(2);
    let yaw = h.viewer.camera().yaw;
    let c = h.rect.center();
    h.drag(
        c,
        c + vec2(100.0, 0.0),
        PointerButton::Primary,
        Modifiers::NONE,
    );
    // The 3D camera is untouched; the layer view moved instead.
    assert_eq!(h.viewer.camera().yaw, yaw);
    let hover = h.frame(vec![Event::PointerMoved(c)]);
    let _ = hover;
}

#[test]
fn resizing_during_a_preset_animation_still_fits_the_new_size() {
    // Regression: a resize while a preset animation ran was taken as the
    // fitted aspect without refitting, so the print stayed framed for the
    // old window shape (cut off after making the window narrower).
    let Some(mut h) = Harness::new() else {
        return;
    };
    let Some(mut reference) = Harness::new() else {
        return;
    };
    let narrow = vec2(300.0, 600.0);
    reference.screen = narrow;
    reference.idle(2);
    reference.viewer.set_view(ViewPreset::Front);
    reference.idle(60);
    let want = *reference.viewer.camera();

    h.viewer.set_view(ViewPreset::Front);
    h.idle(3);
    h.screen = narrow;
    h.idle(60);
    let got = *h.viewer.camera();
    assert!(
        (got.distance / want.distance - 1.0).abs() < 0.02
            && (got.target - want.target).length() < 0.5,
        "camera {got:?}, expected {want:?}"
    );
}

/// The block plus a travel from its top layer out over the empty bed and
/// back.
fn block_with_travels() -> Toolpath {
    let mut tp = block();
    let z = tp.layers.last().map_or(1.2, |l| l.z);
    let mut line = tp.line_count;
    let layer = tp.layers.len() as u32 - 1;
    for (start, end) in [
        ([120.0, 119.5, z], [150.0, 140.0, z + 0.4]),
        ([150.0, 140.0, z + 0.4], [100.0, 100.0, z]),
    ] {
        tp.segments.push(Segment {
            start,
            end,
            feedrate: 150.0,
            duration: 0.3,
            line,
            layer,
            kind: MoveKind::Travel,
            ..Default::default()
        });
        line += 1;
    }
    if let Some(last) = tp.layers.last_mut() {
        last.segments.end = tp.segments.len();
        last.lines.end = line;
    }
    tp.line_count = line;
    tp
}

#[test]
fn hovering_and_clicking_a_shown_travel_reports_its_line() {
    let Some(mut h) = Harness::with_toolpath(block_with_travels()) else {
        return;
    };
    h.viewer.set_view(ViewPreset::Top);
    h.idle(40);
    // Room for the travels beside the block.
    h.viewer.camera_mut().distance *= 3.0;
    h.idle(2);
    // Over empty bed, a pixel or two beside the first travel's midpoint.
    let on_travel = h.screen_of(Vec3::new(135.0, 129.75, 1.4)) + vec2(1.5, 1.0);
    let hidden = h.frame(vec![Event::PointerMoved(on_travel)]);
    assert_eq!(hidden.hovered, None, "travels are hidden by default");

    h.viewer.options_mut().show_travel = true;
    let shown = h.frame(vec![Event::PointerMoved(on_travel)]);
    assert_eq!(shown.hovered, Some(240), "the first travel's line");
    h.button(on_travel, PointerButton::Primary, true, Modifiers::NONE);
    let click = h.button(on_travel, PointerButton::Primary, false, Modifiers::NONE);
    assert_eq!(click.clicked, Some(240));
    let hit = h
        .viewer
        .pick_screen(
            [on_travel.x - h.rect.min.x, on_travel.y - h.rect.min.y],
            [h.rect.width(), h.rect.height()],
        )
        .expect("travel hit");
    let tp = h.viewer.toolpath().expect("loaded");
    assert_eq!(tp.segments[hit.segment].kind, MoveKind::Travel);

    // Well away from any line: nothing.
    let far = h.screen_of(Vec3::new(135.0, 110.0, 0.0));
    assert_eq!(h.frame(vec![Event::PointerMoved(far)]).hovered, None);

    // Over the block, beads win unless the travel is right there: the
    // block's middle is far from both travels.
    let bead = h.screen_of(Vec3::new(110.0, 104.0, 1.1));
    let hovered = h.frame(vec![Event::PointerMoved(bead)]).hovered;
    assert!(hovered.is_some_and(|l| l < 240), "{hovered:?}");

    // Travels outside the shown layers are not pickable.
    h.viewer.options_mut().layer_range = (0, 2);
    let below = h.frame(vec![Event::PointerMoved(on_travel)]);
    assert_eq!(below.hovered, None);
}

/// The block plus two travels in the layer under the top one, buried under
/// its beads, and two in the top layer: one along a bead's crest and one
/// over the groove between two beads.
fn block_with_buried_travels() -> (Toolpath, [u32; 2]) {
    let mut tp = block();
    let mut line = tp.line_count;
    let mut buried = [0; 2];
    let top = tp.layers.len() - 1;
    for (layer, y) in [
        (top - 1, 110.25),
        (top - 1, 110.0),
        (top, 105.0),
        (top, 114.75),
    ] {
        let z = tp.layers[layer].z;
        let at = tp.layers[layer].segments.end;
        tp.segments.insert(
            at,
            Segment {
                start: [102.0, y, z],
                end: [118.0, y, z],
                feedrate: 150.0,
                line: tp.layers[layer].lines.end,
                layer: layer as u32,
                kind: MoveKind::Travel,
                ..Default::default()
            },
        );
        if layer < top {
            buried[(y == 110.0) as usize] = tp.layers[layer].lines.end;
        }
        // Keep lines and segment ranges consistent after the insertion.
        for (i, l) in tp.layers.iter_mut().enumerate() {
            if i == layer {
                l.segments.end += 1;
                l.lines.end += 1;
            } else if i > layer {
                l.segments.start += 1;
                l.segments.end += 1;
                l.lines.start += 1;
                l.lines.end += 1;
            }
        }
        for s in &mut tp.segments[at + 1..] {
            s.line += 1;
        }
        line += 1;
    }
    tp.line_count = line;
    (tp, buried)
}

#[test]
fn travels_hidden_under_beads_are_not_picked() {
    let (tp, buried) = block_with_buried_travels();
    let Some(mut h) = Harness::with_toolpath(tp.clone()) else {
        return;
    };
    h.viewer.options_mut().show_travel = true;
    let size = Vec2::new(800.0, 600.0);
    let top = tp.layers.len() - 1;
    let z = tp.layers[top].z;
    for distance in [200.0f32, 600.0, 1500.0] {
        for pitch in [0.3f32, 0.6, 1.0, 1.4] {
            let cam = h.viewer.camera_mut();
            cam.target = Vec3::new(110.0, 110.0, z);
            cam.distance = distance;
            cam.yaw = 0.7;
            cam.pitch = pitch;
            let world_px = h.viewer.camera().world_per_pixel(distance, size.y);
            let pick = |y: f32| {
                let p = Vec3::new(110.0, y, z);
                let s = h
                    .viewer
                    .camera()
                    .project_to_screen(p, size)
                    .expect("visible");
                let hit = h.viewer.pick_screen([s.x, s.y], size.to_array())?;
                Some(tp.segments[hit.segment])
            };
            for dy in [-1.0f32, -0.5, 0.0, 0.25, 0.5, 1.0] {
                let dy = dy * world_px;
                // Over the buried travels: the top layer's beads (or, seen
                // at a grazing angle from afar, a top travel a few pixels
                // away on screen).
                let hit = pick(110.25 + dy).expect("the block");
                assert!(
                    hit.layer as usize == top && !buried.contains(&hit.line),
                    "{distance} mm, pitch {pitch}, {dy} mm off: picked {hit:?}"
                );
                // Travels lying on the top layer are drawn over it.
                for y in [105.0, 114.75] {
                    let hit = pick(y + dy).expect("the block");
                    assert!(
                        hit.kind == MoveKind::Travel && hit.layer as usize == top,
                        "{distance} mm, pitch {pitch}, {dy} mm off {y}: picked {hit:?}"
                    );
                }
            }
        }
    }
}
