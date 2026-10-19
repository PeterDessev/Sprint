//! [`NumericField`]: a labelled number input with unit, range, decimals and
//! a "modified" indicator against an original value.

use std::ops::RangeInclusive;

use egui::{
    Align2, CornerRadius, DragValue, FontId, Pos2, Response, Sense, Stroke, Ui, Widget, WidgetInfo,
    WidgetType, emath::Numeric, vec2,
};

use crate::icons;
use crate::theme::{self, Palette, with_alpha};

use super::IconButton;

/// Whether `value` differs from `original` when both are shown with
/// `decimals` decimals. Two NaNs are equal; NaN and a number differ.
pub fn is_modified(value: f64, original: f64, decimals: usize) -> bool {
    match (value.is_nan(), original.is_nan()) {
        (true, true) => false,
        (true, false) | (false, true) => true,
        (false, false) => {
            if value.is_infinite() || original.is_infinite() {
                return value != original;
            }
            let tolerance = 0.5 * 10f64.powi(-(decimals.min(12) as i32));
            (value - original).abs() >= tolerance
        }
    }
}

/// `value` formatted with `decimals` decimals (no `-0`), or "—" when it is
/// not finite.
pub fn format_number(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }
    let text = format!("{value:.*}", decimals.min(12));
    if text
        .trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.')
    {
        text.trim_start_matches('-').to_owned()
    } else {
        text
    }
}

/// A labelled number input: label, a drag-or-type value with its unit, and
/// — when an original value is given — a dot and a reset button once the
/// value differs from it. Out-of-range values (e.g. reported by the
/// firmware) are kept, not clamped, and outlined as a warning.
///
/// ```no_run
/// # use sprint_gui::widgets::NumericField;
/// # fn demo(ui: &mut egui::Ui, steps: &mut f64, reported: f64) {
/// let r = ui.add(
///     NumericField::new(steps)
///         .label("X steps")
///         .unit("steps/mm")
///         .range(1.0..=10_000.0)
///         .decimals(2)
///         .original(reported),
/// );
/// if r.changed() { /* mark the setting dirty */ }
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct NumericField<'a, N: Numeric> {
    value: &'a mut N,
    label: Option<&'a str>,
    hint: Option<&'a str>,
    unit: &'a str,
    range: Option<RangeInclusive<N>>,
    decimals: usize,
    speed: Option<f64>,
    original: Option<N>,
    label_width: Option<f32>,
    field_width: f32,
}

impl<'a, N: Numeric> NumericField<'a, N> {
    /// A field bound to `value`, 2 decimals, no label.
    pub fn new(value: &'a mut N) -> Self {
        NumericField {
            value,
            label: None,
            hint: None,
            unit: "",
            range: None,
            decimals: if N::INTEGRAL { 0 } else { 2 },
            speed: None,
            original: None,
            label_width: None,
            field_width: 110.0,
        }
    }

    /// Label on the left.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    /// Tooltip on the label (e.g. what the setting does).
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }

    /// Unit shown after the number ("mm/s", "°C", ...).
    pub fn unit(mut self, unit: &'a str) -> Self {
        self.unit = unit;
        self
    }

    /// Allowed range for user edits (an inverted range is ignored).
    pub fn range(mut self, range: RangeInclusive<N>) -> Self {
        let (lo, hi) = (range.start().to_f64(), range.end().to_f64());
        if !lo.is_nan() && !hi.is_nan() && lo <= hi {
            self.range = Some(range);
        }
        self
    }

    /// Decimals shown and rounded to (0..=8).
    pub fn decimals(mut self, decimals: usize) -> Self {
        self.decimals = decimals.min(8);
        self
    }

    /// Drag speed per point (default: one step of the last decimal, at
    /// least 1/500 of the range).
    pub fn speed(mut self, speed: f64) -> Self {
        if speed.is_finite() && speed > 0.0 {
            self.speed = Some(speed);
        }
        self
    }

    /// The value to compare against (e.g. what the printer reported);
    /// enables the modified dot and the reset button.
    pub fn original(mut self, original: N) -> Self {
        self.original = Some(original);
        self
    }

    /// Like [`original`](Self::original) with an optional value.
    pub fn original_opt(mut self, original: Option<N>) -> Self {
        self.original = original;
        self
    }

    /// Fixed label column width, to align a stack of fields.
    pub fn label_width(mut self, width: f32) -> Self {
        if width.is_finite() && width >= 0.0 {
            self.label_width = Some(width);
        }
        self
    }

    /// Width of the value box (default 110).
    pub fn field_width(mut self, width: f32) -> Self {
        if width.is_finite() && width > 24.0 {
            self.field_width = width;
        }
        self
    }

    fn is_modified(&self) -> bool {
        self.original
            .is_some_and(|o| is_modified(self.value.to_f64(), o.to_f64(), self.decimals))
    }

    fn out_of_range(&self) -> bool {
        let v = self.value.to_f64();
        self.range
            .as_ref()
            .is_some_and(|r| v.is_finite() && (v < r.start().to_f64() || v > r.end().to_f64()))
    }
}

impl<'a> NumericField<'a, f64> {
    /// A field configured from a firmware setting parameter of the
    /// settings catalog: label, unit, range and decimals.
    pub fn from_param(param: &sprint_proto::ParamDef, value: &'a mut f64) -> Self {
        let field = NumericField::new(value)
            .label(param.label)
            .unit(param.unit)
            .decimals(usize::from(param.decimals));
        if param.min.is_finite() && param.max.is_finite() {
            field.range(param.min..=param.max)
        } else {
            field
        }
    }
}

impl<N: Numeric> Widget for NumericField<'_, N> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = theme::ui_palette(ui);
        let modified = self.is_modified();
        let out_of_range = self.out_of_range();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if let Some(label) = self.label {
                let r = label_cell(ui, &p, label, self.label_width, modified);
                if let Some(hint) = self.hint {
                    r.on_hover_text(hint);
                }
            }
            let mut response = if self.value.to_f64().is_finite() {
                ui.scope(|ui| {
                    style_input(ui, &p, modified, out_of_range);
                    ui.spacing_mut().interact_size = vec2(self.field_width, 26.0);
                    let speed = self.speed.unwrap_or_else(|| default_speed(&self));
                    let mut drag = DragValue::new(self.value)
                        .speed(speed)
                        .fixed_decimals(self.decimals)
                        .clamp_existing_to_range(false);
                    if !self.unit.is_empty() {
                        drag = drag.suffix(format!(" {}", self.unit));
                    }
                    if let Some(range) = self.range.clone() {
                        drag = drag.range(range);
                    }
                    ui.add(drag)
                })
                .inner
            } else {
                missing_value(ui, &p, self.label.unwrap_or("Value"), self.field_width)
            };
            if out_of_range && let Some(r) = &self.range {
                response = response.on_hover_text(format!(
                    "Outside the usual range {} – {} {}",
                    format_number(r.start().to_f64(), self.decimals),
                    format_number(r.end().to_f64(), self.decimals),
                    self.unit
                ));
            }
            if let Some(original) = self.original {
                let reset_size = super::ButtonSize::Small.height();
                if modified {
                    let tip = format!(
                        "Reset to {}{}{}",
                        format_number(original.to_f64(), self.decimals),
                        if self.unit.is_empty() { "" } else { " " },
                        self.unit
                    );
                    let reset = ui.add(IconButton::new(icons::RESET).ghost().small().tooltip(tip));
                    if reset.clicked() {
                        *self.value = original;
                        response.mark_changed();
                    }
                } else {
                    // Keep the row width stable when the button is hidden.
                    ui.allocate_exact_size(vec2(reset_size, reset_size), Sense::hover());
                }
            }
            if !self.value.to_f64().is_finite()
                && response.clicked()
                && let Some(fallback) = fallback_value(&self)
            {
                *self.value = fallback;
                response.mark_changed();
            }
            response
        })
        .inner
    }
}

/// The default drag speed: one unit of the last decimal, but fast enough to
/// cross the range in ~500 points.
fn default_speed<N: Numeric>(field: &NumericField<'_, N>) -> f64 {
    let step = 10f64.powi(-(field.decimals as i32));
    let span = field.range.as_ref().map_or(0.0, |r| {
        let s = r.end().to_f64() - r.start().to_f64();
        if s.is_finite() { s } else { 0.0 }
    });
    step.max(span / 500.0)
}

/// What a click on a missing (non-finite) value sets: the original, else
/// zero clamped into the range.
fn fallback_value<N: Numeric>(field: &NumericField<'_, N>) -> Option<N> {
    if let Some(o) = field.original
        && o.to_f64().is_finite()
    {
        return Some(o);
    }
    let zero = match &field.range {
        Some(r) => 0.0_f64.clamp(r.start().to_f64(), r.end().to_f64()),
        None => 0.0,
    };
    zero.is_finite().then(|| N::from_f64(zero))
}

/// The label column with the modified dot.
fn label_cell(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    width: Option<f32>,
    modified: bool,
) -> Response {
    let font = FontId::proportional(13.0);
    let color = if modified { p.text } else { p.text_dim };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let dot = 12.0;
    let natural = dot + galley.size().x;
    let width = width.unwrap_or(natural).max(dot);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        if modified {
            painter.circle_filled(Pos2::new(rect.min.x + 3.5, rect.center().y), 3.0, p.accent);
        }
        let pos = Pos2::new(rect.min.x + dot, rect.center().y - galley.size().y / 2.0);
        painter.galley(pos, galley, color);
    }
    response
}

/// Input styling for the DragValue in this scope.
fn style_input(ui: &mut Ui, p: &Palette, modified: bool, out_of_range: bool) {
    let border = if out_of_range {
        p.warning
    } else if modified {
        with_alpha(p.accent, 0.75)
    } else {
        p.border
    };
    let w = &mut ui.visuals_mut().widgets;
    for state in [&mut w.inactive, &mut w.hovered, &mut w.active] {
        state.weak_bg_fill = p.input;
        state.bg_fill = p.input;
        state.corner_radius = CornerRadius::same(theme::RADIUS);
    }
    w.inactive.bg_stroke = Stroke::new(1.0, border);
    w.hovered.bg_stroke = Stroke::new(
        1.0,
        if out_of_range || modified {
            border
        } else {
            p.border_strong
        },
    );
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.inactive.fg_stroke.color = p.text;
    w.hovered.fg_stroke.color = p.text;
}

/// A placeholder box for a missing value; click to fill it in.
fn missing_value(ui: &mut Ui, p: &Palette, label: &str, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click());
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            ui.is_enabled(),
            format!("{label}: no value"),
        )
    });
    if ui.is_rect_visible(rect) {
        ui.painter().rect(
            rect,
            CornerRadius::same(theme::RADIUS),
            p.input,
            Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "—",
            FontId::proportional(14.0),
            p.text_faint,
        );
    }
    response.on_hover_text("No value reported; click to enter one")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_respects_decimals() {
        assert!(!is_modified(80.0, 80.0, 2));
        assert!(!is_modified(80.004, 80.0, 2));
        assert!(is_modified(80.006, 80.0, 2));
        assert!(is_modified(81.0, 80.0, 0));
        assert!(!is_modified(80.4, 80.0, 0));
        assert!(!is_modified(f64::NAN, f64::NAN, 2));
        assert!(is_modified(f64::NAN, 1.0, 2));
        assert!(is_modified(1.0, f64::NAN, 2));
        assert!(!is_modified(f64::INFINITY, f64::INFINITY, 2));
        assert!(is_modified(f64::INFINITY, 1.0, 2));
        // Absurd decimals do not overflow.
        assert!(!is_modified(1.0, 1.0, usize::MAX));
    }

    #[test]
    fn format_number_handles_edge_cases() {
        assert_eq!(format_number(1.5, 2), "1.50");
        assert_eq!(format_number(-0.0001, 2), "0.00");
        assert_eq!(format_number(-0.5, 0), "0");
        assert_eq!(format_number(-1.5, 1), "-1.5");
        assert_eq!(format_number(f64::NAN, 2), "—");
        assert_eq!(format_number(f64::NEG_INFINITY, 2), "—");
        assert_eq!(format_number(3.0, usize::MAX), "3.000000000000");
    }

    #[test]
    fn default_speed_scales_with_range() {
        let mut v = 0.0_f64;
        let f = NumericField::new(&mut v).decimals(2);
        assert!((default_speed(&f) - 0.01).abs() < 1e-12);
        let mut v = 0.0_f64;
        let f = NumericField::new(&mut v).decimals(0).range(0.0..=10_000.0);
        assert!((default_speed(&f) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn inverted_or_nan_range_is_ignored() {
        let mut v = 5.0_f64;
        assert!(NumericField::new(&mut v).range(10.0..=0.0).range.is_none());
        let mut v = 5.0_f64;
        assert!(
            NumericField::new(&mut v)
                .range(f64::NAN..=1.0)
                .range
                .is_none()
        );
    }

    #[test]
    fn fallback_prefers_original_then_zero_in_range() {
        let mut v = f64::NAN;
        let f = NumericField::new(&mut v).original(42.0);
        assert_eq!(fallback_value(&f), Some(42.0));
        let mut v = f64::NAN;
        let f = NumericField::new(&mut v).range(5.0..=10.0);
        assert_eq!(fallback_value(&f), Some(5.0));
        let mut v = f64::NAN;
        let f = NumericField::new(&mut v).original(f64::NAN);
        assert_eq!(fallback_value(&f), Some(0.0));
    }

    #[test]
    fn from_param_copies_catalog_metadata() {
        let def = sprint_proto::ParamDef {
            letter: 'X',
            label: "X steps",
            unit: "steps/mm",
            min: 1.0,
            max: 10_000.0,
            decimals: 2,
            flag: false,
        };
        let mut v = 80.0;
        let f = NumericField::from_param(&def, &mut v);
        assert_eq!(f.label, Some("X steps"));
        assert_eq!(f.unit, "steps/mm");
        assert_eq!(f.decimals, 2);
        assert_eq!(f.range, Some(1.0..=10_000.0));
    }
}
