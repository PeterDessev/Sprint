//! [`ConfirmDialog`]: a modal question with optional danger styling.

use egui::{
    Align, Align2, Color32, Context, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Modal,
    Modifiers, Sense, Stroke, Ui, vec2,
};

use crate::icons;
use crate::theme::{self, Palette, TextRole, with_alpha};

use super::{ActionButton, ButtonKind, icon_font};

/// How a [`ConfirmDialog`] was answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DialogResult {
    /// The confirm button (or Enter, for non-danger dialogs).
    Confirmed,
    /// Cancel, Escape or a click outside the dialog.
    Cancelled,
}

/// A modal confirmation. Show it every frame while the question is open;
/// it returns `Some` once the user answers.
///
/// Danger dialogs get a red icon and confirm button, and Enter does *not*
/// confirm them (only an explicit click does), so a stray keypress cannot
/// trigger e.g. a factory reset.
///
/// Like every egui modal, an open dialog blocks all widgets behind it. Draw
/// the E-STOP with [`EStopButton::show_above_modals`](super::EStopButton::show_above_modals)
/// so it stays reachable.
///
/// ```no_run
/// # use sprint_gui::widgets::{ConfirmDialog, DialogResult};
/// # fn demo(ctx: &egui::Context, asking: &mut bool) {
/// if *asking {
///     let answer = ConfirmDialog::new("reset-eeprom", "Reset all settings?")
///         .message("M502 restores firmware defaults. Unsaved tuning is lost.")
///         .details("M502")
///         .confirm_label("Reset")
///         .danger(true)
///         .show(ctx);
///     match answer {
///         Some(DialogResult::Confirmed) => { /* send M502 */ *asking = false; }
///         Some(DialogResult::Cancelled) => *asking = false,
///         None => {}
///     }
/// }
/// # }
/// ```
#[must_use = "call `.show(ctx)` every frame while open"]
pub struct ConfirmDialog<'a> {
    id: Id,
    title: &'a str,
    message: Option<&'a str>,
    details: Option<&'a str>,
    confirm_label: &'a str,
    cancel_label: &'a str,
    icon: Option<&'a str>,
    danger: bool,
    width: f32,
}

impl<'a> ConfirmDialog<'a> {
    /// A dialog with an id salt (unique among open dialogs) and a title.
    pub fn new(id_salt: impl std::hash::Hash + std::fmt::Debug, title: &'a str) -> Self {
        ConfirmDialog {
            id: Id::new(("sprint-confirm", id_salt)),
            title,
            message: None,
            details: None,
            confirm_label: "Confirm",
            cancel_label: "Cancel",
            icon: None,
            danger: false,
            width: 380.0,
        }
    }

    /// Body text.
    pub fn message(mut self, message: &'a str) -> Self {
        self.message = Some(message);
        self
    }

    /// Monospace detail block, e.g. the G-code that will be sent.
    pub fn details(mut self, details: &'a str) -> Self {
        self.details = Some(details);
        self
    }

    /// Label of the confirm button.
    pub fn confirm_label(mut self, label: &'a str) -> Self {
        self.confirm_label = label;
        self
    }

    /// Label of the cancel button.
    pub fn cancel_label(mut self, label: &'a str) -> Self {
        self.cancel_label = label;
        self
    }

    /// Icon next to the title (default: a warning sign for danger
    /// dialogs, a question mark otherwise).
    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Destructive action: red styling, Enter does not confirm.
    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    /// Dialog width in points (280..=640).
    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.clamp(280.0, 640.0);
        }
        self
    }

    /// Show the dialog; `Some` once answered.
    pub fn show(self, ctx: &Context) -> Option<DialogResult> {
        let p = theme::palette(ctx);
        let tone = if self.danger { p.danger } else { p.accent };
        let frame = Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(
                1.0,
                if p.dark { p.border_strong } else { p.border },
            ))
            .corner_radius(CornerRadius::same(theme::RADIUS_XL))
            .inner_margin(Margin::same(20))
            .shadow(ctx.global_style().visuals.window_shadow);
        let backdrop = if p.dark {
            Color32::from_black_alpha(150)
        } else {
            Color32::from_black_alpha(90)
        };
        let mut result = None;
        let modal = Modal::new(self.id)
            .frame(frame)
            .backdrop_color(backdrop)
            .show(ctx, |ui| {
                ui.set_width(self.width);
                ui.spacing_mut().item_spacing.y = 10.0;
                self.header(ui, &p, tone);
                if let Some(message) = self.message {
                    ui.add(egui::Label::new(egui::RichText::new(message).color(p.text_dim)).wrap());
                }
                if let Some(details) = self.details {
                    details_block(ui, &p, details);
                }
                ui.add_space(6.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let kind = if self.danger {
                        ButtonKind::Danger
                    } else {
                        ButtonKind::Primary
                    };
                    let confirm = ui.add(
                        ActionButton::new(self.confirm_label)
                            .kind(kind)
                            .min_width(96.0),
                    );
                    let cancel = ui.add(ActionButton::new(self.cancel_label).min_width(96.0));
                    // Danger dialogs focus Cancel so Space/Enter on the
                    // focused button is the safe choice.
                    if ui.memory(|m| m.focused().is_none()) && self.danger {
                        cancel.request_focus();
                    }
                    if confirm.clicked() {
                        result = Some(DialogResult::Confirmed);
                    } else if cancel.clicked() {
                        result = Some(DialogResult::Cancelled);
                    }
                });
            });
        if result.is_none() {
            if modal.should_close() {
                result = Some(DialogResult::Cancelled);
            } else if !self.danger
                && modal.is_top_modal
                && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
            {
                result = Some(DialogResult::Confirmed);
            }
        }
        result
    }

    fn header(&self, ui: &mut Ui, p: &Palette, tone: Color32) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let icon = self.icon.unwrap_or(if self.danger {
                icons::WARNING
            } else {
                icons::HELP
            });
            let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
            ui.painter().circle_filled(
                rect.center(),
                18.0,
                with_alpha(tone, if p.dark { 0.18 } else { 0.12 }),
            );
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                icon,
                icon_font(ui, 19.0),
                tone,
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(self.title)
                        .font(FontId::new(16.0, theme::font(ui, TextRole::Title).family))
                        .color(p.text),
                )
                .wrap(),
            );
        });
    }
}

fn details_block(ui: &mut Ui, p: &Palette, details: &str) {
    Frame::new()
        .fill(p.input)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(details)
                        .font(theme::builtin_font(ui, egui::TextStyle::Monospace))
                        .color(p.text),
                )
                .wrap(),
            );
        });
}
