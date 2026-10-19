//! Status indicators: pills, badges and chips.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Response, Sense, Stroke, StrokeKind,
    Ui, Vec2, Widget, WidgetInfo, WidgetType, vec2,
};

use crate::theme::{self, TextRole, Tone, mix, with_alpha};

use super::{hover_t, icon_font, paint_focus_ring};

/// A capsule with a colored dot and a label: connection or job state.
///
/// ```no_run
/// # use sprint_gui::{theme::Tone, widgets::StatusPill};
/// # fn demo(ui: &mut egui::Ui) {
/// ui.add(StatusPill::new("Connected", Tone::Success));
/// ui.add(StatusPill::new("Connecting", Tone::Warning).pulse(true));
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct StatusPill<'a> {
    label: &'a str,
    tone: Tone,
    pulse: bool,
    icon: Option<&'a str>,
}

impl<'a> StatusPill<'a> {
    /// A pill with a dot in `tone`.
    pub fn new(label: &'a str, tone: Tone) -> Self {
        StatusPill {
            label,
            tone,
            pulse: false,
            icon: None,
        }
    }

    /// Animate the dot (for transient states such as "Connecting").
    pub fn pulse(mut self, pulse: bool) -> Self {
        self.pulse = pulse;
        self
    }

    /// Show an icon instead of the dot.
    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }
}

impl Widget for StatusPill<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let font = FontId::proportional(13.0);
        let galley = ui
            .painter()
            .layout_no_wrap(self.label.to_owned(), font, Color32::PLACEHOLDER);
        let lead = if self.icon.is_some() { 14.0 } else { 8.0 };
        let height = 24.0;
        let width = 10.0 + lead + 7.0 + galley.size().x + 11.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, self.label));
        if ui.is_rect_visible(rect) {
            let p = theme::ui_palette(ui);
            let tone = p.tone(self.tone);
            let painter = ui.painter();
            painter.rect(
                rect,
                CornerRadius::same(255),
                p.tone_soft(self.tone),
                Stroke::new(1.0, with_alpha(tone, 0.28)),
                StrokeKind::Inside,
            );
            let lead_center = Pos2::new(rect.min.x + 10.0 + lead / 2.0, rect.center().y);
            if let Some(icon) = self.icon {
                painter.text(
                    lead_center,
                    Align2::CENTER_CENTER,
                    icon,
                    icon_font(ui, 14.0),
                    tone,
                );
            } else {
                if self.pulse {
                    let t = ui.input(|i| i.time) as f32;
                    let phase = (t * 1.6).fract();
                    painter.circle_filled(
                        lead_center,
                        4.0 + 5.0 * phase,
                        with_alpha(tone, 0.45 * (1.0 - phase)),
                    );
                    ui.ctx().request_repaint();
                }
                painter.circle_filled(lead_center, 4.0, tone);
            }
            let text_color = mix(p.text, tone, if p.dark { 0.15 } else { 0.25 });
            let pos = Pos2::new(
                lead_center.x + lead / 2.0 + 7.0,
                rect.center().y - galley.size().y / 2.0,
            );
            painter.galley(pos, galley, text_color);
        }
        response
    }
}

/// A small label in a tinted capsule: counts, tags, firmware flags.
///
/// ```no_run
/// # use sprint_gui::{theme::Tone, widgets::Badge};
/// # fn demo(ui: &mut egui::Ui) {
/// ui.add(Badge::new("BETA").tone(Tone::Info));
/// ui.add(Badge::new("3").tone(Tone::Danger).filled(true));
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct Badge<'a> {
    text: &'a str,
    tone: Tone,
    filled: bool,
}

impl<'a> Badge<'a> {
    /// A neutral, soft badge.
    pub fn new(text: &'a str) -> Self {
        Badge {
            text,
            tone: Tone::Neutral,
            filled: false,
        }
    }

    /// Color intent.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Solid fill instead of a soft tint.
    pub fn filled(mut self, filled: bool) -> Self {
        self.filled = filled;
        self
    }
}

impl Badge<'_> {
    const HEIGHT: f32 = 18.0;

    fn galley(&self, ui: &Ui) -> std::sync::Arc<egui::Galley> {
        let font = FontId::new(11.0, theme::font(ui, TextRole::Section).family);
        ui.painter()
            .layout_no_wrap(self.text.to_owned(), font, Color32::PLACEHOLDER)
    }

    /// The width the badge will take in `ui`, for layout decisions.
    pub fn width(&self, ui: &Ui) -> f32 {
        (self.galley(ui).size().x + 12.0).max(Self::HEIGHT)
    }
}

impl Widget for Badge<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let galley = self.galley(ui);
        let height = Self::HEIGHT;
        let width = (galley.size().x + 12.0).max(height);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, self.text));
        if ui.is_rect_visible(rect) {
            let p = theme::ui_palette(ui);
            let tone = p.tone(self.tone);
            let (fill, fg) = if self.filled {
                let fill = if self.tone == Tone::Neutral {
                    p.raised_hover
                } else {
                    tone
                };
                (fill, p.text_on(fill))
            } else {
                let fg = if self.tone == Tone::Neutral {
                    p.text_dim
                } else {
                    tone
                };
                (p.tone_soft(self.tone), fg)
            };
            ui.painter()
                .rect_filled(rect, CornerRadius::same(theme::RADIUS_SM), fill);
            let pos = rect.center() - galley.size() / 2.0;
            ui.painter().galley(pos, galley, fg);
        }
        response
    }
}

/// A selectable capsule (filters, presets). The caller flips the state on
/// click.
///
/// ```no_run
/// # use sprint_gui::widgets::Chip;
/// # fn demo(ui: &mut egui::Ui, show_travel: &mut bool) {
/// if ui.add(Chip::new("Travel").selected(*show_travel)).clicked() {
///     *show_travel = !*show_travel;
/// }
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct Chip<'a> {
    label: &'a str,
    icon: Option<&'a str>,
    selected: bool,
    color: Option<Color32>,
}

impl<'a> Chip<'a> {
    /// An unselected chip.
    pub fn new(label: &'a str) -> Self {
        Chip {
            label,
            icon: None,
            selected: false,
            color: None,
        }
    }

    /// Leading icon.
    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Selected state.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// A color swatch dot before the label (legend chips).
    pub fn swatch(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }
}

impl Widget for Chip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let font = FontId::proportional(13.0);
        let galley = ui
            .painter()
            .layout_no_wrap(self.label.to_owned(), font, Color32::PLACEHOLDER);
        let lead = match (self.icon, self.color) {
            (Some(_), _) => 14.0 + 6.0,
            (None, Some(_)) => 8.0 + 6.0,
            (None, None) => 0.0,
        };
        let height = 26.0;
        let width = 12.0 + lead + galley.size().x + 12.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::Checkbox,
                ui.is_enabled(),
                self.selected,
                self.label,
            )
        });
        if ui.is_rect_visible(rect) {
            let p = theme::ui_palette(ui);
            let hover = hover_t(ui, &response);
            let (fill, stroke, fg) = if self.selected {
                (
                    mix(p.accent_soft, with_alpha(p.accent, 0.26), hover),
                    Stroke::new(1.0, with_alpha(p.accent, 0.75)),
                    if p.dark {
                        p.accent_hover
                    } else {
                        p.accent_pressed
                    },
                )
            } else {
                (
                    mix(p.raised, p.raised_hover, hover),
                    Stroke::new(1.0, mix(p.border, p.border_strong, hover)),
                    mix(p.text_dim, p.text, hover),
                )
            };
            let radius = CornerRadius::same(255);
            let painter = ui.painter();
            painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
            let mut x = rect.min.x + 12.0;
            if let Some(icon) = self.icon {
                painter.text(
                    Pos2::new(x + 7.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    icon,
                    icon_font(ui, 14.0),
                    fg,
                );
                x += lead;
            } else if let Some(color) = self.color {
                painter.circle_filled(Pos2::new(x + 4.0, rect.center().y), 4.0, color);
                x += lead;
            }
            let pos = Pos2::new(x, rect.center().y - galley.size().y / 2.0);
            painter.galley(pos, galley, fg);
            paint_focus_ring(ui, &response, rect, radius);
        }
        response.on_hover_cursor(CursorIcon::PointingHand)
    }
}

/// A plain colored dot (inline status marker), `diameter` points wide.
pub fn status_dot(ui: &mut Ui, tone: Tone, diameter: f32) -> Response {
    let diameter = if diameter.is_finite() {
        diameter.clamp(2.0, 64.0)
    } else {
        8.0
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = theme::ui_palette(ui);
        ui.painter()
            .circle_filled(rect.center(), diameter / 2.0, p.tone(tone));
    }
    response
}
