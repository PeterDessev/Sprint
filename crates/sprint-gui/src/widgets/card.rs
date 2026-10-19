//! Layout building blocks: cards, section headers and empty states.

use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, InnerResponse, Layout,
    Margin, Pos2, Rect, Response, Sense, Shadow, Stroke, Ui, Vec2, WidgetInfo, WidgetType,
    collapsing_header::CollapsingState,
    text::{LayoutJob, TextFormat},
    vec2,
};

use crate::theme::{self, CARD_PADDING, Palette, RADIUS_LG, TextRole, mix};

use super::{ActionButton, ButtonKind, icon_font};

/// The frame used by cards: surface fill, hairline border, large radius.
pub fn card_frame(p: &Palette) -> Frame {
    let shadow = if p.dark {
        Shadow::NONE
    } else {
        Shadow {
            offset: [0, 1],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(12),
        }
    };
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(RADIUS_LG))
        .inner_margin(Margin::same(CARD_PADDING))
        .shadow(shadow)
}

/// A raised section with an icon + title header and optional controls on
/// the right of the header.
///
/// ```no_run
/// # use sprint_gui::{icons, widgets::{card, IconButton}};
/// # fn demo(ui: &mut egui::Ui) {
/// card(ui, icons::FAN, "Fan", |ui| { ui.add(IconButton::new(icons::OFF).ghost().small()); }, |ui| {
///     ui.label("Part cooling: 100%");
/// });
/// # }
/// ```
pub fn card<R>(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    header_right: impl FnOnce(&mut Ui),
    contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    Card::new(title)
        .icon(icon)
        .show_fixed(ui, header_right, contents)
}

/// Builder for a [`card`] with more options: subtitle, collapsible body,
/// custom fill and width behavior.
///
/// ```no_run
/// # use sprint_gui::{icons, widgets::Card};
/// # fn demo(ui: &mut egui::Ui) {
/// Card::new("Macros").icon(icons::MACRO).collapsible(true).show(ui, |ui| {
///     ui.label("No macros yet");
/// });
/// # }
/// ```
#[must_use = "call `.show(ui, ...)`"]
pub struct Card<'a> {
    title: &'a str,
    icon: Option<&'a str>,
    subtitle: Option<&'a str>,
    collapsible: Option<bool>,
    id_salt: Option<Id>,
    fill: Option<Color32>,
    fill_width: bool,
    padding: i8,
}

impl<'a> Card<'a> {
    /// A card with a title. An empty title hides the header.
    pub fn new(title: &'a str) -> Self {
        Card {
            title,
            icon: None,
            subtitle: None,
            collapsible: None,
            id_salt: None,
            fill: None,
            fill_width: true,
            padding: CARD_PADDING,
        }
    }

    /// Header icon.
    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Dim text after the title.
    pub fn subtitle(mut self, subtitle: &'a str) -> Self {
        self.subtitle = Some(subtitle);
        self
    }

    /// Let the header toggle the body; `default_open` is the initial state
    /// (remembered per card id afterwards).
    pub fn collapsible(mut self, default_open: bool) -> Self {
        self.collapsible = Some(default_open);
        self
    }

    /// Id salt for the collapsed state (defaults to the title; set it when
    /// two collapsible cards in the same parent share a title).
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(Id::new(salt));
        self
    }

    /// Override the surface color.
    pub fn fill(mut self, fill: Color32) -> Self {
        self.fill = Some(fill);
        self
    }

    /// Stretch to the available width (default `true`).
    pub fn fill_width(mut self, fill: bool) -> Self {
        self.fill_width = fill;
        self
    }

    /// Inner margin in points (default [`CARD_PADDING`]).
    pub fn padding(mut self, padding: i8) -> Self {
        self.padding = padding.max(0);
        self
    }

    /// Show the card. The inner value is `None` only when a collapsible
    /// card is collapsed.
    pub fn show<R>(
        self,
        ui: &mut Ui,
        contents: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<Option<R>> {
        self.show_with_header(ui, |_| {}, contents)
    }

    /// Show the card with extra controls on the right of the header.
    pub fn show_with_header<R>(
        self,
        ui: &mut Ui,
        header_right: impl FnOnce(&mut Ui),
        contents: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<Option<R>> {
        let Some(default_open) = self.collapsible else {
            let InnerResponse { inner, response } = self.show_fixed(ui, header_right, contents);
            return InnerResponse {
                inner: Some(inner),
                response,
            };
        };
        let p = theme::ui_palette(ui);
        let id = super::widget_id(ui, self.id_salt.unwrap_or_else(|| Id::new(self.title)));
        self.frame(&p).show(ui, |ui| {
            if self.fill_width {
                ui.set_min_width(ui.available_width());
            }
            let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
            let openness = state.openness(ui.ctx());
            let header = header_row(ui, &p, &self, Some(openness), header_right);
            if header.clicked() {
                state.toggle(ui);
            }
            state
                .show_body_unindented(ui, |ui| {
                    ui.add_space(6.0);
                    contents(ui)
                })
                .map(|body| body.inner)
        })
    }

    fn frame(&self, p: &Palette) -> Frame {
        let frame = card_frame(p).inner_margin(Margin::same(self.padding));
        match self.fill {
            Some(fill) => frame.fill(fill),
            None => frame,
        }
    }

    /// A card whose body is always shown.
    fn show_fixed<R>(
        self,
        ui: &mut Ui,
        header_right: impl FnOnce(&mut Ui),
        contents: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<R> {
        let p = theme::ui_palette(ui);
        self.frame(&p).show(ui, |ui| {
            if self.fill_width {
                ui.set_min_width(ui.available_width());
            }
            if !self.title.is_empty() || self.icon.is_some() {
                header_row(ui, &p, &self, None, header_right);
                ui.add_space(6.0);
            }
            contents(ui)
        })
    }
}

/// Paint the header; returns the clickable header response (collapsible
/// cards only; otherwise a hover response).
fn header_row(
    ui: &mut Ui,
    p: &Palette,
    card: &Card<'_>,
    openness: Option<f32>,
    header_right: impl FnOnce(&mut Ui),
) -> Response {
    let height = 24.0;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let sense = if openness.is_some() {
        Sense::click()
    } else {
        Sense::hover()
    };
    // Header controls first, right-aligned, so the title can be truncated
    // to the space they leave instead of running underneath them.
    let mut right = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    right.spacing_mut().item_spacing.x = 4.0;
    header_right(&mut right);
    let used_right = right.min_rect();
    let text_end = if used_right.width() > 0.0 {
        used_right.min.x - 8.0
    } else {
        rect.max.x
    };
    let mut x = rect.min.x;
    let painter = ui.painter().clone();
    if let Some(openness) = openness {
        let caret = if openness > 0.5 {
            crate::icons::CARET_DOWN
        } else {
            crate::icons::CARET_RIGHT
        };
        painter.text(
            Pos2::new(x + 6.0, rect.center().y),
            Align2::CENTER_CENTER,
            caret,
            icon_font(ui, 12.0),
            p.text_faint,
        );
        x += 16.0;
    }
    if let Some(icon) = card.icon {
        painter.text(
            Pos2::new(x + 8.0, rect.center().y),
            Align2::CENTER_CENTER,
            icon,
            icon_font(ui, 16.0),
            p.text_dim,
        );
        x += 24.0;
    }
    let title_font = theme::font(ui, TextRole::Title);
    let title = truncated(&painter, card.title, title_font, p.text, text_end - x);
    let elided = title.elided;
    let title_rect = Rect::from_min_size(
        Pos2::new(x, rect.center().y - title.size().y / 2.0),
        title.size(),
    );
    painter.galley(title_rect.min, title, p.text);
    x = title_rect.max.x;
    // The subtitle only gets the room that is left (none when the title
    // was cut short).
    if let Some(sub) = card.subtitle
        && !elided
        && text_end - (x + 8.0) >= 24.0
    {
        let sub = truncated(
            &painter,
            sub,
            FontId::proportional(12.0),
            p.text_dim,
            text_end - (x + 8.0),
        );
        let pos = Pos2::new(x + 8.0, rect.center().y - sub.size().y / 2.0);
        painter.galley(pos, sub, p.text_dim);
    }
    // The clickable part excludes the header controls.
    let click_rect = Rect::from_min_max(
        rect.min,
        Pos2::new(
            if used_right.width() > 0.0 {
                used_right.min.x - 4.0
            } else {
                rect.max.x
            },
            rect.max.y,
        ),
    );
    let response = ui.interact(
        click_rect,
        super::widget_id(ui, ("card-header", card.title)),
        sense,
    );
    let response = if elided {
        response.on_hover_text(card.title)
    } else {
        response
    };
    if openness.is_some() {
        response.widget_info(|| {
            WidgetInfo::labeled(WidgetType::CollapsingHeader, ui.is_enabled(), card.title)
        });
        return response.on_hover_cursor(CursorIcon::PointingHand);
    }
    response
}

/// `text` on one line, cut with "…" when wider than `max_width`.
fn truncated(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    painter.layout_job(job)
}

/// An uppercase section label with a hairline to the right.
///
/// ```no_run
/// # fn demo(ui: &mut egui::Ui) {
/// sprint_gui::widgets::section_header(ui, "Temperatures");
/// # }
/// ```
pub fn section_header(ui: &mut Ui, title: &str) -> Response {
    SectionHeader::new(title).show(ui)
}

/// Builder for [`section_header`].
#[must_use = "call `.show(ui)`"]
pub struct SectionHeader<'a> {
    title: &'a str,
    icon: Option<&'a str>,
    line: bool,
}

impl<'a> SectionHeader<'a> {
    /// A section header with a trailing line.
    pub fn new(title: &'a str) -> Self {
        SectionHeader {
            title,
            icon: None,
            line: true,
        }
    }

    /// Leading icon.
    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Draw the trailing hairline (default `true`).
    pub fn line(mut self, line: bool) -> Self {
        self.line = line;
        self
    }

    /// Show it, taking the full available width.
    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::ui_palette(ui);
        let font = theme::font(ui, TextRole::Section);
        let mut job = LayoutJob::default();
        job.append(
            &self.title.to_uppercase(),
            0.0,
            TextFormat {
                font_id: font,
                color: p.text_dim,
                extra_letter_spacing: 0.8,
                ..TextFormat::default()
            },
        );
        // A label wider than the panel is cut with "…" instead of
        // spilling out of it.
        let icon_w = if self.icon.is_some() { 18.0 } else { 0.0 };
        job.wrap =
            egui::text::TextWrapping::truncate_at_width((ui.available_width() - icon_w).max(0.0));
        let galley = ui.painter().layout_job(job);
        let height = galley.size().y.max(16.0) + 4.0;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, self.title));
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let mut x = rect.min.x;
            if let Some(icon) = self.icon {
                painter.text(
                    Pos2::new(x + 6.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    icon,
                    icon_font(ui, 13.0),
                    p.text_dim,
                );
                x += 18.0;
            }
            let pos = Pos2::new(x, rect.center().y - galley.size().y / 2.0);
            let text_end = pos.x + galley.size().x;
            painter.galley(pos, galley, p.text_dim);
            if self.line && text_end + 10.0 < rect.max.x {
                painter.hline(
                    (text_end + 10.0)..=rect.max.x,
                    rect.center().y,
                    Stroke::new(1.0, p.border),
                );
            }
        }
        response
    }
}

/// What [`EmptyState::show`] returns.
#[derive(Debug)]
pub struct EmptyStateResponse {
    /// The whole placeholder area.
    pub response: Response,
    /// The action button, if one was configured.
    pub action: Option<Response>,
}

impl EmptyStateResponse {
    /// Whether the action button was clicked this frame.
    pub fn action_clicked(&self) -> bool {
        self.action.as_ref().is_some_and(Response::clicked)
    }
}

/// A centered placeholder for empty panels: icon, title, message and an
/// optional action button.
///
/// ```no_run
/// # use sprint_gui::{icons, widgets::EmptyState};
/// # fn demo(ui: &mut egui::Ui) {
/// let r = EmptyState::new(icons::FILE, "No file loaded")
///     .message("Open a G-code file or drop one here.")
///     .action("Open file…")
///     .show(ui);
/// if r.action_clicked() { /* open dialog */ }
/// # }
/// ```
#[must_use = "call `.show(ui)`"]
pub struct EmptyState<'a> {
    icon: &'a str,
    title: &'a str,
    message: Option<&'a str>,
    action: Option<&'a str>,
    action_icon: Option<&'a str>,
    action_blocked: Option<&'a str>,
}

impl<'a> EmptyState<'a> {
    /// A placeholder with an icon and a title.
    pub fn new(icon: &'a str, title: &'a str) -> Self {
        EmptyState {
            icon,
            title,
            message: None,
            action: None,
            action_icon: None,
            action_blocked: None,
        }
    }

    /// Disable the action button when `reason` is `Some`, with the reason
    /// as its hover text.
    pub fn action_blocked(mut self, reason: Option<&'a str>) -> Self {
        self.action_blocked = reason;
        self
    }

    /// Explanatory text below the title.
    pub fn message(mut self, message: &'a str) -> Self {
        self.message = Some(message);
        self
    }

    /// Add an action button with this label.
    pub fn action(mut self, label: &'a str) -> Self {
        self.action = Some(label);
        self
    }

    /// Icon of the action button.
    pub fn action_icon(mut self, icon: &'a str) -> Self {
        self.action_icon = Some(icon);
        self
    }

    /// Show it centered in the available width.
    pub fn show(self, ui: &mut Ui) -> EmptyStateResponse {
        let p = theme::ui_palette(ui);
        let mut action = None;
        let inner = ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
            if ui.is_rect_visible(rect) {
                ui.painter()
                    .circle_filled(rect.center(), 28.0, mix(p.raised, p.surface, 0.3));
                ui.painter()
                    .circle_stroke(rect.center(), 28.0, Stroke::new(1.0, p.border));
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    self.icon,
                    icon_font(ui, 26.0),
                    p.text_dim,
                );
            }
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(self.title)
                    .font(theme::font(ui, TextRole::Title))
                    .color(p.text),
            );
            if let Some(message) = self.message {
                ui.add_space(2.0);
                let width = ui.available_width().min(320.0);
                ui.allocate_ui(vec2(width, 0.0), |ui| {
                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(message).color(p.text_dim)).wrap(),
                        );
                    });
                });
            }
            if let Some(label) = self.action {
                ui.add_space(10.0);
                let mut button = ActionButton::new(label).kind(ButtonKind::Normal);
                if let Some(icon) = self.action_icon {
                    button = button.icon(icon);
                }
                let r = ui.add_enabled(self.action_blocked.is_none(), button);
                action = Some(match self.action_blocked {
                    Some(reason) => r.on_disabled_hover_text(reason),
                    None => r,
                });
            }
            ui.add_space(12.0);
        });
        EmptyStateResponse {
            response: inner.response,
            action,
        }
    }
}
