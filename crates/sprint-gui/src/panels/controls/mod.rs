//! Left sidebar with printer controls: temperatures, motion, extruder, live
//! tuning and macros.
//!
//! Every section is a collapsible [`Card`] whose open state egui keeps in
//! its memory. The sidebar scrolls vertically by itself and lays out for
//! widths from about 280 to 480 points, so the shell can place it straight
//! into a resizable side panel.
//!
//! Safety rules the sections follow:
//! * Nothing is sent while the printer is not connected (controls look
//!   disabled).
//! * Anything that moves the machine or extrudes (jog, home, move-to,
//!   motors off, extrude, filament load) is disabled while a job is active:
//!   the host would interleave those commands with the print stream.
//!   Temperatures, overrides, fans and babystepping stay available; turning
//!   a heater off or applying a preset then asks first.
//! * Macros that ask for confirmation, and any macro started while a job
//!   runs, go through a [`ConfirmDialog`](crate::widgets::ConfirmDialog).
//!
//! Widgets never send G-code; the sections turn their actions into
//! [`sprint_host::Printer`] calls, which never block the UI thread.

mod extruder;
mod macros;
mod motion;
mod temperatures;
mod tuning;

use egui::{Align, CornerRadius, Frame, Label, Layout, Margin, Response, RichText, Ui};

use crate::context::AppCtx;
use crate::icons;
use crate::theme::{self, Tone};
use crate::widgets::Card;

/// Left sidebar with printer controls: temperatures, motion, extruder, live
/// tuning and macros.
///
/// Keeps only UI state (extrusion length, keyboard-jog toggle, slider drags,
/// the macro editor, ...); everything about the machine comes from the
/// frame's [`AppCtx`].
#[derive(Default)]
pub struct ControlsSidebar {
    temperatures: temperatures::Temperatures,
    motion: motion::Motion,
    extruder: extruder::Extruder,
    tuning: tuning::Tuning,
    macros: macros::Macros,
}

impl ControlsSidebar {
    /// Draw the panel (it scrolls vertically by itself).
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        egui::ScrollArea::vertical()
            .id_salt("sprint-controls-scroll")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = theme::GAP;
                if !ctx.connected() {
                    offline_hint(ui, ctx);
                } else if ctx.state.halted {
                    notice(
                        ui,
                        icons::WARNING,
                        "The printer is halted (emergency stop or a firmware error). It \
                         ignores commands until its firmware restarts (M999).",
                        Tone::Danger,
                    );
                }
                self.temperatures.show(ui, ctx);
                self.motion.show(ui, ctx);
                self.extruder.show(ui, ctx);
                self.tuning.show(ui, ctx);
                self.macros.show(ui, ctx);
            });
        // Dialogs are modal windows of their own, outside the scroll area.
        self.temperatures.dialogs(ui, ctx);
        self.macros.dialogs(ui, ctx);
    }
}

/// Why machine motion is unavailable right now, if it is.
fn motion_lock(ctx: &AppCtx<'_>) -> Option<&'static str> {
    if !ctx.connected() {
        Some("Connect to a printer to use these controls.")
    } else if ctx.state.halted {
        Some(
            "The printer is halted and ignores commands until its firmware restarts \
             (M999).",
        )
    } else if ctx.job_active() {
        Some(
            "Locked while a job runs: moves and extrusion would be interleaved \
             with the print. Live tuning, fans and temperatures stay available.",
        )
    } else {
        None
    }
}

/// The short notice above controls locked by [`motion_lock`] while
/// connected (offline, the sidebar's own hint explains it).
fn lock_title(ctx: &AppCtx<'_>) -> Option<&'static str> {
    if !ctx.connected() {
        None
    } else if ctx.state.halted {
        Some("Locked while the printer is halted")
    } else if ctx.job_active() {
        Some("Locked while a job runs")
    } else {
        None
    }
}

/// A collapsible sidebar section (its open state lives in egui memory).
fn section<R>(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    header_right: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    Card::new(title)
        .icon(icon)
        .collapsible(true)
        .id_salt(("sprint-controls-section", title))
        .show_with_header(ui, header_right, |ui| {
            ui.spacing_mut().item_spacing.y = theme::GAP;
            body(ui)
        })
        .inner
}

/// A tinted inline message with an icon; the text wraps to the width.
fn notice(ui: &mut Ui, icon: &str, text: &str, tone: Tone) -> Response {
    let p = theme::ui_palette(ui);
    Frame::new()
        .fill(p.tone_soft(tone))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(Margin::symmetric(9, 7))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                ui.label(RichText::new(icon).color(p.tone(tone)).size(14.0));
                ui.vertical(|ui| {
                    ui.add(Label::new(RichText::new(text).color(p.text).size(12.5)).wrap());
                });
            });
        })
        .response
}

/// A dim caption line (hints under controls).
fn caption(ui: &mut Ui, text: &str) -> Response {
    let p = theme::ui_palette(ui);
    ui.add(
        Label::new(
            RichText::new(text)
                .font(theme::font(ui, theme::TextRole::Caption))
                .color(p.text_dim),
        )
        .wrap(),
    )
}

/// A row with `left` content and `right` content aligned to the right edge.
fn split_row(ui: &mut Ui, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            right(ui);
            ui.with_layout(Layout::left_to_right(Align::Center), left);
        });
    });
}

/// The "not connected" hint at the top of the sidebar.
fn offline_hint(ui: &mut Ui, ctx: &AppCtx<'_>) {
    let (icon, text, tone) = match &ctx.state.status {
        sprint_host::ConnectionStatus::Connecting | sprint_host::ConnectionStatus::Handshaking => (
            icons::HOURGLASS,
            "Connecting… the controls unlock once the printer answers.",
            Tone::Info,
        ),
        sprint_host::ConnectionStatus::Failed(_) => (
            icons::WARNING,
            "The connection failed. Reconnect to use the controls.",
            Tone::Danger,
        ),
        _ => (
            icons::DISCONNECT,
            "Offline. Connect to a printer to use the controls.",
            Tone::Neutral,
        ),
    };
    notice(ui, icon, text, tone);
}
