//! Shared helpers for the egui_kittest tests.
//!
//! Snapshots are rendered with wgpu and compared against the PNGs in
//! `tests/snapshots/` (regenerate with `UPDATE_SNAPSHOTS=1`). They were made
//! with the reference fonts below; on a machine with different system
//! fonts, or without a GPU adapter, the widgets are still rendered but the
//! image comparison is skipped.

#![allow(dead_code)]

use std::{cell::Cell, rc::Rc, sync::Arc};

use egui::{Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder, SnapshotError};
use egui_wgpu::wgpu;
use sprint_gui::{
    fonts,
    theme::{self, Theme},
};

/// Fonts the committed snapshots were rendered with (Windows 10).
const REFERENCE_FONTS: [&str; 3] = ["Segoe UI", "Segoe UI Semibold", "Cascadia Mono"];

/// The wgpu setup for snapshots: kittest's default, but preferring a real
/// GPU over CPU rasterizers (the DX12 WARP adapter crashes on some Windows
/// machines) and Vulkan/Metal over DX12/GL. `WGPU_BACKEND` still narrows
/// the backends.
fn wgpu_setup() -> egui_wgpu::WgpuSetup {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup
        .instance_descriptor
        .backends
        .remove(wgpu::Backends::BROWSER_WEBGPU);
    setup.native_adapter_selector = Some(Arc::new(|adapters, _surface| {
        let mut adapters: Vec<_> = adapters
            .iter()
            .filter(|a| {
                let info = a.get_info();
                !(info.backend == wgpu::Backend::Dx12 && info.device_type == wgpu::DeviceType::Cpu)
            })
            .collect();
        adapters.sort_by_key(|a| {
            let info = a.get_info();
            let backend = match info.backend {
                wgpu::Backend::Vulkan | wgpu::Backend::Metal => 0,
                wgpu::Backend::Dx12 => 1,
                wgpu::Backend::Gl => 2,
                _ => 3,
            };
            let device = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::Other | wgpu::DeviceType::VirtualGpu => 2,
                wgpu::DeviceType::Cpu => 3,
            };
            (backend, device)
        });
        adapters
            .first()
            .map(|a| (*a).clone())
            .ok_or_else(|| "no usable wgpu adapter".to_owned())
    }));
    egui_wgpu::WgpuSetup::CreateNew(setup)
}

fn builder<S>(theme: Theme, size: impl Into<Vec2>) -> HarnessBuilder<S> {
    Harness::builder()
        .with_size(size)
        .with_theme(theme.into())
        .with_max_steps(64)
        .wgpu_setup(wgpu_setup())
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
