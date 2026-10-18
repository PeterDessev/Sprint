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
}

#[derive(Default, Debug)]
struct Frame {
    clicked: Option<u32>,
    hovered: Option<u32>,
}

impl Harness {
    fn new() -> Option<Harness> {
        let Some((device, queue)) = headless_device() else {
            eprintln!("skipping UI test: no wgpu adapter available");
            return None;
        };
        let mut viewer = GcodeViewer::new_headless(&device, &queue);
        viewer.set_toolpath(Arc::new(block()));
        viewer.wait_until_loaded();
        let mut h = Harness {
            ctx: egui::Context::default(),
            viewer,
            time: 0.0,
            rect: egui::Rect::NOTHING,
        };
        h.frame(vec![]);
        Some(h)
    }

    fn frame(&mut self, events: Vec<Event>) -> Frame {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, SCREEN)),
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
