//! Small charts: the [`Sparkline`] and the smooth [`MiniChart`], plus the
//! geometry helpers behind them ([`smooth_points`], [`nice_ticks`]).
//!
//! Full-size, zoomable plots belong to `egui_plot`; these are for compact
//! read-only trends inside cards.

use egui::{
    Align2, Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Response, Sense, Shape, Stroke, Ui,
    Widget, WidgetInfo, WidgetType, epaint::PathShape, pos2, vec2,
};

use crate::theme::{self, Palette, with_alpha};

/// Monotone cubic interpolation (Fritsch–Carlson) through `points`, which
/// must be sorted by increasing `x` and finite (see [`finite_runs`]).
///
/// Returns the input points with `samples_per_segment - 1` extra points
/// between each pair. Unlike Catmull-Rom splines the curve never overshoots
/// the data, so a temperature that plateaus stays flat. Fewer than three
/// points, or `samples_per_segment < 2`, are returned unchanged.
pub fn smooth_points(points: &[Pos2], samples_per_segment: usize) -> Vec<Pos2> {
    let n = points.len();
    if n < 3 || samples_per_segment < 2 {
        return points.to_vec();
    }
    // Secant slopes.
    let mut delta = Vec::with_capacity(n - 1);
    for w in points.windows(2) {
        let dx = w[1].x - w[0].x;
        delta.push(if dx.abs() > f32::EPSILON {
            (w[1].y - w[0].y) / dx
        } else {
            0.0
        });
    }
    // Tangents: average of neighbouring secants, zero at extrema.
    let mut m = vec![0.0_f32; n];
    m[0] = delta[0];
    m[n - 1] = delta[n - 2];
    for i in 1..n - 1 {
        m[i] = if delta[i - 1] * delta[i] <= 0.0 {
            0.0
        } else {
            (delta[i - 1] + delta[i]) / 2.0
        };
    }
    // Fritsch–Carlson limiter keeps each segment monotone.
    for i in 0..n - 1 {
        if delta[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let a = m[i] / delta[i];
        let b = m[i + 1] / delta[i];
        let s = a * a + b * b;
        if s > 9.0 {
            let t = 3.0 / s.sqrt();
            m[i] = t * a * delta[i];
            m[i + 1] = t * b * delta[i];
        }
    }
    let mut out = Vec::with_capacity((n - 1) * samples_per_segment + 1);
    for i in 0..n - 1 {
        let (p0, p1) = (points[i], points[i + 1]);
        let h = p1.x - p0.x;
        for s in 0..samples_per_segment {
            let t = s as f32 / samples_per_segment as f32;
            let (t2, t3) = (t * t, t * t * t);
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            let y = h00 * p0.y + h10 * h * m[i] + h01 * p1.y + h11 * h * m[i + 1];
            out.push(pos2(p0.x + t * h, y));
        }
    }
    out.push(points[n - 1]);
    out
}

/// Split `values` into runs of consecutive finite values, as
/// `(start_index, run)` pairs. NaN and infinities end a run (a gap in the
/// line).
pub fn finite_runs(values: &[f32]) -> Vec<(usize, &[f32])> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, v) in values.iter().enumerate() {
        match (v.is_finite(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push((s, &values[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, &values[s..]));
    }
    runs
}

/// "Nice" tick values (1, 2, 2.5 or 5 times a power of ten) inside
/// `min..=max`, at most `max_ticks + 1` of them. Empty for non-finite
/// input or `max_ticks == 0`; a single tick for an empty range.
pub fn nice_ticks(min: f64, max: f64, max_ticks: usize) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || max_ticks == 0 {
        return Vec::new();
    }
    let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
    let span = hi - lo;
    if span <= f64::EPSILON * lo.abs().max(1.0) {
        return vec![lo];
    }
    let raw = span / max_ticks as f64;
    let magnitude = 10f64.powf(raw.log10().floor());
    let count = |step: f64| (hi / step).floor() - (lo / step).ceil() + 1.0;
    let step = [1.0, 2.0, 2.5, 5.0, 10.0, 20.0]
        .iter()
        .map(|f| f * magnitude)
        .find(|s| count(*s) <= max_ticks as f64 + 1.0)
        .unwrap_or(20.0 * magnitude);
    let first = (lo / step).ceil() * step;
    let snap = 10f64.powi(2 - magnitude.log10().floor() as i32);
    let mut ticks = Vec::new();
    let mut i = 0.0;
    loop {
        let t = first + i * step;
        if t > hi + step * 1e-9 || ticks.len() > 1000 {
            break;
        }
        // Snap away floating-point noise such as 0.30000000000000004.
        let v = (t * snap).round() / snap;
        ticks.push(if v == 0.0 { 0.0 } else { v });
        i += 1.0;
    }
    ticks
}

/// Min and max of the finite values, `None` when there are none.
fn finite_bounds(values: impl IntoIterator<Item = f32>) -> Option<(f32, f32)> {
    values
        .into_iter()
        .filter(|v| v.is_finite())
        .fold(None, |acc, v| match acc {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
}

/// Widen `lo..hi` to at least `min_span` (around its center) and add a
/// little headroom so lines do not touch the edges.
fn padded_range(lo: f32, hi: f32, min_span: f32) -> (f32, f32) {
    let min_span = if min_span.is_finite() {
        min_span.max(1e-6)
    } else {
        1.0
    };
    let (mut lo, mut hi) = (lo, hi);
    if hi - lo < min_span {
        let c = (lo + hi) / 2.0;
        lo = c - min_span / 2.0;
        hi = c + min_span / 2.0;
    }
    let pad = (hi - lo) * 0.08;
    (lo - pad, hi + pad)
}

/// A vertical gradient under a polyline down to `bottom`: `color` at the
/// line, transparent at the bottom.
fn area_mesh(points: &[Pos2], bottom: f32, color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    if points.len() < 2 {
        return mesh;
    }
    for (i, p) in points.iter().enumerate() {
        mesh.colored_vertex(*p, color);
        mesh.colored_vertex(pos2(p.x, bottom), Color32::TRANSPARENT);
        if i > 0 {
            let base = (2 * i) as u32;
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base + 1, base);
        }
    }
    mesh
}

/// A dashed horizontal line.
fn dashed_hline(x: egui::Rangef, y: f32, stroke: Stroke) -> Vec<Shape> {
    Shape::dashed_line(&[pos2(x.min, y), pos2(x.max, y)], stroke, 4.0, 3.0)
}

/// A compact trend line of evenly spaced samples, e.g. the last minutes of
/// a heater's temperature. NaN samples leave a gap.
///
/// ```no_run
/// # use sprint_gui::widgets::Sparkline;
/// # fn demo(ui: &mut egui::Ui, history: &[f32]) {
/// ui.add(Sparkline::new(history).reference(Some(215.0)).height(32.0));
/// # }
/// ```
#[must_use = "add it with `ui.add(...)`"]
pub struct Sparkline<'a> {
    values: &'a [f32],
    color: Option<Color32>,
    width: Option<f32>,
    height: f32,
    fill: bool,
    smooth: bool,
    range: Option<(f32, f32)>,
    reference: Option<f32>,
    min_span: f32,
    end_dot: bool,
}

impl<'a> Sparkline<'a> {
    /// A sparkline of `values` (oldest first), 36 pt high, full width.
    pub fn new(values: &'a [f32]) -> Self {
        Sparkline {
            values,
            color: None,
            width: None,
            height: 36.0,
            fill: true,
            smooth: true,
            range: None,
            reference: None,
            min_span: 1.0,
            end_dot: true,
        }
    }

    /// Line color (default: the accent).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Fixed width in points (default: the available width).
    pub fn width(mut self, width: f32) -> Self {
        if let Some(width) = super::finite_clamped(width, 1.0, super::MAX_EXTENT) {
            self.width = Some(width);
        }
        self
    }

    /// Height in points.
    pub fn height(mut self, height: f32) -> Self {
        if let Some(height) = super::finite_clamped(height, 4.0, super::MAX_EXTENT) {
            self.height = height;
        }
        self
    }

    /// Gradient fill under the line (default `true`).
    pub fn fill(mut self, fill: bool) -> Self {
        self.fill = fill;
        self
    }

    /// Smooth (monotone cubic) curve instead of straight segments
    /// (default `true`).
    pub fn smooth(mut self, smooth: bool) -> Self {
        self.smooth = smooth;
        self
    }

    /// Fixed value range; by default it fits the data and the reference.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        if min.is_finite() && max.is_finite() && min < max {
            self.range = Some((min, max));
        }
        self
    }

    /// A dashed reference line, e.g. the heater target. `None` or a
    /// non-finite value draws nothing.
    pub fn reference(mut self, value: Option<f32>) -> Self {
        self.reference = value.filter(|v| v.is_finite());
        self
    }

    /// The smallest value span shown, so sensor noise on a flat line is not
    /// magnified into a zig-zag (default 1.0).
    pub fn min_span(mut self, span: f32) -> Self {
        if span.is_finite() && span > 0.0 {
            self.min_span = span;
        }
        self
    }

    /// Mark the latest sample with a dot (default `true`).
    pub fn end_dot(mut self, end_dot: bool) -> Self {
        self.end_dot = end_dot;
        self
    }
}

impl Widget for Sparkline<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let width = self.width.unwrap_or_else(|| ui.available_width().max(24.0));
        let (rect, response) = ui.allocate_exact_size(vec2(width, self.height), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "sparkline"));
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let p = theme::ui_palette(ui);
        let color = self.color.unwrap_or(p.accent);
        let painter = ui.painter_at(rect.expand(3.0));
        let bounds = finite_bounds(self.values.iter().copied().chain(self.reference));
        let Some((lo, hi)) = self.range.or(bounds) else {
            // No data yet: a faint baseline.
            painter.hline(
                rect.x_range(),
                rect.center().y,
                Stroke::new(1.0, with_alpha(p.text_faint, 0.5)),
            );
            return response;
        };
        let (lo, hi) = if self.range.is_some() {
            (lo, hi)
        } else {
            padded_range(lo, hi, self.min_span)
        };
        let plot = rect.shrink2(vec2(if self.end_dot { 3.0 } else { 0.0 }, 2.0));
        let n = self.values.len();
        let x_of = |i: usize| {
            if n <= 1 {
                plot.max.x
            } else {
                plot.min.x + plot.width() * i as f32 / (n - 1) as f32
            }
        };
        let y_of = |v: f32| plot.max.y - plot.height() * ((v - lo) / (hi - lo)).clamp(-0.05, 1.05);
        if let Some(r) = self.reference {
            painter.extend(dashed_hline(
                plot.x_range(),
                y_of(r),
                Stroke::new(1.0, with_alpha(color, 0.45)),
            ));
        }
        let mut last = None;
        for (start, run) in finite_runs(self.values) {
            let pts: Vec<Pos2> = run
                .iter()
                .enumerate()
                .map(|(j, v)| pos2(x_of(start + j), y_of(*v)))
                .collect();
            let pts = if self.smooth {
                smooth_points(&pts, 6)
            } else {
                pts
            };
            if self.fill {
                painter.add(area_mesh(
                    &pts,
                    rect.max.y,
                    with_alpha(color, if p.dark { 0.22 } else { 0.16 }),
                ));
            }
            if pts.len() == 1 {
                painter.circle_filled(pts[0], 1.5, color);
            } else {
                painter.add(PathShape::line(pts.clone(), Stroke::new(1.6, color)));
            }
            last = pts.last().copied();
        }
        if self.end_dot
            && let Some(end) = last
            && self.values.last().is_some_and(|v| v.is_finite())
        {
            painter.circle_filled(end, 4.5, with_alpha(color, 0.25));
            painter.circle_filled(end, 2.5, color);
        }
        response
    }
}

/// One line of a [`MiniChart`].
#[derive(Clone, Debug)]
pub struct ChartSeries<'a> {
    /// Legend and tooltip name.
    pub name: &'a str,
    /// `[x, y]` points sorted by `x`; non-finite points leave a gap.
    pub points: &'a [[f64; 2]],
    /// Line color.
    pub color: Color32,
    /// Draw dashed (e.g. a target line).
    pub dashed: bool,
    /// Gradient fill under the line.
    pub fill: bool,
}

impl<'a> ChartSeries<'a> {
    /// A solid, unfilled series.
    pub fn new(name: &'a str, points: &'a [[f64; 2]], color: Color32) -> Self {
        ChartSeries {
            name,
            points,
            color,
            dashed: false,
            fill: false,
        }
    }

    /// Dashed line.
    pub fn dashed(mut self, dashed: bool) -> Self {
        self.dashed = dashed;
        self
    }

    /// Gradient fill under the line.
    pub fn fill(mut self, fill: bool) -> Self {
        self.fill = fill;
        self
    }
}

/// A smooth, compact line chart with a light grid, y-axis labels, an
/// optional legend and a hover read-out. For temperature history in a card,
/// calibration results and similar small trends.
///
/// ```no_run
/// # use sprint_gui::widgets::{ChartSeries, MiniChart};
/// # fn demo(ui: &mut egui::Ui, temps: &[[f64; 2]], targets: &[[f64; 2]]) {
/// MiniChart::new()
///     .series(ChartSeries::new("Hotend", temps, egui::Color32::ORANGE).fill(true))
///     .series(ChartSeries::new("Target", targets, egui::Color32::ORANGE).dashed(true))
///     .unit("°C")
///     .show(ui);
/// # }
/// ```
#[must_use = "call `.show(ui)`"]
pub struct MiniChart<'a> {
    series: Vec<ChartSeries<'a>>,
    height: f32,
    unit: &'a str,
    y_range: Option<(f64, f64)>,
    min_y_span: f64,
    legend: bool,
    decimals: usize,
}

impl Default for MiniChart<'_> {
    fn default() -> Self {
        MiniChart::new()
    }
}

impl<'a> MiniChart<'a> {
    /// An empty chart, 120 pt high.
    pub fn new() -> Self {
        MiniChart {
            series: Vec::new(),
            height: 120.0,
            unit: "",
            y_range: None,
            min_y_span: 1.0,
            legend: true,
            decimals: 1,
        }
    }

    /// Add a line.
    pub fn series(mut self, series: ChartSeries<'a>) -> Self {
        self.series.push(series);
        self
    }

    /// Plot height in points (legend excluded).
    pub fn height(mut self, height: f32) -> Self {
        if let Some(height) = super::finite_clamped(height, 4.0, super::MAX_EXTENT) {
            self.height = height;
        }
        self
    }

    /// Unit appended to values in the hover read-out.
    pub fn unit(mut self, unit: &'a str) -> Self {
        self.unit = unit;
        self
    }

    /// Fixed y range (default: fit the data).
    pub fn y_range(mut self, min: f64, max: f64) -> Self {
        if min.is_finite() && max.is_finite() && min < max {
            self.y_range = Some((min, max));
        }
        self
    }

    /// Smallest y span when fitting the data (default 1.0).
    pub fn min_y_span(mut self, span: f64) -> Self {
        if span.is_finite() && span > 0.0 {
            self.min_y_span = span;
        }
        self
    }

    /// Show the legend row under the plot (default `true`).
    pub fn legend(mut self, legend: bool) -> Self {
        self.legend = legend;
        self
    }

    /// Decimals in labels and the read-out (default 1).
    pub fn decimals(mut self, decimals: usize) -> Self {
        self.decimals = decimals.min(6);
        self
    }

    /// Draw the chart. The response covers the plot area.
    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::ui_palette(ui);
        let label_font = theme::font(ui, theme::TextRole::Caption);
        let finite = |pt: &&[f64; 2]| pt[0].is_finite() && pt[1].is_finite();
        let all = || {
            self.series
                .iter()
                .flat_map(|s| s.points.iter().filter(finite))
        };
        let x_bounds = all().fold(None, |acc: Option<(f64, f64)>, pt| match acc {
            None => Some((pt[0], pt[0])),
            Some((lo, hi)) => Some((lo.min(pt[0]), hi.max(pt[0]))),
        });
        let y_bounds = self.y_range.or_else(|| {
            all()
                .fold(None, |acc: Option<(f64, f64)>, pt| match acc {
                    None => Some((pt[1], pt[1])),
                    Some((lo, hi)) => Some((lo.min(pt[1]), hi.max(pt[1]))),
                })
                .map(|(lo, hi)| {
                    let (lo, hi) = padded_range(lo as f32, hi as f32, self.min_y_span as f32);
                    (f64::from(lo), f64::from(hi))
                })
        });
        let ticks = y_bounds.map_or_else(Vec::new, |(lo, hi)| nice_ticks(lo, hi, 4));
        let fmt = |v: f64| format!("{v:.*}", tick_decimals(&ticks, self.decimals));
        let label_w = ticks
            .iter()
            .map(|t| {
                ui.painter()
                    .layout_no_wrap(fmt(*t), label_font.clone(), Color32::PLACEHOLDER)
                    .size()
                    .x
            })
            .fold(0.0_f32, f32::max);
        let width = ui.available_width().max(60.0);
        let (outer, response) = ui.allocate_exact_size(vec2(width, self.height), Sense::hover());
        let plot = Rect::from_min_max(
            pos2(outer.min.x + label_w + 8.0, outer.min.y + 4.0),
            pos2(outer.max.x - 2.0, outer.max.y - 4.0),
        );
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "chart"));
        if ui.is_rect_visible(outer) {
            let painter = ui.painter_at(outer.expand(4.0));
            match (x_bounds, y_bounds) {
                (Some((x0, x1)), Some((y0, y1))) if plot.width() > 4.0 => {
                    let x_span = if x1 - x0 > f64::EPSILON { x1 - x0 } else { 1.0 };
                    let to_screen = |pt: [f64; 2]| {
                        pos2(
                            plot.min.x + plot.width() * ((pt[0] - x0) / x_span) as f32,
                            plot.max.y - plot.height() * ((pt[1] - y0) / (y1 - y0)) as f32,
                        )
                    };
                    for t in &ticks {
                        let y = to_screen([x0, *t]).y;
                        if y < plot.min.y - 1.0 || y > plot.max.y + 1.0 {
                            continue;
                        }
                        painter.hline(
                            plot.x_range(),
                            y,
                            Stroke::new(1.0, with_alpha(p.border, 0.8)),
                        );
                        painter.text(
                            pos2(plot.min.x - 8.0, y),
                            Align2::RIGHT_CENTER,
                            fmt(*t),
                            label_font.clone(),
                            p.text_faint,
                        );
                    }
                    for s in &self.series {
                        paint_series(&painter, &p, s, plot, &to_screen);
                    }
                    if let Some(hover) = response.hover_pos().filter(|h| plot.contains(*h)) {
                        let x = x0 + f64::from((hover.x - plot.min.x) / plot.width()) * x_span;
                        hover_readout(ui, &painter, &p, &self, plot, x, &to_screen);
                    }
                }
                _ => {
                    painter.rect_stroke(
                        plot,
                        CornerRadius::same(theme::RADIUS),
                        Stroke::new(1.0, p.border),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        plot.center(),
                        Align2::CENTER_CENTER,
                        "No data",
                        FontId::proportional(12.0),
                        p.text_faint,
                    );
                }
            }
        }
        if self.legend && !self.series.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(label_w + 8.0);
                ui.spacing_mut().item_spacing.x = 14.0;
                for s in &self.series {
                    legend_entry(ui, &p, s);
                }
            });
        }
        response
    }
}

/// Decimals needed to tell ticks apart, at least `min`.
fn tick_decimals(ticks: &[f64], min: usize) -> usize {
    match ticks {
        [a, b, ..] => {
            let step = (b - a).abs();
            if step >= 1.0 || step <= 0.0 {
                0
            } else {
                ((-step.log10()).ceil() as usize).min(min.max(1) + 2)
            }
        }
        _ => min,
    }
}

fn paint_series(
    painter: &egui::Painter,
    p: &Palette,
    s: &ChartSeries<'_>,
    plot: Rect,
    to_screen: &impl Fn([f64; 2]) -> Pos2,
) {
    let mut runs: Vec<Vec<Pos2>> = vec![Vec::new()];
    for pt in s.points {
        if pt[0].is_finite() && pt[1].is_finite() {
            if let Some(run) = runs.last_mut() {
                run.push(to_screen(*pt));
            }
        } else if runs.last().is_some_and(|r| !r.is_empty()) {
            runs.push(Vec::new());
        }
    }
    let stroke = Stroke::new(if s.dashed { 1.2 } else { 1.8 }, s.color);
    for run in runs.into_iter().filter(|r| !r.is_empty()) {
        let pts = if s.dashed {
            run
        } else {
            smooth_points(&run, 6)
        };
        if s.fill {
            painter.add(area_mesh(
                &pts,
                plot.max.y,
                with_alpha(s.color, if p.dark { 0.20 } else { 0.14 }),
            ));
        }
        if pts.len() == 1 {
            painter.circle_filled(pts[0], 2.0, s.color);
        } else if s.dashed {
            painter.extend(Shape::dashed_line(&pts, stroke, 5.0, 4.0));
        } else {
            painter.add(PathShape::line(pts, stroke));
        }
    }
}

/// The y value of a series at `x` (linear between neighbours), if `x` lies
/// within its finite data.
fn value_at(points: &[[f64; 2]], x: f64) -> Option<f64> {
    let finite: Vec<[f64; 2]> = points
        .iter()
        .copied()
        .filter(|p| p[0].is_finite() && p[1].is_finite())
        .collect();
    let i = finite.partition_point(|p| p[0] < x);
    match (i.checked_sub(1).and_then(|j| finite.get(j)), finite.get(i)) {
        (Some(a), Some(b)) => {
            let span = b[0] - a[0];
            if span.abs() < f64::EPSILON {
                Some(b[1])
            } else {
                Some(a[1] + (b[1] - a[1]) * (x - a[0]) / span)
            }
        }
        (None, Some(b)) if (b[0] - x).abs() < f64::EPSILON => Some(b[1]),
        (Some(a), None) if (a[0] - x).abs() < f64::EPSILON => Some(a[1]),
        _ => None,
    }
}

fn hover_readout(
    ui: &Ui,
    painter: &egui::Painter,
    p: &Palette,
    chart: &MiniChart<'_>,
    plot: Rect,
    x: f64,
    to_screen: &impl Fn([f64; 2]) -> Pos2,
) {
    let sx = to_screen([x, 0.0]).x;
    painter.vline(
        sx,
        plot.y_range(),
        Stroke::new(1.0, with_alpha(p.text_dim, 0.5)),
    );
    let mut lines = Vec::new();
    for s in &chart.series {
        if let Some(y) = value_at(s.points, x) {
            let pt = to_screen([x, y]);
            painter.circle_filled(pt, 3.5, s.color);
            painter.circle_stroke(pt, 3.5, Stroke::new(1.5, p.surface));
            lines.push(format!(
                "{}: {:.*}{}{}",
                s.name,
                chart.decimals,
                y,
                if chart.unit.is_empty() { "" } else { " " },
                chart.unit
            ));
        }
    }
    if !lines.is_empty() {
        egui::Tooltip::always_open(
            ui.ctx().clone(),
            ui.layer_id(),
            super::widget_id(ui, "chart-readout"),
            egui::PopupAnchor::Pointer,
        )
        .gap(12.0)
        .show(|ui| {
            for line in lines {
                ui.label(line);
            }
        });
    }
}

fn legend_entry(ui: &mut Ui, p: &Palette, s: &ChartSeries<'_>) {
    let font = FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(s.name.to_owned(), font, p.text_dim);
    let (rect, _) = ui.allocate_exact_size(
        vec2(18.0 + galley.size().x, galley.size().y.max(14.0)),
        Sense::hover(),
    );
    let y = rect.center().y;
    let line = [pos2(rect.min.x, y), pos2(rect.min.x + 12.0, y)];
    if s.dashed {
        ui.painter().extend(Shape::dashed_line(
            &line,
            Stroke::new(1.5, s.color),
            3.0,
            2.0,
        ));
    } else {
        ui.painter().line_segment(line, Stroke::new(2.0, s.color));
    }
    let pos = pos2(rect.min.x + 18.0, y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, p.text_dim);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_passes_through_inputs_and_never_overshoots() {
        let pts = [
            pos2(0.0, 0.0),
            pos2(1.0, 10.0),
            pos2(2.0, 10.0),
            pos2(3.0, 2.0),
            pos2(4.0, 2.5),
        ];
        let out = smooth_points(&pts, 8);
        assert_eq!(out.len(), 4 * 8 + 1);
        for p in &pts {
            assert!(
                out.iter()
                    .any(|q| (q.x - p.x).abs() < 1e-5 && (q.y - p.y).abs() < 1e-4)
            );
        }
        // Each segment stays within its endpoints (monotone, no overshoot).
        for seg in out.chunks(8).zip(pts.windows(2)) {
            let (samples, w) = seg;
            let (lo, hi) = (w[0].y.min(w[1].y), w[0].y.max(w[1].y));
            for q in samples {
                assert!(
                    q.y >= lo - 1e-4 && q.y <= hi + 1e-4,
                    "{q:?} outside {lo}..{hi}"
                );
            }
        }
        // x increases monotonically.
        assert!(out.windows(2).all(|w| w[1].x >= w[0].x));
    }

    #[test]
    fn smooth_handles_short_and_degenerate_input() {
        assert!(smooth_points(&[], 8).is_empty());
        assert_eq!(smooth_points(&[pos2(1.0, 1.0)], 8).len(), 1);
        assert_eq!(smooth_points(&[pos2(0.0, 0.0), pos2(1.0, 1.0)], 8).len(), 2);
        assert_eq!(
            smooth_points(&[pos2(0.0, 0.0), pos2(1.0, 1.0), pos2(2.0, 0.0)], 1).len(),
            3
        );
        // Duplicate x values do not produce NaN.
        let out = smooth_points(&[pos2(0.0, 0.0), pos2(0.0, 5.0), pos2(1.0, 1.0)], 4);
        assert!(out.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
    }

    #[test]
    fn finite_runs_split_on_gaps() {
        let v = [1.0, 2.0, f32::NAN, 3.0, f32::INFINITY, f32::NAN, 4.0, 5.0];
        let runs = finite_runs(&v);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0], (0, &v[0..2]));
        assert_eq!(runs[1].0, 3);
        assert_eq!(runs[2], (6, &v[6..8]));
        assert!(finite_runs(&[f32::NAN]).is_empty());
        assert!(finite_runs(&[]).is_empty());
    }

    #[test]
    fn nice_ticks_are_round_and_cover_range() {
        assert_eq!(
            nice_ticks(0.0, 100.0, 5),
            vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]
        );
        assert_eq!(nice_ticks(20.0, 230.0, 4), vec![50.0, 100.0, 150.0, 200.0]);
        let t = nice_ticks(-0.12, 0.31, 4);
        assert_eq!(t, vec![-0.1, 0.0, 0.1, 0.2, 0.3]);
        assert!(t[1].is_sign_positive(), "no negative zero");
        assert_eq!(nice_ticks(5.0, 5.0, 4), vec![5.0]);
        assert!(nice_ticks(f64::NAN, 1.0, 4).is_empty());
        assert!(nice_ticks(0.0, f64::INFINITY, 4).is_empty());
        assert!(nice_ticks(0.0, 1.0, 0).is_empty());
        // Reversed bounds work too.
        assert_eq!(nice_ticks(10.0, 0.0, 2), vec![0.0, 5.0, 10.0]);
        // Huge ranges stay bounded.
        assert!(nice_ticks(-1e300, 1e300, 10).len() <= 12);
    }

    #[test]
    fn value_at_interpolates_within_data_only() {
        let pts = [[0.0, 0.0], [10.0, 100.0], [f64::NAN, 5.0], [20.0, 0.0]];
        assert_eq!(value_at(&pts, 5.0), Some(50.0));
        assert_eq!(value_at(&pts, 15.0), Some(50.0));
        assert_eq!(value_at(&pts, 0.0), Some(0.0));
        assert_eq!(value_at(&pts, 20.0), Some(0.0));
        assert_eq!(value_at(&pts, -1.0), None);
        assert_eq!(value_at(&pts, 21.0), None);
        assert_eq!(value_at(&[], 1.0), None);
    }

    #[test]
    fn padded_range_enforces_min_span() {
        let (lo, hi) = padded_range(200.0, 200.2, 2.0);
        assert!(hi - lo >= 2.0);
        assert!(lo < 200.0 && hi > 200.2);
        let (lo, hi) = padded_range(0.0, 100.0, f32::NAN);
        assert!(lo < 0.0 && hi > 100.0);
    }

    #[test]
    fn tick_decimals_follow_step() {
        assert_eq!(tick_decimals(&[0.0, 50.0], 1), 0);
        assert_eq!(tick_decimals(&[0.0, 0.1], 1), 1);
        assert_eq!(tick_decimals(&[0.0, 0.05], 1), 2);
        assert_eq!(tick_decimals(&[1.0], 2), 2);
    }
}
