//! Buttons: icon buttons, labelled action buttons and the E-STOP button.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2, Widget, WidgetInfo, WidgetText, WidgetType, epaint::PathShape, vec2,
};

use crate::theme::{self, Palette, RADIUS, RADIUS_LG, TextRole, mix, with_alpha};

use super::{hover_t, icon_font, paint_focus_ring};

/// Visual weight of a button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ButtonKind {
    /// No background until hovered: toolbars, inline actions.
    Ghost,
    /// A raised neutral button.
    #[default]
    Normal,
    /// The one main action of a group, filled with the accent.
    Primary,
    /// A destructive action, filled red.
    Danger,
}

/// Button size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ButtonSize {
    /// 24 pt high, for dense rows and table cells.
    Small,
    /// 30 pt high, the default.
    #[default]
    Regular,
    /// 40 pt high, for primary machine actions.
    Large,
}

impl ButtonSize {
    /// Height in points.
    pub fn height(self) -> f32 {
        match self {
            ButtonSize::Small => 24.0,
            ButtonSize::Regular => 30.0,
            ButtonSize::Large => 40.0,
        }
    }

    /// Icon glyph size in points.
    pub fn icon_size(self) -> f32 {
        match self {
            ButtonSize::Small => 14.0,
            ButtonSize::Regular => 16.0,
            ButtonSize::Large => 20.0,
        }
    }

    /// Label font size in points.
    pub fn font_size(self) -> f32 {
        match self {
            ButtonSize::Small => 13.0,
            ButtonSize::Regular => 14.0,
            ButtonSize::Large => 15.0,
        }
    }

    /// Horizontal padding in points.
    pub fn padding(self) -> f32 {
        match self {
            ButtonSize::Small => 8.0,
            ButtonSize::Regular => 12.0,
            ButtonSize::Large => 16.0,
        }
    }
}

/// Resolved colors of a button in one interaction state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ButtonColors {
    pub fill: Color32,
    pub stroke: Stroke,
    pub fg: Color32,
}

/// Colors for `kind` given hover amount (0..=1), pressed and selected.
pub(crate) fn button_colors(
    p: &Palette,
    kind: ButtonKind,
    hover: f32,
    pressed: bool,
    selected: bool,
) -> ButtonColors {
    match kind {
        ButtonKind::Ghost => {
            let base = if selected {
                p.accent_soft
            } else {
                Color32::TRANSPARENT
            };
            let hovered = if selected {
                with_alpha(p.accent, 0.26)
            } else {
                p.raised
            };
            let fill = if pressed {
                mix(p.raised, p.bg, 0.3)
            } else {
                mix(base, hovered, hover)
            };
            let fg = if selected {
                p.accent
            } else {
                mix(p.text_dim, p.text, hover)
            };
            ButtonColors {
                fill,
                stroke: Stroke::NONE,
                fg,
            }
        }
        ButtonKind::Normal => {
            if selected {
                return ButtonColors {
                    fill: mix(p.accent_soft, with_alpha(p.accent, 0.26), hover),
                    stroke: Stroke::new(1.0, with_alpha(p.accent, 0.7)),
                    fg: if p.dark {
                        p.accent_hover
                    } else {
                        p.accent_pressed
                    },
                };
            }
            let fill = if pressed {
                mix(p.raised, p.bg, 0.35)
            } else {
                mix(p.raised, p.raised_hover, hover)
            };
            ButtonColors {
                fill,
                stroke: Stroke::new(1.0, mix(p.border, p.border_strong, hover)),
                fg: p.text,
            }
        }
        ButtonKind::Primary => ButtonColors {
            fill: if pressed {
                p.accent_pressed
            } else {
                mix(p.accent, p.accent_hover, hover)
            },
            stroke: Stroke::NONE,
            fg: p.on_accent,
        },
        ButtonKind::Danger => ButtonColors {
            fill: if pressed {
                p.danger_pressed
            } else {
                mix(p.danger_fill, p.danger_hover, hover)
            },
            stroke: Stroke::NONE,
            fg: p.on_danger,
        },
    }
}

/// A square button showing one icon, with a tooltip.
///
/// ```no_run
/// # use sprint_gui::{icons, widgets::{IconButton, ButtonKind}};
/// # fn demo(ui: &mut egui::Ui) {
/// if ui.add(IconButton::new(icons::HOME).tooltip("Home all")).clicked() { /* ... */ }
/// ui.add(IconButton::new(icons::DELETE).kind(ButtonKind::Danger).small());
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct IconButton<'a> {
    icon: &'a str,
    tooltip: Option<WidgetText>,
    kind: ButtonKind,
    size: ButtonSize,
    selected: bool,
    tint: Option<Color32>,
}

impl<'a> IconButton<'a> {
    /// A normal, regular-size icon button.
    pub fn new(icon: &'a str) -> Self {
        IconButton {
            icon,
            tooltip: None,
            kind: ButtonKind::Normal,
            size: ButtonSize::Regular,
            selected: false,
            tint: None,
        }
    }

    /// Tooltip shown on hover; also the accessible label.
    pub fn tooltip(mut self, text: impl Into<WidgetText>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Visual weight.
    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    /// Shorthand for [`ButtonKind::Ghost`].
    pub fn ghost(self) -> Self {
        self.kind(ButtonKind::Ghost)
    }

    /// Shorthand for [`ButtonKind::Primary`].
    pub fn primary(self) -> Self {
        self.kind(ButtonKind::Primary)
    }

    /// Shorthand for [`ButtonKind::Danger`].
    pub fn danger(self) -> Self {
        self.kind(ButtonKind::Danger)
    }

    /// Size.
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Shorthand for [`ButtonSize::Small`].
    pub fn small(self) -> Self {
        self.size(ButtonSize::Small)
    }

    /// Shorthand for [`ButtonSize::Large`].
    pub fn large(self) -> Self {
        self.size(ButtonSize::Large)
    }

    /// Show as toggled on (accent tint), e.g. for view toggles.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Override the icon color (e.g. a heater color).
    pub fn tint(mut self, color: Color32) -> Self {
        self.tint = Some(color);
        self
    }
}

impl Widget for IconButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let side = self.size.height();
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
        let label = self
            .tooltip
            .as_ref()
            .map_or_else(|| self.icon.to_owned(), |t| t.text().to_owned());
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::Button,
                ui.is_enabled(),
                self.selected,
                label.clone(),
            )
        });
        if ui.is_rect_visible(rect) {
            let p = theme::ui_palette(ui);
            let hover = hover_t(ui, &response);
            let pressed = response.is_pointer_button_down_on();
            let c = button_colors(&p, self.kind, hover, pressed, self.selected);
            let radius = CornerRadius::same(RADIUS);
            ui.painter()
                .rect(rect, radius, c.fill, c.stroke, StrokeKind::Inside);
            let fg = self.tint.unwrap_or(c.fg);
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                self.icon,
                icon_font(ui, self.size.icon_size()),
                fg,
            );
            paint_focus_ring(ui, &response, rect, radius);
        }
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        match self.tooltip {
            Some(t) => response.on_hover_text(t),
            None => response,
        }
    }
}

/// A labelled button with an optional leading icon. Use [`ButtonSize::Large`]
/// and [`ActionButton::fill_width`] for big machine actions.
///
/// ```no_run
/// # use sprint_gui::{icons, widgets::{ActionButton, ButtonKind, ButtonSize}};
/// # fn demo(ui: &mut egui::Ui) {
/// let print = ActionButton::new("Start print").icon(icons::PLAY).primary().large();
/// if ui.add(print).clicked() { /* ... */ }
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct ActionButton<'a> {
    icon: Option<&'a str>,
    label: &'a str,
    tooltip: Option<WidgetText>,
    kind: ButtonKind,
    size: ButtonSize,
    selected: bool,
    fill_width: bool,
    min_width: f32,
}

impl<'a> ActionButton<'a> {
    /// A normal, regular-size button with a text label.
    pub fn new(label: &'a str) -> Self {
        ActionButton {
            icon: None,
            label,
            tooltip: None,
            kind: ButtonKind::Normal,
            size: ButtonSize::Regular,
            selected: false,
            fill_width: false,
            min_width: 0.0,
        }
    }

    /// Leading icon.
    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Tooltip shown on hover.
    pub fn tooltip(mut self, text: impl Into<WidgetText>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Visual weight.
    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    /// Shorthand for [`ButtonKind::Ghost`].
    pub fn ghost(self) -> Self {
        self.kind(ButtonKind::Ghost)
    }

    /// Shorthand for [`ButtonKind::Primary`].
    pub fn primary(self) -> Self {
        self.kind(ButtonKind::Primary)
    }

    /// Shorthand for [`ButtonKind::Danger`].
    pub fn danger(self) -> Self {
        self.kind(ButtonKind::Danger)
    }

    /// Size.
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Shorthand for [`ButtonSize::Small`].
    pub fn small(self) -> Self {
        self.size(ButtonSize::Small)
    }

    /// Shorthand for [`ButtonSize::Large`].
    pub fn large(self) -> Self {
        self.size(ButtonSize::Large)
    }

    /// Show as toggled on.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Stretch to the available width.
    pub fn fill_width(mut self, fill: bool) -> Self {
        self.fill_width = fill;
        self
    }

    /// Minimum width in points.
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = width.max(0.0);
        self
    }
}

impl Widget for ActionButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = self.size;
        let large = size == ButtonSize::Large;
        let font = if large || self.kind == ButtonKind::Primary || self.kind == ButtonKind::Danger {
            FontId::new(size.font_size(), theme::font(ui, TextRole::Title).family)
        } else {
            FontId::proportional(size.font_size())
        };
        let galley = ui
            .painter()
            .layout_no_wrap(self.label.to_owned(), font, Color32::PLACEHOLDER);
        let icon_w = if self.icon.is_some() {
            size.icon_size() + 6.0
        } else {
            0.0
        };
        let content_w = icon_w + galley.size().x;
        let mut width = (content_w + 2.0 * size.padding()).max(self.min_width);
        if self.fill_width {
            width = width.max(ui.available_width());
        }
        let (rect, response) = ui.allocate_exact_size(vec2(width, size.height()), Sense::click());
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::Button,
                ui.is_enabled(),
                self.selected,
                self.label,
            )
        });
        if ui.is_rect_visible(rect) {
            let p = theme::ui_palette(ui);
            let hover = hover_t(ui, &response);
            let pressed = response.is_pointer_button_down_on();
            let c = button_colors(&p, self.kind, hover, pressed, self.selected);
            let radius = CornerRadius::same(if large { RADIUS_LG } else { RADIUS });
            ui.painter()
                .rect(rect, radius, c.fill, c.stroke, StrokeKind::Inside);
            let mut x = rect.center().x - content_w / 2.0;
            if let Some(icon) = self.icon {
                ui.painter().text(
                    Pos2::new(x + size.icon_size() / 2.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    icon,
                    icon_font(ui, size.icon_size()),
                    c.fg,
                );
                x += icon_w;
            }
            let text_pos = Pos2::new(x, rect.center().y - galley.size().y / 2.0);
            ui.painter().galley(text_pos, galley, c.fg);
            paint_focus_ring(ui, &response, rect, radius);
        }
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        match self.tooltip {
            Some(t) => response.on_hover_text(t),
            None => response,
        }
    }
}

/// Shape of the [`EStopButton`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EStopStyle {
    /// A wide red badge with an octagon mark and a label: for toolbars.
    Badge,
    /// A stop-sign octagon of the given diameter (points): for control panels.
    Octagon(f32),
}

/// The emergency-stop button: unmistakably red, octagon-marked.
///
/// The caller sends M112 when it is clicked; the button only reports the click.
///
/// ```no_run
/// # use sprint_gui::widgets::{EStopButton, EStopStyle};
/// # fn demo(ui: &mut egui::Ui) {
/// if ui.add(EStopButton::new()).clicked() { /* printer.emergency_stop() */ }
/// ui.add(EStopButton::new().style(EStopStyle::Octagon(72.0)));
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct EStopButton<'a> {
    label: &'a str,
    tooltip: WidgetText,
    style: EStopStyle,
}

impl Default for EStopButton<'_> {
    fn default() -> Self {
        EStopButton::new()
    }
}

impl<'a> EStopButton<'a> {
    /// A badge labelled "E-STOP".
    pub fn new() -> Self {
        EStopButton {
            label: "E-STOP",
            tooltip: "Emergency stop: halts the printer immediately (M112)".into(),
            style: EStopStyle::Badge,
        }
    }

    /// Replace the label (keep it short and uppercase).
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    /// Replace the tooltip.
    pub fn tooltip(mut self, text: impl Into<WidgetText>) -> Self {
        self.tooltip = text.into();
        self
    }

    /// Badge or octagon.
    pub fn style(mut self, style: EStopStyle) -> Self {
        self.style = style;
        self
    }
}

/// Points of a regular octagon with flat top/bottom edges.
pub(crate) fn octagon_points(center: Pos2, radius: f32) -> Vec<Pos2> {
    (0..8)
        .map(|i| {
            let a = std::f32::consts::FRAC_PI_8 + i as f32 * std::f32::consts::FRAC_PI_4;
            center + radius * Vec2::angled(a)
        })
        .collect()
}

fn estop_fill(p: &Palette, hover: f32, pressed: bool) -> Color32 {
    // Always a saturated red, in both themes.
    let base = if p.dark {
        theme::hex(0xE5383B)
    } else {
        theme::hex(0xD92F35)
    };
    if pressed {
        mix(base, Color32::BLACK, 0.18)
    } else {
        mix(base, theme::hex(0xFF4D4F), hover)
    }
}

impl Widget for EStopButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let strong = theme::font(ui, TextRole::Title).family;
        match self.style {
            EStopStyle::Badge => {
                let font = FontId::new(14.0, strong);
                let galley =
                    ui.painter()
                        .layout_no_wrap(self.label.to_owned(), font, Color32::WHITE);
                let height = 34.0;
                let mark = 20.0;
                let width = 12.0 + mark + 8.0 + galley.size().x + 14.0;
                let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
                response.widget_info(|| {
                    WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), self.label)
                });
                if ui.is_rect_visible(rect) {
                    let p = theme::ui_palette(ui);
                    let hover = hover_t(ui, &response);
                    let pressed = response.is_pointer_button_down_on();
                    let fill = estop_fill(&p, hover, pressed);
                    let radius = CornerRadius::same(RADIUS_LG);
                    let painter = ui.painter();
                    if hover > 0.0 {
                        painter.rect_stroke(
                            rect.expand(2.0),
                            CornerRadius::same(RADIUS_LG + 2),
                            Stroke::new(3.0, with_alpha(fill, 0.35 * hover)),
                            StrokeKind::Outside,
                        );
                    }
                    painter.rect(
                        rect,
                        radius,
                        fill,
                        Stroke::new(1.0, mix(fill, Color32::BLACK, 0.25)),
                        StrokeKind::Inside,
                    );
                    // Subtle top highlight for a tactile look.
                    let top = Rect::from_min_max(
                        rect.min + vec2(1.0, 1.0),
                        Pos2::new(rect.max.x - 1.0, rect.center().y),
                    );
                    painter.rect_filled(
                        top,
                        CornerRadius {
                            nw: RADIUS_LG - 1,
                            ne: RADIUS_LG - 1,
                            sw: 0,
                            se: 0,
                        },
                        Color32::from_white_alpha(14),
                    );
                    let mark_center = Pos2::new(rect.min.x + 12.0 + mark / 2.0, rect.center().y);
                    painter.add(PathShape::closed_line(
                        octagon_points(mark_center, mark / 2.0),
                        Stroke::new(1.6, Color32::WHITE),
                    ));
                    painter.text(
                        mark_center,
                        Align2::CENTER_CENTER,
                        crate::icons::ESTOP,
                        icon_font(ui, 11.0),
                        Color32::WHITE,
                    );
                    let text_pos = Pos2::new(
                        mark_center.x + mark / 2.0 + 8.0,
                        rect.center().y - galley.size().y / 2.0,
                    );
                    painter.galley(text_pos, galley, Color32::WHITE);
                    paint_focus_ring(ui, &response, rect, radius);
                }
                response
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text(self.tooltip)
            }
            EStopStyle::Octagon(diameter) => {
                let diameter = if diameter.is_finite() {
                    diameter.clamp(32.0, 400.0)
                } else {
                    72.0
                };
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
                response.widget_info(|| {
                    WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), self.label)
                });
                if ui.is_rect_visible(rect) {
                    let p = theme::ui_palette(ui);
                    let hover = hover_t(ui, &response);
                    let pressed = response.is_pointer_button_down_on();
                    let fill = estop_fill(&p, hover, pressed);
                    let painter = ui.painter();
                    let c = rect.center();
                    // The octagon's flat-to-flat size equals the diameter.
                    let r = diameter / 2.0 / std::f32::consts::FRAC_PI_8.cos();
                    let r = r.min(diameter / 2.0 * 1.04);
                    if hover > 0.0 {
                        painter.add(PathShape::closed_line(
                            octagon_points(c, r + 2.5),
                            Stroke::new(3.0, with_alpha(fill, 0.35 * hover)),
                        ));
                    }
                    painter.add(Shape::convex_polygon(
                        octagon_points(c, r - 1.0),
                        fill,
                        Stroke::new(1.0, mix(fill, Color32::BLACK, 0.25)),
                    ));
                    painter.add(PathShape::closed_line(
                        octagon_points(c, r * 0.86),
                        Stroke::new((diameter / 36.0).clamp(1.5, 4.0), Color32::WHITE),
                    ));
                    let font = FontId::new((diameter * 0.22).round(), strong);
                    painter.text(c, Align2::CENTER_CENTER, self.label, font, Color32::WHITE);
                }
                response
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text(self.tooltip)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_and_danger_are_filled() {
        let p = Palette::dark();
        let c = button_colors(&p, ButtonKind::Primary, 0.0, false, false);
        assert_eq!(c.fill, p.accent);
        assert_eq!(c.fg, p.on_accent);
        let c = button_colors(&p, ButtonKind::Danger, 1.0, false, false);
        assert_eq!(c.fill, p.danger_hover);
        let c = button_colors(&p, ButtonKind::Danger, 0.0, true, false);
        assert_eq!(c.fill, p.danger_pressed);
    }

    #[test]
    fn ghost_is_transparent_until_hovered() {
        let p = Palette::light();
        let c = button_colors(&p, ButtonKind::Ghost, 0.0, false, false);
        assert_eq!(c.fill, Color32::TRANSPARENT);
        assert_eq!(c.fg, p.text_dim);
        let c = button_colors(&p, ButtonKind::Ghost, 1.0, false, false);
        assert_eq!(c.fill, p.raised);
        assert_eq!(c.fg, p.text);
        let c = button_colors(&p, ButtonKind::Ghost, 0.0, false, true);
        assert_eq!(c.fg, p.accent);
    }

    #[test]
    fn octagon_has_flat_top() {
        let pts = octagon_points(Pos2::ZERO, 10.0);
        assert_eq!(pts.len(), 8);
        // Two vertices share the topmost y (a flat edge), mirrored in x.
        let mut ys: Vec<f32> = pts.iter().map(|p| p.y).collect();
        ys.sort_by(f32::total_cmp);
        assert!((ys[0] - ys[1]).abs() < 1e-4);
        for p in &pts {
            assert!((p.to_vec2().length() - 10.0).abs() < 1e-4);
        }
    }

    #[test]
    fn sizes_grow() {
        assert!(ButtonSize::Small.height() < ButtonSize::Regular.height());
        assert!(ButtonSize::Regular.height() < ButtonSize::Large.height());
        assert!(ButtonSize::Small.icon_size() < ButtonSize::Large.icon_size());
    }
}
