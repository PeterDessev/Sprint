//! Shared helpers for the egui_kittest tests.
//!
//! Snapshots are rendered with wgpu and compared against the PNGs in
//! `tests/snapshots/` (regenerate with `UPDATE_SNAPSHOTS=1`). They were made
//! with the reference fonts below; on a machine with different system
//! fonts, or without a GPU adapter, the widgets are still rendered but the
//! image comparison is skipped.

#![allow(dead_code)]

use std::{cell::Cell, rc::Rc};

use egui::{Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder, SnapshotError};
use sprint_gui::{
    fonts,
    theme::{self, Theme},
};

#[path = "../gpu_support/mod.rs"]
mod gpu_support;

/// Fonts the committed snapshots were rendered with (Windows 10).
const REFERENCE_FONTS: [&str; 3] = ["Segoe UI", "Segoe UI Semibold", "Cascadia Mono"];

/// A harness builder rendering on the test binary's shared GPU.
fn builder<S>(theme: Theme, size: impl Into<Vec2>) -> HarnessBuilder<S> {
    let builder = Harness::builder()
        .with_size(size)
        .with_theme(theme.into())
        .with_max_steps(64);
    match gpu_support::renderer() {
        Some(renderer) => builder.renderer(renderer),
        None => builder,
    }
}

/// Margin around the content: kittest's own 8 pt outer margin plus ours.
const MARGIN: f32 = 16.0;
const KITTEST_MARGIN: f32 = 8.0;

/// Build a harness showing `app` on the theme's background with a 16 pt
/// margin. With `fit`, the window height is then shrunk or grown to the
/// content (the width stays fixed, so fill-width widgets are deterministic).
fn build<'a, S>(
    theme: Theme,
    size: Vec2,
    state: S,
    fit: bool,
    mut app: impl FnMut(&mut egui::Ui, &mut S) + 'a,
) -> Harness<'a, S> {
    let content = Rc::new(Cell::new(Rect::NOTHING));
    let recorded = Rc::clone(&content);
    let mut h = builder(theme, size).build_ui_state(
        move |ui, state| {
            let p = theme::ui_palette(ui);
            // Paint over kittest's panel frame so the whole image is `bg`.
            let screen = ui.ctx().content_rect();
            ui.painter()
                .with_clip_rect(screen)
                .rect_filled(screen, 0.0, p.bg);
            let inner = egui::Frame::new().inner_margin(MARGIN).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if !fit {
                    ui.set_min_height(ui.available_height());
                }
                ui.scope(|ui| app(ui, state)).response.rect
            });
            recorded.set(inner.inner);
        },
        state,
    );
    theme::setup(&h.ctx, theme);
    settle(&mut h);
    if fit {
        // Twice: the first resize can change wrapping and thus the height.
        for _ in 0..2 {
            let rect = content.get();
            if rect.is_positive() {
                let height = (rect.max.y + MARGIN + KITTEST_MARGIN).ceil().max(32.0);
                h.set_size(Vec2::new(size.x, height));
                settle(&mut h);
            }
        }
    }
    h
}

/// A fixed-size harness with Sprint's fonts and `theme` installed.
pub fn harness<'a>(
    theme: Theme,
    size: impl Into<Vec2>,
    mut app: impl FnMut(&mut egui::Ui) + 'a,
) -> Harness<'a> {
    build(theme, size.into(), (), false, move |ui, _| app(ui))
}

/// A harness `width` points wide whose height fits the content.
pub fn fit<'a>(theme: Theme, width: f32, mut app: impl FnMut(&mut egui::Ui) + 'a) -> Harness<'a> {
    build(theme, Vec2::new(width, 2000.0), (), true, move |ui, _| {
        app(ui)
    })
}

/// A fixed-size harness with state.
pub fn harness_state<'a, S>(
    theme: Theme,
    size: impl Into<Vec2>,
    state: S,
    app: impl FnMut(&mut egui::Ui, &mut S) + 'a,
) -> Harness<'a, S> {
    build(theme, size.into(), state, false, app)
}

/// A content-fitted harness with state.
pub fn fit_state<'a, S>(
    theme: Theme,
    width: f32,
    state: S,
    app: impl FnMut(&mut egui::Ui, &mut S) + 'a,
) -> Harness<'a, S> {
    build(theme, Vec2::new(width, 2000.0), state, true, app)
}

/// Run until animations settle (bounded, never panics on endless repaint).
pub fn settle<S>(h: &mut Harness<'_, S>) {
    let _ = h.run_ok();
}

/// Whether this machine has the fonts the snapshots were made with.
pub fn has_reference_fonts<S>(h: &Harness<'_, S>) -> bool {
    let Some(report) = fonts::report(&h.ctx) else {
        return false;
    };
    report.proportional.as_deref() == Some(REFERENCE_FONTS[0])
        && report.strong.as_deref() == Some(REFERENCE_FONTS[1])
        && report.monospace.as_deref() == Some(REFERENCE_FONTS[2])
}

/// Compare against `tests/snapshots/{name}.png` (see the module docs).
pub fn snapshot<S>(h: &mut Harness<'_, S>, name: &str) {
    if !has_reference_fonts(h) {
        eprintln!("snapshot {name}: system fonts differ from the reference, comparison skipped");
        if let Err(err) = h.render() {
            eprintln!("snapshot {name}: rendering unavailable: {err}");
        }
        return;
    }
    match h.try_snapshot(name) {
        Ok(()) => {}
        Err(SnapshotError::RenderError { err }) => {
            eprintln!("snapshot {name}: rendering unavailable ({err}), comparison skipped");
        }
        Err(err) => panic!("{err}"),
    }
}

/// Move the (virtual) mouse to `pos` and let hover animations finish.
pub fn hover<S>(h: &mut Harness<'_, S>, pos: Pos2) {
    h.hover_at(pos);
    settle(h);
}

/// Click at a position (press + release) and run one frame.
pub fn click_at<S>(h: &mut Harness<'_, S>, pos: Pos2) {
    h.hover_at(pos);
    h.step();
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}
