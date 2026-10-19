//! Progress indicators: the circular [`ProgressRing`] and the slim
//! [`LinearProgress`] bar.

use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, Widget,
    WidgetInfo, WidgetType, epaint::PathShape, pos2, vec2,
};

use crate::theme::{self, TextRole, Tone, with_alpha};

/// Clamp a progress fraction to `0..=1`; `None` for NaN.
pub(crate) fn clamp_fraction(fraction: f32) -> Option<f32> {
    if fraction.is_nan() {
        None
    } else {
        Some(fraction.clamp(0.0, 1.0))
    }
}

/// "42%" for a fraction, "—" for NaN. Rounds down so 99.9 % never shows
/// as "100%" before the job is really done.
pub fn percent_label(fraction: f32) -> String {
    match clamp_fraction(fraction) {
        Some(f) => format!("{}%", (f * 100.0 + 1e-4).floor() as u32),
        None => "—".to_owned(),
    }
}

/// Points along a circular arc, clockwise from 12 o'clock, covering
/// `fraction` of the circle.
fn arc_points(center: Pos2, radius: f32, fraction: f32) -> Vec<Pos2> {
    let segments = ((fraction * 96.0).ceil() as usize).max(2);
    let start = -std::f32::consts::FRAC_PI_2;
    let sweep = fraction * std::f32::consts::TAU;
    (0..=segments)
        .map(|i| {
            let a = start + sweep * i as f32 / segments as f32;
            center + radius * Vec2::angled(a)
        })
        .collect()
}

/// Circular progress with a centered label, e.g. job progress.
///
/// ```no_run
/// # use sprint_gui::widgets::ProgressRing;
/// # fn demo(ui: &mut egui::Ui) {
/// ui.add(ProgressRing::new(0.42).size(96.0).sublabel("1:23 left"));
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct ProgressRing<'a> {
    fraction: f32,
    size: f32,
    thickness: Option<f32>,
    tone: Tone,
    color: Option<Color32>,
    text: Option<String>,
    sublabel: Option<&'a str>,
    animate: bool,
}

impl<'a> ProgressRing<'a> {
    /// A 96 pt ring at `fraction` (0..=1, clamped; NaN shows "—").
    pub fn new(fraction: f32) -> Self {
        ProgressRing {
            fraction,
            size: 96.0,
            thickness: None,
            tone: Tone::Accent,
            color: None,
            text: None,
            sublabel: None,
            animate: true,
        }
    }

    /// Diameter in points (24..=400).
    pub fn size(mut self, size: f32) -> Self {
        if size.is_finite() {
            self.size = size.clamp(24.0, 400.0);
        }
        self
    }

    /// Ring thickness in points (default: 8 % of the size).
    pub fn thickness(mut self, thickness: f32) -> Self {
        if thickness.is_finite() && thickness > 0.0 {
            self.thickness = Some(thickness);
        }
        self
    }

    /// Color by intent (default accent; e.g. success when done).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Explicit arc color (overrides the tone).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Center text (default: the percentage).
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Small dim line under the center text.
    pub fn sublabel(mut self, sublabel: &'a str) -> Self {
        self.sublabel = Some(sublabel);
        self
    }

    /// Ease towards new values (default `true`).
    pub fn animate(mut self, animate: bool) -> Self {
        self.animate = animate;
        self
    }
}

impl Widget for ProgressRing<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::hover());
        let label = self
            .text
            .clone()
            .unwrap_or_else(|| percent_label(self.fraction));
        response.widget_info(|| WidgetInfo::labeled(WidgetType::ProgressIndicator, true, &label));
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let p = theme::ui_palette(ui);
        let color = self.color.unwrap_or_else(|| p.tone(self.tone));
        let thickness = self
            .thickness
            .unwrap_or(self.size * 0.08)
            .clamp(2.0, self.size / 4.0);
        let radius = self.size / 2.0 - thickness / 2.0 - 1.0;
        let center = rect.center();
        let painter = ui.painter();
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(thickness, if p.dark { p.raised } else { p.raised_hover }),
        );
        let target = clamp_fraction(self.fraction).unwrap_or(0.0);
        let shown = if self.animate {
            ui.ctx()
                .animate_value_with_time(response.id.with("ring"), target, 0.3)
        } else {
            target
        };
        if shown > 0.001 {
            let pts = arc_points(center, radius, shown);
            // Round caps.
            if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
                painter.circle_filled(*first, thickness / 2.0, color);
                painter.circle_filled(*last, thickness / 2.0, color);
            }
            painter.add(PathShape::line(pts, Stroke::new(thickness, color)));
        }
        // The label must fit inside the ring: shrink long custom texts.
        let family = theme::font(ui, TextRole::Display).family;
        let max_w = (radius - thickness) * 1.5;
        let mut main_size = (self.size * 0.24).round().max(10.0);
        let mut main = painter.layout_no_wrap(
            label.clone(),
            FontId::new(main_size, family.clone()),
            p.text,
        );
        if main.size().x > max_w && main.size().x > 0.0 {
            main_size = (main_size * max_w / main.size().x).floor().max(9.0);
            main = painter.layout_no_wrap(label, FontId::new(main_size, family), p.text);
        }
        let sub = self.sublabel.map(|s| {
            painter.layout_no_wrap(
                s.to_owned(),
                FontId::proportional((self.size * 0.12).clamp(10.0, 14.0)),
                p.text_dim,
            )
        });
        let total_h = main.size().y + sub.as_ref().map_or(0.0, |s| s.size().y);
        let mut y = center.y - total_h / 2.0;
        painter.galley(
            pos2(center.x - main.size().x / 2.0, y),
            main.clone(),
            p.text,
        );
        y += main.size().y;
        if let Some(sub) = sub {
            let max_w = radius * 1.6;
            if sub.size().x <= max_w {
                painter.galley(pos2(center.x - sub.size().x / 2.0, y), sub, p.text_dim);
            }
        }
        response
    }
}

/// A slim linear progress bar with an optional label row above it.
/// `None` progress is indeterminate (an animated sliding segment).
///
/// ```no_run
/// # use sprint_gui::widgets::LinearProgress;
/// # fn demo(ui: &mut egui::Ui) {
/// ui.add(LinearProgress::new(0.42).label("Uploading to SD").show_percentage(true));
/// ui.add(LinearProgress::indeterminate().label("Analyzing…"));
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct LinearProgress<'a> {
    fraction: Option<f32>,
    height: f32,
    width: Option<f32>,
    tone: Tone,
    color: Option<Color32>,
    label: Option<&'a str>,
    show_percentage: bool,
}

impl<'a> LinearProgress<'a> {
    /// A bar at `fraction` (0..=1, clamped; NaN is drawn empty).
    pub fn new(fraction: f32) -> Self {
        LinearProgress {
            fraction: Some(fraction),
            height: 6.0,
            width: None,
            tone: Tone::Accent,
            color: None,
            label: None,
            show_percentage: false,
        }
    }

    /// An indeterminate bar (work of unknown length).
    pub fn indeterminate() -> Self {
        LinearProgress {
            fraction: None,
            ..LinearProgress::new(0.0)
        }
    }

    /// Bar thickness in points (2..=24, default 6).
    pub fn height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.height = height.clamp(2.0, 24.0);
        }
        self
    }

    /// Fixed width (default: the available width).
    pub fn width(mut self, width: f32) -> Self {
        if let Some(width) = super::finite_clamped(width, 8.0, super::MAX_EXTENT) {
            self.width = Some(width);
        }
        self
    }

    /// Color by intent (default accent).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Explicit fill color (overrides the tone).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Caption on the left above the bar.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    /// Show the percentage on the right above the bar.
    pub fn show_percentage(mut self, show: bool) -> Self {
        self.show_percentage = show;
        self
    }
}

impl Widget for LinearProgress<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = theme::ui_palette(ui);
        let width = self.width.unwrap_or_else(|| ui.available_width().max(24.0));
        let font = FontId::proportional(12.0);
        let caption_h = if self.label.is_some() || self.show_percentage {
            ui.ctx().fonts_mut(|f| f.row_height(&font)) + 4.0
        } else {
            0.0
        };
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, caption_h + self.height), Sense::hover());
        let text = self.label.unwrap_or("progress");
        response.widget_info(|| WidgetInfo::labeled(WidgetType::ProgressIndicator, true, text));
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let painter = ui.painter();
        if let Some(label) = self.label {
            painter.text(rect.min, Align2::LEFT_TOP, label, font.clone(), p.text_dim);
        }
        if self.show_percentage {
            let pct = self.fraction.map_or_else(|| "—".to_owned(), percent_label);
            painter.text(
                pos2(rect.max.x, rect.min.y),
                Align2::RIGHT_TOP,
                pct,
                font,
                p.text,
            );
        }
        let bar = Rect::from_min_max(pos2(rect.min.x, rect.max.y - self.height), rect.max);
        let radius = CornerRadius::same(255);
        let color = self.color.unwrap_or_else(|| p.tone(self.tone));
        painter.rect_filled(bar, radius, if p.dark { p.raised } else { p.raised_hover });
        match self.fraction {
            Some(fraction) => {
                let target = clamp_fraction(fraction).unwrap_or(0.0);
                let shown = ui
                    .ctx()
                    .animate_value_with_time(response.id.with("bar"), target, 0.3);
                if shown > 0.0 {
                    let w = (bar.width() * shown).max(self.height);
                    let fill = Rect::from_min_size(bar.min, vec2(w, bar.height()));
                    painter.rect_filled(fill, radius, color);
                }
            }
            None => {
                let t = ui.input(|i| i.time) as f32;
                let phase = (t * 0.8).fract();
                let seg_w = bar.width() * 0.3;
                let x0 = bar.min.x - seg_w + (bar.width() + seg_w) * phase;
                let seg = Rect::from_min_max(
                    pos2(x0.max(bar.min.x), bar.min.y),
                    pos2((x0 + seg_w).min(bar.max.x), bar.max.y),
                );
                if seg.width() > 0.0 {
                    painter.rect_filled(seg, radius, with_alpha(color, 0.9));
                }
                ui.ctx().request_repaint();
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_label_rounds_down_and_handles_nan() {
        assert_eq!(percent_label(0.0), "0%");
        assert_eq!(percent_label(0.425), "42%");
        assert_eq!(percent_label(0.999), "99%");
        assert_eq!(percent_label(1.0), "100%");
        assert_eq!(percent_label(0.29), "29%");
        assert_eq!(percent_label(7.0), "100%");
        assert_eq!(percent_label(-1.0), "0%");
        assert_eq!(percent_label(f32::NAN), "—");
        assert_eq!(percent_label(f32::INFINITY), "100%");
    }

    #[test]
    fn arc_starts_at_twelve_and_runs_clockwise() {
        let pts = arc_points(Pos2::ZERO, 10.0, 0.25);
        let first = pts[0];
        let last = *pts.last().expect("points");
        assert!((first.x).abs() < 1e-4 && (first.y + 10.0).abs() < 1e-4);
        // A quarter turn clockwise (screen y down) ends at 3 o'clock.
        assert!((last.x - 10.0).abs() < 1e-3 && last.y.abs() < 1e-3);
        assert!(arc_points(Pos2::ZERO, 10.0, 0.0).len() >= 2);
    }
}
