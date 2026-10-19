//! [`Heatmap`]: a 2D grid of values such as a bed mesh, colored with a
//! diverging blue-white-red map centered at zero.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Mesh, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::theme::{self, Palette, mix, readable_text_on, with_alpha};

/// Colormap stops (RdBu): deep blue, light blue, white, light red, deep red.
const STOPS: [(f32, u32); 5] = [
    (-1.0, 0x2166AC),
    (-0.5, 0x67A9CF),
    (0.0, 0xF7F7F7),
    (0.5, 0xEF8A62),
    (1.0, 0xB2182B),
];

/// The diverging color for `t` in `-1..=1` (clamped): blue below zero,
/// white at zero, red above. NaN gives a neutral gray.
pub fn diverging_color(t: f32) -> Color32 {
    if t.is_nan() {
        return theme::hex(0x8A9099);
    }
    let t = t.clamp(-1.0, 1.0);
    for pair in STOPS.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if t <= t1 {
            return mix(theme::hex(c0), theme::hex(c1), (t - t0) / (t1 - t0));
        }
    }
    theme::hex(STOPS[STOPS.len() - 1].1)
}

/// Summary of the finite values of a grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridStats {
    /// Smallest value.
    pub min: f32,
    /// Largest value.
    pub max: f32,
    /// Arithmetic mean.
    pub mean: f32,
    /// Number of finite values.
    pub count: usize,
    /// Number of missing (non-finite) cells, including ragged-row gaps.
    pub missing: usize,
}

impl GridStats {
    /// Of the finite values in `grid` (rows may differ in length; short
    /// rows count their gaps as missing). `None` when nothing is finite.
    pub fn of(grid: &[Vec<f32>]) -> Option<GridStats> {
        let cols = grid.iter().map(Vec::len).max().unwrap_or(0);
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum = 0.0_f64;
        let mut count = 0;
        let mut missing = 0;
        for row in grid {
            missing += cols - row.len();
            for v in row {
                if v.is_finite() {
                    min = min.min(*v);
                    max = max.max(*v);
                    sum += f64::from(*v);
                    count += 1;
                } else {
                    missing += 1;
                }
            }
        }
        (count > 0).then(|| GridStats {
            min,
            max,
            mean: (sum / count as f64) as f32,
            count,
            missing,
        })
    }

    /// `max - min`.
    pub fn range(&self) -> f32 {
        self.max - self.min
    }
}

/// One cell of a [`Heatmap`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeatmapCell {
    /// Row index (0 = first row of the grid, drawn at the bottom).
    pub row: usize,
    /// Column index (0 = left).
    pub col: usize,
    /// The value (NaN for missing cells).
    pub value: f32,
}

/// What [`Heatmap::show`] returns.
#[derive(Debug)]
pub struct HeatmapResponse {
    /// The response of the cell area.
    pub response: Response,
    /// The cell under the pointer.
    pub hovered: Option<HeatmapCell>,
    /// The cell clicked this frame.
    pub clicked: Option<HeatmapCell>,
}

/// A value with an explicit sign: "+0.123", "−0.050", "0.000".
pub fn signed_value(value: f32, decimals: usize) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }
    let decimals = decimals.min(6);
    let text = format!("{:.*}", decimals, value.abs());
    if text.chars().all(|c| c == '0' || c == '.') {
        text
    } else if value < 0.0 {
        format!("\u{2212}{text}")
    } else {
        format!("+{text}")
    }
}

/// A grid of values (e.g. a bed mesh) as colored cells. Row 0 is drawn at
/// the bottom (the front of the bed), column 0 on the left. Missing values
/// (NaN, or short rows) are hatched.
///
/// ```no_run
/// # use sprint_gui::widgets::Heatmap;
/// # fn demo(ui: &mut egui::Ui, mesh: &sprint_proto::BedMesh) {
/// let r = Heatmap::from_mesh(mesh).show(ui);
/// if let Some(cell) = r.clicked { /* probe again at that point */ }
/// # }
/// ```
#[must_use = "call `.show(ui)`"]
pub struct Heatmap<'a> {
    grid: &'a [Vec<f32>],
    x_positions: Option<&'a [f32]>,
    y_positions: Option<&'a [f32]>,
    unit: &'a str,
    decimals: usize,
    labels: bool,
    legend: bool,
    scale: Option<f32>,
    max_cell: f32,
    id_salt: Option<Id>,
}

impl<'a> Heatmap<'a> {
    /// A heatmap of `grid[row][col]`.
    pub fn new(grid: &'a [Vec<f32>]) -> Self {
        Heatmap {
            grid,
            x_positions: None,
            y_positions: None,
            unit: "mm",
            decimals: 3,
            labels: true,
            legend: true,
            scale: None,
            max_cell: 64.0,
            id_salt: None,
        }
    }

    /// A heatmap of a probed bed mesh, with its probe positions as axes.
    pub fn from_mesh(mesh: &'a sprint_proto::BedMesh) -> Self {
        let mut map = Heatmap::new(&mesh.points);
        map.x_positions = mesh.x_positions.as_deref();
        map.y_positions = mesh.y_positions.as_deref();
        map
    }

    /// Axis positions of the columns and rows (used when their lengths
    /// match the grid).
    pub fn positions(mut self, x: Option<&'a [f32]>, y: Option<&'a [f32]>) -> Self {
        self.x_positions = x;
        self.y_positions = y;
        self
    }

    /// Unit of the values (default "mm").
    pub fn unit(mut self, unit: &'a str) -> Self {
        self.unit = unit;
        self
    }

    /// Decimals of the labels (default 3).
    pub fn decimals(mut self, decimals: usize) -> Self {
        self.decimals = decimals.min(6);
        self
    }

    /// Print values in cells that are large enough (default `true`).
    pub fn labels(mut self, labels: bool) -> Self {
        self.labels = labels;
        self
    }

    /// Show the color legend and statistics (default `true`).
    pub fn legend(mut self, legend: bool) -> Self {
        self.legend = legend;
        self
    }

    /// Fixed half-range of the colormap (e.g. 0.3 mm), so several meshes
    /// are comparable. Default: the largest absolute value.
    pub fn scale(mut self, scale: f32) -> Self {
        if scale.is_finite() && scale > 0.0 {
            self.scale = Some(scale);
        }
        self
    }

    /// Largest cell side in points (16..=160, default 64).
    pub fn max_cell(mut self, size: f32) -> Self {
        if size.is_finite() {
            self.max_cell = size.clamp(16.0, 160.0);
        }
        self
    }

    /// A stable id salt (by default the id follows the position in the parent).
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(Id::new(salt));
        self
    }

    fn rows(&self) -> usize {
        self.grid.len()
    }

    fn cols(&self) -> usize {
        self.grid.iter().map(Vec::len).max().unwrap_or(0)
    }

    fn value(&self, row: usize, col: usize) -> f32 {
        self.grid
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or(f32::NAN)
    }

    /// The colormap half-range actually used.
    fn effective_scale(&self, stats: Option<&GridStats>) -> f32 {
        self.scale.unwrap_or_else(|| {
            stats
                .map(|s| s.min.abs().max(s.max.abs()))
                .filter(|s| *s > 1e-6)
                .unwrap_or(1.0)
        })
    }

    fn axis_label(positions: Option<&[f32]>, len: usize, index: usize) -> Option<String> {
        let p = positions.filter(|p| p.len() == len)?;
        let v = *p.get(index)?;
        v.is_finite().then(|| {
            let s = format!("{v:.1}");
            s.strip_suffix(".0").map_or(s.clone(), str::to_owned)
        })
    }

    fn cell_name(&self, row: usize, col: usize) -> String {
        match (
            Self::axis_label(self.x_positions, self.cols(), col),
            Self::axis_label(self.y_positions, self.rows(), row),
        ) {
            (Some(x), Some(y)) => format!("X {x}  Y {y}"),
            _ => format!("Row {row}, column {col}"),
        }
    }

    /// Show the heatmap.
    pub fn show(self, ui: &mut Ui) -> HeatmapResponse {
        let p = theme::ui_palette(ui);
        let (rows, cols) = (self.rows(), self.cols());
        if rows == 0 || cols == 0 {
            let r = super::EmptyState::new(crate::icons::MESH, "No mesh data")
                .message("Probe the bed to see its topography.")
                .show(ui);
            return HeatmapResponse {
                response: r.response,
                hovered: None,
                clicked: None,
            };
        }
        let stats = GridStats::of(self.grid);
        let scale = self.effective_scale(stats.as_ref());
        let has_y = self.y_positions.is_some_and(|p| p.len() == rows);
        let has_x = self.x_positions.is_some_and(|p| p.len() == cols);
        let gutter_left = if has_y { 40.0 } else { 0.0 };
        let gutter_bottom = if has_x { 18.0 } else { 0.0 };
        let avail = (ui.available_width() - gutter_left).max(cols as f32 * 4.0);
        let cell = (avail / cols as f32).clamp(4.0, self.max_cell);
        let grid_size = vec2(cell * cols as f32, cell * rows as f32);
        let (outer, alloc) = ui.allocate_exact_size(
            vec2(gutter_left + grid_size.x, grid_size.y + gutter_bottom),
            Sense::hover(),
        );
        let grid_rect =
            Rect::from_min_size(pos2(outer.min.x + gutter_left, outer.min.y), grid_size);
        let id = match self.id_salt {
            Some(salt) => super::widget_id(ui, salt),
            None => alloc.id.with("cells"),
        };
        let response = ui.interact(grid_rect, id, Sense::click());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "heatmap"));
        let cell_at = |pos: Pos2| -> Option<HeatmapCell> {
            if !grid_rect.contains(pos) {
                return None;
            }
            let col = (((pos.x - grid_rect.min.x) / cell) as usize).min(cols - 1);
            let display_row = (((pos.y - grid_rect.min.y) / cell) as usize).min(rows - 1);
            let row = rows - 1 - display_row;
            Some(HeatmapCell {
                row,
                col,
                value: self.value(row, col),
            })
        };
        let hovered = response.hover_pos().and_then(cell_at);
        // The cell under the press, not the release (a click may end a few
        // points away, over a neighbour).
        let clicked = super::PressOrigin::track(ui, &response)
            .clicked
            .and_then(cell_at);
        if ui.is_rect_visible(outer) {
            self.paint_cells(ui, &p, grid_rect, cell, scale, hovered);
            self.paint_axes(ui, &p, grid_rect, cell, has_x, has_y);
        }
        let response = match hovered {
            Some(c) => {
                let text = format!(
                    "{}\n{} {}",
                    self.cell_name(c.row, c.col),
                    if c.value.is_finite() {
                        signed_value(c.value, self.decimals)
                    } else {
                        "not probed".to_owned()
                    },
                    if c.value.is_finite() { self.unit } else { "" }
                );
                response
                    .on_hover_cursor(CursorIcon::Crosshair)
                    .on_hover_text_at_pointer(text)
            }
            None => response,
        };
        if self.legend {
            ui.add_space(8.0);
            self.paint_legend(ui, &p, grid_size.x + gutter_left, scale, stats.as_ref());
        }
        HeatmapResponse {
            response,
            hovered,
            clicked,
        }
    }

    fn paint_cells(
        &self,
        ui: &Ui,
        p: &Palette,
        grid_rect: Rect,
        cell: f32,
        scale: f32,
        hovered: Option<HeatmapCell>,
    ) {
        let painter = ui.painter_at(grid_rect.expand(2.0));
        let (rows, cols) = (self.rows(), self.cols());
        let gap = if cell >= 12.0 { 2.0 } else { 1.0 };
        let radius = CornerRadius::same(if cell >= 24.0 { 4 } else { 1 });
        let decimals = if cell >= 52.0 {
            self.decimals
        } else {
            self.decimals.min(2)
        };
        let font = FontId::proportional((cell * 0.24).clamp(9.0, 12.0));
        let show_labels = self.labels && cell >= 34.0;
        for row in 0..rows {
            for col in 0..cols {
                let display_row = rows - 1 - row;
                let r = Rect::from_min_size(
                    pos2(
                        grid_rect.min.x + col as f32 * cell,
                        grid_rect.min.y + display_row as f32 * cell,
                    ),
                    vec2(cell, cell),
                )
                .shrink(gap / 2.0);
                let v = self.value(row, col);
                if v.is_finite() {
                    let fill = diverging_color(v / scale);
                    // Near-white cells need an edge on light surfaces.
                    let edge = if p.dark {
                        Stroke::NONE
                    } else {
                        Stroke::new(1.0, Color32::from_black_alpha(16))
                    };
                    painter.rect(r, radius, fill, edge, StrokeKind::Inside);
                    if show_labels {
                        painter.text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            signed_value(v, decimals),
                            font.clone(),
                            readable_text_on(fill),
                        );
                    }
                } else {
                    paint_missing(&painter, p, r, radius);
                }
            }
        }
        if let Some(h) = hovered {
            let display_row = rows - 1 - h.row;
            let r = Rect::from_min_size(
                pos2(
                    grid_rect.min.x + h.col as f32 * cell,
                    grid_rect.min.y + display_row as f32 * cell,
                ),
                vec2(cell, cell),
            )
            .shrink(gap / 2.0);
            painter.rect_stroke(r, radius, Stroke::new(2.0, p.text), StrokeKind::Inside);
        }
    }

    fn paint_axes(
        &self,
        ui: &Ui,
        p: &Palette,
        grid_rect: Rect,
        cell: f32,
        has_x: bool,
        has_y: bool,
    ) {
        let painter = ui.painter();
        let font = theme::font(ui, theme::TextRole::Caption);
        let (rows, cols) = (self.rows(), self.cols());
        // Label every row / column when they fit, else only the ends.
        let indices = |n: usize, size: f32, min_gap: f32| -> Vec<usize> {
            let step = if size >= min_gap {
                1
            } else {
                n.saturating_sub(1).max(1)
            };
            let mut v: Vec<usize> = (0..n).step_by(step).collect();
            v.push(n.saturating_sub(1));
            v.dedup();
            v
        };
        if has_x {
            for col in indices(cols, cell, 30.0) {
                if let Some(label) = Self::axis_label(self.x_positions, cols, col) {
                    painter.text(
                        pos2(
                            grid_rect.min.x + (col as f32 + 0.5) * cell,
                            grid_rect.max.y + 4.0,
                        ),
                        Align2::CENTER_TOP,
                        label,
                        font.clone(),
                        p.text_faint,
                    );
                }
            }
        }
        if has_y {
            for row in indices(rows, cell, 16.0) {
                if let Some(label) = Self::axis_label(self.y_positions, rows, row) {
                    let display_row = rows - 1 - row;
                    painter.text(
                        pos2(
                            grid_rect.min.x - 8.0,
                            grid_rect.min.y + (display_row as f32 + 0.5) * cell,
                        ),
                        Align2::RIGHT_CENTER,
                        label,
                        font.clone(),
                        p.text_faint,
                    );
                }
            }
        }
    }

    fn paint_legend(
        &self,
        ui: &mut Ui,
        p: &Palette,
        width: f32,
        scale: f32,
        stats: Option<&GridStats>,
    ) {
        let width = width.max(160.0).min(ui.available_width().max(160.0));
        let (rect, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
        if ui.is_rect_visible(rect) {
            let bar = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.min.y + 8.0));
            let mut mesh = Mesh::default();
            let steps = 32;
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let x = egui::lerp(bar.x_range(), t);
                let c = diverging_color(t * 2.0 - 1.0);
                mesh.colored_vertex(pos2(x, bar.min.y), c);
                mesh.colored_vertex(pos2(x, bar.max.y), c);
                if i > 0 {
                    let k = (2 * i) as u32;
                    mesh.add_triangle(k - 2, k - 1, k);
                    mesh.add_triangle(k - 1, k + 1, k);
                }
            }
            ui.painter().add(mesh);
            ui.painter().rect_stroke(
                bar,
                CornerRadius::same(2),
                Stroke::new(1.0, with_alpha(p.border_strong, 0.6)),
                StrokeKind::Outside,
            );
            let font = theme::font(ui, theme::TextRole::Caption);
            let y = bar.max.y + 4.0;
            ui.painter().text(
                pos2(bar.min.x, y),
                Align2::LEFT_TOP,
                signed_value(-scale, self.decimals),
                font.clone(),
                p.text_dim,
            );
            ui.painter().text(
                pos2(bar.center().x, y),
                Align2::CENTER_TOP,
                "0",
                font.clone(),
                p.text_dim,
            );
            ui.painter().text(
                pos2(bar.max.x, y),
                Align2::RIGHT_TOP,
                signed_value(scale, self.decimals),
                font,
                p.text_dim,
            );
        }
        if let Some(s) = stats {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                let d = self.decimals;
                stat(ui, p, "Min", &signed_value(s.min, d));
                stat(ui, p, "Max", &signed_value(s.max, d));
                stat(
                    ui,
                    p,
                    "Range",
                    &format!("{:.*} {}", d, s.range(), self.unit),
                );
                stat(ui, p, "Mean", &signed_value(s.mean, d));
                if s.missing > 0 {
                    stat(ui, p, "Missing", &s.missing.to_string());
                }
            });
        }
    }
}

/// "Label value" as one unbreakable label, so a wrapped row can move it to
/// the next line as a whole.
fn stat(ui: &mut Ui, p: &Palette, label: &str, value: &str) {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        0.0,
        egui::TextFormat::simple(FontId::proportional(12.0), p.text_dim),
    );
    job.append(
        value,
        5.0,
        egui::TextFormat::simple(FontId::proportional(12.5), p.text),
    );
    ui.add(egui::Label::new(job).extend());
}

/// A hatched placeholder for a missing cell.
fn paint_missing(painter: &egui::Painter, p: &Palette, r: Rect, radius: CornerRadius) {
    painter.rect(
        r,
        radius,
        with_alpha(p.text_faint, 0.12),
        Stroke::new(1.0, with_alpha(p.text_faint, 0.35)),
        StrokeKind::Inside,
    );
    let hatch = painter.with_clip_rect(r.shrink(1.0).intersect(painter.clip_rect()));
    let step = 6.0;
    let stroke = Stroke::new(1.0, with_alpha(p.text_faint, 0.35));
    let mut x = r.min.x - r.height();
    while x < r.max.x {
        hatch.line_segment([pos2(x, r.max.y), pos2(x + r.height(), r.min.y)], stroke);
        x += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colormap_endpoints_and_center() {
        assert_eq!(diverging_color(0.0), theme::hex(0xF7F7F7));
        assert_eq!(diverging_color(-1.0), theme::hex(0x2166AC));
        assert_eq!(diverging_color(1.0), theme::hex(0xB2182B));
        assert_eq!(diverging_color(-7.0), diverging_color(-1.0));
        assert_eq!(diverging_color(f32::INFINITY), diverging_color(1.0));
        assert_eq!(diverging_color(f32::NAN), theme::hex(0x8A9099));
        // Negative is blue-ish, positive red-ish.
        let neg = diverging_color(-0.6);
        let pos = diverging_color(0.6);
        assert!(neg.b() > neg.r());
        assert!(pos.r() > pos.b());
    }

    #[test]
    fn stats_skip_nan_and_count_gaps() {
        let grid = vec![
            vec![0.1, -0.2, f32::NAN],
            vec![0.3, 0.0],
            vec![f32::INFINITY, 0.05, 0.1],
        ];
        let s = GridStats::of(&grid).expect("stats");
        assert_eq!(s.min, -0.2);
        assert_eq!(s.max, 0.3);
        assert_eq!(s.count, 6);
        assert_eq!(s.missing, 3);
        assert!((s.range() - 0.5).abs() < 1e-6);
        assert!((s.mean - 0.058_333).abs() < 1e-4);
        assert_eq!(GridStats::of(&[]), None);
        assert_eq!(GridStats::of(&[vec![f32::NAN]]), None);
    }

    #[test]
    fn signed_values() {
        assert_eq!(signed_value(0.1234, 3), "+0.123");
        assert_eq!(signed_value(-0.05, 3), "\u{2212}0.050");
        assert_eq!(signed_value(-0.0001, 3), "0.000");
        assert_eq!(signed_value(0.0, 2), "0.00");
        assert_eq!(signed_value(f32::NAN, 2), "—");
        assert_eq!(signed_value(1.0, 99), "+1.000000");
    }

    #[test]
    fn scale_defaults_to_largest_magnitude() {
        let grid = vec![vec![-0.4, 0.1], vec![0.2, f32::NAN]];
        let map = Heatmap::new(&grid);
        let stats = GridStats::of(&grid);
        assert!((map.effective_scale(stats.as_ref()) - 0.4).abs() < 1e-6);
        let flat = vec![vec![0.0, 0.0]];
        let map = Heatmap::new(&flat);
        assert_eq!(map.effective_scale(GridStats::of(&flat).as_ref()), 1.0);
        let map = Heatmap::new(&grid).scale(0.25);
        assert_eq!(map.effective_scale(stats.as_ref()), 0.25);
        assert_eq!(Heatmap::new(&grid).scale(f32::NAN).scale, None);
    }

    #[test]
    fn ragged_rows_read_as_missing() {
        let grid = vec![vec![1.0, 2.0, 3.0], vec![4.0]];
        let map = Heatmap::new(&grid);
        assert_eq!(map.cols(), 3);
        assert_eq!(map.value(1, 0), 4.0);
        assert!(map.value(1, 2).is_nan());
        assert!(map.value(9, 9).is_nan());
    }

    #[test]
    fn cell_names_use_positions_when_they_match() {
        let grid = vec![vec![0.0, 0.0], vec![0.0, 0.0]];
        let x = [10.0, 200.0];
        let y = [15.5, 190.0];
        let map = Heatmap::new(&grid).positions(Some(&x), Some(&y));
        assert_eq!(map.cell_name(1, 0), "X 10  Y 190");
        let short = [1.0];
        let map = Heatmap::new(&grid).positions(Some(&short), Some(&y));
        assert_eq!(map.cell_name(0, 1), "Row 0, column 1");
    }
}
