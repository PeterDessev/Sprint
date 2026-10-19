//! Binary and one-of-many selectors: toggle switch and segmented control.

use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    Ui, Widget, WidgetInfo, WidgetText, WidgetType, vec2,
};

use crate::theme::{self, RADIUS, mix, with_alpha};

use super::{hover_t, paint_focus_ring};

/// An on/off switch with an optional label on the right.
///
/// ```no_run
/// # use sprint_gui::widgets::Toggle;
/// # fn demo(ui: &mut egui::Ui, autoscroll: &mut bool) {
/// if ui.add(Toggle::new(autoscroll).label("Autoscroll")).changed() { /* ... */ }
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct Toggle<'a> {
    on: &'a mut bool,
    label: Option<WidgetText>,
}

impl<'a> Toggle<'a> {
    /// A switch bound to `on`.
    pub fn new(on: &'a mut bool) -> Self {
        Toggle { on, label: None }
    }

    /// Text to the right of the switch (also clickable).
    pub fn label(mut self, label: impl Into<WidgetText>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Widget for Toggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let track = vec2(34.0, 20.0);
        let galley = self.label.map(|label| {
            label.into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Body,
            )
        });
        let gap = 8.0;
        let width = track.x + galley.as_ref().map_or(0.0, |g| gap + g.size().x);
        let height = track
            .y
            .max(galley.as_ref().map_or(0.0, |g| g.size().y))
            .max(24.0);
        let (rect, mut response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        let on = *self.on;
        let label_text = galley
            .as_ref()
            .map_or(String::new(), |g| g.text().to_owned());
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::Checkbox,
                ui.is_enabled(),
                on,
                label_text.clone(),
            )
        });
        if ui.is_rect_visible(rect) {
            let p = theme::ui_palette(ui);
            let t = ui
                .ctx()
                .animate_bool_with_time(response.id.with("knob"), on, 0.12);
            let hover = hover_t(ui, &response);
            let track_rect = Rect::from_center_size(
                Pos2::new(rect.min.x + track.x / 2.0, rect.center().y),
                track,
            );
            let off_fill = mix(p.raised_hover, p.border_strong, hover * 0.5);
            let on_fill = mix(p.accent, p.accent_hover, hover);
            let fill = mix(off_fill, on_fill, t);
            let radius = CornerRadius::same(255);
            ui.painter().rect(
                track_rect,
                radius,
                fill,
                Stroke::new(1.0, with_alpha(p.border_strong, 1.0 - t)),
                StrokeKind::Inside,
            );
            let knob_r = track.y / 2.0 - 3.0;
            let x = egui::lerp(
                (track_rect.min.x + 3.0 + knob_r)..=(track_rect.max.x - 3.0 - knob_r),
                t,
            );
            let knob_center = Pos2::new(x, track_rect.center().y);
            ui.painter().circle_filled(
                knob_center + vec2(0.0, 1.0),
                knob_r,
                Color32::from_black_alpha(40),
            );
            let knob = if p.dark {
                mix(theme::hex(0xD5D9DE), Color32::WHITE, t)
            } else {
                Color32::WHITE
            };
            ui.painter().circle_filled(knob_center, knob_r, knob);
            paint_focus_ring(ui, &response, track_rect, radius);
            if let Some(galley) = galley {
                let pos = Pos2::new(
                    track_rect.max.x + gap,
                    rect.center().y - galley.size().y / 2.0,
                );
                ui.painter().galley(pos, galley, p.text);
            }
        }
        response.on_hover_cursor(CursorIcon::PointingHand)
    }
}

/// A row of mutually exclusive segments with a sliding highlight.
///
/// ```no_run
/// # use sprint_gui::widgets::SegmentedControl;
/// # #[derive(Clone, PartialEq)] enum View { Iso, Top }
/// # fn demo(ui: &mut egui::Ui, view: &mut View) {
/// ui.add(SegmentedControl::new(view, &[(View::Iso, "3D"), (View::Top, "Layer")]));
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct SegmentedControl<'a, T, S> {
    value: &'a mut T,
    options: &'a [(T, S)],
    fill_width: bool,
    segment_width: Option<f32>,
    id_salt: Option<Id>,
}

impl<'a, T: PartialEq + Clone, S: AsRef<str>> SegmentedControl<'a, T, S> {
    /// A control bound to `value` with `(value, label)` options.
    pub fn new(value: &'a mut T, options: &'a [(T, S)]) -> Self {
        SegmentedControl {
            value,
            options,
            fill_width: false,
            segment_width: None,
            id_salt: None,
        }
    }

    /// Stretch to the available width, segments sharing it equally.
    pub fn fill_width(mut self, fill: bool) -> Self {
        self.fill_width = fill;
        self
    }

    /// Fixed width of every segment.
    pub fn segment_width(mut self, width: f32) -> Self {
        if width.is_finite() && width > 0.0 {
            self.segment_width = Some(width);
        }
        self
    }

    /// Id salt (needed when two controls share options).
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(Id::new(salt));
        self
    }
}

impl<T: PartialEq + Clone, S: AsRef<str>> Widget for SegmentedControl<'_, T, S> {
    fn ui(self, ui: &mut Ui) -> Response {
        let font = FontId::proportional(13.0);
        let height = 28.0;
        let pad = 3.0;
        let galleys: Vec<_> = self
            .options
            .iter()
            .map(|(_, label)| {
                ui.painter().layout_no_wrap(
                    label.as_ref().to_owned(),
                    font.clone(),
                    Color32::PLACEHOLDER,
                )
            })
            .collect();
        let n = galleys.len().max(1);
        let natural = galleys
            .iter()
            .map(|g| g.size().x + 24.0)
            .fold(0.0_f32, f32::max);
        let mut seg_w = self.segment_width.unwrap_or(natural.max(36.0));
        if self.fill_width {
            seg_w = seg_w.max((ui.available_width() - 2.0 * pad) / n as f32);
        }
        let width = seg_w * n as f32 + 2.0 * pad;
        let id = self
            .id_salt
            .map_or_else(|| ui.next_auto_id(), |salt| ui.make_persistent_id(salt));
        let (rect, mut response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let p = theme::ui_palette(ui);
        let selected = self.options.iter().position(|(v, _)| *v == *self.value);

        let seg_rect = |i: usize| {
            Rect::from_min_size(
                Pos2::new(rect.min.x + pad + seg_w * i as f32, rect.min.y + pad),
                vec2(seg_w, height - 2.0 * pad),
            )
        };
        let radius = CornerRadius::same(RADIUS);
        let inner_radius = CornerRadius::same(RADIUS - 2);
        if ui.is_rect_visible(rect) {
            ui.painter().rect(
                rect,
                radius,
                if p.dark { p.input } else { p.raised_hover },
                Stroke::new(1.0, p.border),
                StrokeKind::Inside,
            );
        }
        // Sliding highlight behind the selected segment.
        if let Some(sel) = selected {
            let x = ui
                .ctx()
                .animate_value_with_time(id.with("slide"), sel as f32, 0.12);
            let highlight = Rect::from_min_size(
                Pos2::new(rect.min.x + pad + seg_w * x, rect.min.y + pad),
                vec2(seg_w, height - 2.0 * pad),
            );
            let fill = if p.dark { p.raised_hover } else { p.surface };
            if !p.dark {
                ui.painter().rect_filled(
                    highlight.translate(vec2(0.0, 1.0)),
                    inner_radius,
                    Color32::from_black_alpha(18),
                );
            }
            ui.painter().rect(
                highlight,
                inner_radius,
                fill,
                Stroke::new(1.0, if p.dark { p.border_strong } else { p.border }),
                StrokeKind::Inside,
            );
        }
        for (i, ((value, label), galley)) in self.options.iter().zip(galleys).enumerate() {
            let r = seg_rect(i);
            let seg = ui.interact(r, id.with(i), Sense::click());
            let is_sel = selected == Some(i);
            seg.widget_info(|| {
                WidgetInfo::selected(
                    WidgetType::RadioButton,
                    ui.is_enabled(),
                    is_sel,
                    label.as_ref(),
                )
            });
            if seg.clicked() && !is_sel {
                *self.value = value.clone();
                response.mark_changed();
            }
            let hover = hover_t(ui, &seg);
            if !is_sel && hover > 0.0 {
                ui.painter().rect_filled(
                    r.shrink(1.0),
                    inner_radius,
                    with_alpha(p.raised_hover, 0.6 * hover),
                );
            }
            let color = if is_sel {
                p.text
            } else {
                mix(p.text_dim, p.text, hover)
            };
            ui.painter()
                .galley(r.center() - galley.size() / 2.0, galley, color);
            paint_focus_ring(ui, &seg, r, inner_radius);
            let _ = seg.on_hover_cursor(CursorIcon::PointingHand);
        }
        response
    }
}
