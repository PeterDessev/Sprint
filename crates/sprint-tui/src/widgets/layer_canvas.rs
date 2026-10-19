//! Top-down view of one layer of a toolpath, drawn with Braille dots.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine, Rectangle};
use ratatui::widgets::{Block, Paragraph, Widget};
use sprint_gcode::{Bounds, Feature, Segment, Toolpath};

use crate::format;
use crate::theme::{Theme, default_theme};

/// Which area the layer view is zoomed to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LayerFit {
    /// The whole bed (falls back to the object when the bed size is
    /// unusable).
    #[default]
    Bed,
    /// The printed object's extents (all layers, so the view does not jump
    /// between layers).
    Object,
}

/// Tool position marker.
const TOOL_MARKER: &str = "●";

/// Bounds of the printed object: extrusions except custom start/end
/// G-code (purge lines and the like), or of all extrusions when that
/// leaves nothing. Empty for toolpaths without extrusions.
pub fn object_bounds(toolpath: &Toolpath) -> Bounds {
    let mut bounds = Bounds::empty();
    for s in toolpath
        .segments
        .iter()
        .filter(|s| s.is_extrusion() && s.feature != Feature::Custom)
    {
        if s.start.iter().chain(&s.end).all(|v| v.is_finite()) {
            bounds.include(s.start);
            bounds.include(s.end);
        }
    }
    if bounds.is_empty() {
        toolpath.bounds
    } else {
        bounds
    }
}

/// One layer of a [`Toolpath`] seen from above, colored by feature.
///
/// * Extrusions are drawn in their feature colors; with a progress line,
///   segments up to it are bright and the rest is ghosted, and a marker
///   shows the tool position.
/// * Optionally the previous layer is drawn ghosted underneath, and travel
///   moves of the layer in a dim color.
/// * The view fits the bed (with its outline) or the object bounds, keeping
///   millimetres square (a Braille cell is 2x4 dots and terminal cells are
///   about twice as tall as wide).
///
/// Out-of-range layer indices are clamped to the last layer; an empty
/// toolpath shows a message.
#[derive(Clone, Debug)]
pub struct LayerCanvas<'a> {
    toolpath: &'a Toolpath,
    layer: usize,
    progress_line: Option<u32>,
    bed_size: [f32; 2],
    bed_origin_center: bool,
    fit: LayerFit,
    show_previous: bool,
    show_travel: bool,
    show_tool: bool,
    show_label: bool,
    object_bounds: Option<Bounds>,
    block: Option<Block<'a>>,
    theme: &'a Theme,
}

/// The visible region in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
struct View {
    x: [f64; 2],
    y: [f64; 2],
}

impl View {
    /// The terminal cell of `area` whose Braille dots contain the point
    /// (x, y), with the same rounding the canvas uses for its dots.
    fn cell_of(&self, area: Rect, x: f64, y: f64) -> Option<(u16, u16)> {
        let (w, h) = (self.x[1] - self.x[0], self.y[1] - self.y[0]);
        if area.is_empty() || !(w > 0.0 && h > 0.0) {
            return None;
        }
        if !(self.x[0]..=self.x[1]).contains(&x) || !(self.y[0]..=self.y[1]).contains(&y) {
            return None;
        }
        let dots_x = f64::from(area.width) * 2.0 - 1.0;
        let dots_y = f64::from(area.height) * 4.0 - 1.0;
        // In range by the checks above, so the casts cannot overflow.
        let dx = ((x - self.x[0]) * dots_x / w).round() as u32 / 2;
        let dy = ((self.y[1] - y) * dots_y / h).round() as u32 / 4;
        let cx = u16::try_from(dx).ok()?.min(area.width - 1);
        let cy = u16::try_from(dy).ok()?.min(area.height - 1);
        Some((area.x + cx, area.y + cy))
    }
}

impl<'a> LayerCanvas<'a> {
    /// Layer `layer` of `toolpath`, with print progress up to
    /// `progress_line` (0-based source line; `None` draws everything
    /// bright) on a bed of `bed_size` millimetres (X, Y).
    pub fn new(
        toolpath: &'a Toolpath,
        layer: usize,
        progress_line: Option<u32>,
        bed_size: [f32; 2],
    ) -> LayerCanvas<'a> {
        LayerCanvas {
            toolpath,
            layer,
            progress_line,
            bed_size,
            bed_origin_center: false,
            fit: LayerFit::Bed,
            show_previous: true,
            show_travel: false,
            show_tool: true,
            show_label: false,
            object_bounds: None,
            block: None,
            theme: default_theme(),
        }
    }

    /// The bed origin is its centre (delta printers) instead of the front
    /// left corner.
    #[must_use]
    pub fn origin_center(mut self, center: bool) -> Self {
        self.bed_origin_center = center;
        self
    }

    /// Zoom to the bed or to the object (default: bed).
    #[must_use]
    pub fn fit(mut self, fit: LayerFit) -> Self {
        self.fit = fit;
        self
    }

    /// Draw the previous layer ghosted underneath (default true).
    #[must_use]
    pub fn show_previous(mut self, show: bool) -> Self {
        self.show_previous = show;
        self
    }

    /// Draw travel moves (default false).
    #[must_use]
    pub fn show_travel(mut self, show: bool) -> Self {
        self.show_travel = show;
        self
    }

    /// Mark the tool position at the progress line (default true).
    #[must_use]
    pub fn show_tool(mut self, show: bool) -> Self {
        self.show_tool = show;
        self
    }

    /// Print the layer number and Z in the top-left corner (default false).
    #[must_use]
    pub fn show_label(mut self, show: bool) -> Self {
        self.show_label = show;
        self
    }

    /// Bounds used by [`LayerFit::Object`] (default: [`object_bounds`] of
    /// the toolpath, computed on every render; pass them precomputed for
    /// big toolpaths).
    #[must_use]
    pub fn object_bounds(mut self, bounds: Bounds) -> Self {
        self.object_bounds = Some(bounds);
        self
    }

    /// Surround the view with a block.
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Draw with this theme.
    #[must_use]
    pub fn theme(mut self, theme: &'a Theme) -> Self {
        self.theme = theme;
        self
    }

    /// The layer actually shown (the requested one, clamped), or `None`
    /// when the toolpath has no layers.
    pub fn shown_layer(&self) -> Option<usize> {
        let count = self.toolpath.layers.len();
        (count > 0).then(|| self.layer.min(count - 1))
    }

    /// Segments of a layer (empty for bad indices or ranges).
    fn segments(&self, layer: usize) -> &'a [Segment] {
        let toolpath = self.toolpath;
        toolpath
            .layers
            .get(layer)
            .and_then(|l| toolpath.segments.get(l.segments.clone()))
            .unwrap_or(&[])
    }

    /// The bed rectangle in millimetres, if the size is usable.
    fn bed_rect(&self) -> Option<View> {
        let [w, h] = self.bed_size.map(f64::from);
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return None;
        }
        let (x0, y0) = if self.bed_origin_center {
            (-w / 2.0, -h / 2.0)
        } else {
            (0.0, 0.0)
        };
        Some(View {
            x: [x0, x0 + w],
            y: [y0, y0 + h],
        })
    }

    /// XY extents of the object: the given bounds, else [`object_bounds`]
    /// of the toolpath, else the bounds of all moves.
    fn object_rect(&self) -> Option<View> {
        let usable = |b: &Bounds| {
            !b.is_empty() && (0..2).all(|i| b.min[i].is_finite() && b.max[i].is_finite())
        };
        let b = self
            .object_bounds
            .filter(usable)
            .or_else(|| Some(object_bounds(self.toolpath)).filter(usable))
            .or_else(|| Some(self.toolpath.travel_bounds).filter(usable))?;
        Some(View {
            x: [f64::from(b.min[0]), f64::from(b.max[0])],
            y: [f64::from(b.min[1]), f64::from(b.max[1])],
        })
    }

    /// The visible region for a canvas of `cols` x `rows` cells: the fitted
    /// rectangle plus a margin, widened so a millimetre is as wide as it is
    /// tall on screen.
    fn view(&self, cols: u16, rows: u16) -> View {
        let rect = match self.fit {
            LayerFit::Bed => self.bed_rect().or_else(|| self.object_rect()),
            LayerFit::Object => self.object_rect().or_else(|| self.bed_rect()),
        }
        .unwrap_or(View {
            x: [0.0, 220.0],
            y: [0.0, 220.0],
        });
        let margin = |lo: f64, hi: f64| ((hi - lo) * 0.03).max(1.0);
        let mx = margin(rect.x[0], rect.x[1]);
        let my = margin(rect.y[0], rect.y[1]);
        let (x0, x1) = (rect.x[0] - mx, rect.x[1] + mx);
        let (y0, y1) = (rect.y[0] - my, rect.y[1] + my);
        // Braille: 2 dots per column, 4 per row; dots are about square.
        let dots_x = f64::from(cols.max(1)) * 2.0;
        let dots_y = f64::from(rows.max(1)) * 4.0;
        let scale = ((x1 - x0) / dots_x).max((y1 - y0) / dots_y);
        let half_w = scale * dots_x / 2.0;
        let half_h = scale * dots_y / 2.0;
        let cx = (x0 + x1) / 2.0;
        let cy = (y0 + y1) / 2.0;
        View {
            x: [cx - half_w, cx + half_w],
            y: [cy - half_h, cy + half_h],
        }
    }

    /// "Layer 12/100  Z 2.40 mm".
    pub fn label_text(&self) -> String {
        match self.shown_layer() {
            Some(i) => {
                let z = self.toolpath.layers.get(i).map_or(f32::NAN, |l| l.z);
                format!(
                    "Layer {}/{}  Z {} mm",
                    i + 1,
                    self.toolpath.layers.len(),
                    format::trimmed(f64::from(z), 2)
                )
            }
            None => "No layers".to_string(),
        }
    }

    /// Where the tool is at the progress line (XY, millimetres).
    fn tool_position(&self) -> Option<(f64, f64)> {
        let line = self.progress_line?;
        let index = self.toolpath.segment_at_line(line)?;
        let s = self.toolpath.segments.get(index)?;
        let (x, y) = (f64::from(s.end[0]), f64::from(s.end[1]));
        (x.is_finite() && y.is_finite()).then_some((x, y))
    }

    fn render_message(&self, area: Rect, buf: &mut Buffer, text: &str) {
        if area.is_empty() {
            return;
        }
        let y = area.y + area.height / 2;
        Paragraph::new(Line::from(Span::styled(text.to_string(), self.theme.dim())))
            .alignment(Alignment::Center)
            .render(Rect::new(area.x, y, area.width, 1), buf);
    }
}

/// Draw segments as lines; `color` picks each segment's color (`None`
/// skips it).
fn draw_segments(
    ctx: &mut Context,
    segments: &[Segment],
    color: impl Fn(&Segment) -> Option<Color>,
) {
    for s in segments {
        let Some(c) = color(s) else { continue };
        let [x1, y1, _] = s.start.map(f64::from);
        let [x2, y2, _] = s.end.map(f64::from);
        if !(x1.is_finite() && y1.is_finite() && x2.is_finite() && y2.is_finite()) {
            continue;
        }
        ctx.draw(&CanvasLine::new(x1, y1, x2, y2, c));
    }
}

impl Widget for LayerCanvas<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = match &self.block {
            Some(block) => {
                block.clone().render(area, buf);
                block.inner(area)
            }
            None => area,
        };
        if inner.is_empty() {
            return;
        }
        let Some(layer) = self.shown_layer() else {
            self.render_message(inner, buf, "No toolpath loaded");
            return;
        };
        let theme = self.theme;
        let view = self.view(inner.width, inner.height);
        let current = self.segments(layer);
        let previous = if self.show_previous && layer > 0 {
            self.segments(layer - 1)
        } else {
            &[]
        };
        let progress = self.progress_line;
        let printed = |s: &Segment| progress.is_none_or(|p| s.line <= p);
        let bed = self.bed_rect();
        let tool = if self.show_tool {
            self.tool_position()
        } else {
            None
        };
        // Without colors, ghosted parts are told apart by a coarser marker.
        let ghost_marker = if theme.has_color() {
            Marker::Braille
        } else {
            Marker::Dot
        };
        let tool_style: Style = theme.accent();
        let canvas = Canvas::default()
            // The canvas clears its area; keep the screen background.
            .background_color(theme.background)
            .marker(Marker::Braille)
            .x_bounds(view.x)
            .y_bounds(view.y)
            .paint(|ctx| {
                if let Some(bed) = bed {
                    ctx.draw(&Rectangle::new(
                        bed.x[0],
                        bed.y[0],
                        bed.x[1] - bed.x[0],
                        bed.y[1] - bed.y[0],
                        theme.border,
                    ));
                }
                ctx.marker(ghost_marker);
                draw_segments(ctx, previous, |s| s.is_extrusion().then_some(theme.travel));
                if self.show_travel {
                    ctx.marker(Marker::Braille);
                    draw_segments(ctx, current, |s| {
                        (!s.is_extrusion()).then_some(theme.travel)
                    });
                }
                ctx.marker(ghost_marker);
                draw_segments(ctx, current, |s| {
                    (s.is_extrusion() && !printed(s)).then(|| theme.feature_dim(s.feature))
                });
                ctx.marker(Marker::Braille);
                draw_segments(ctx, current, |s| {
                    (s.is_extrusion() && printed(s)).then(|| theme.feature(s.feature))
                });
            });
        canvas.render(inner, buf);
        if let Some((x, y)) = tool
            && let Some((cx, cy)) = view.cell_of(inner, x, y)
        {
            buf.set_string(cx, cy, TOOL_MARKER, tool_style);
        }
        if self.show_label {
            let label = self.label_text();
            buf.set_line(
                inner.x,
                inner.y,
                &Line::from(Span::styled(label, theme.dim())),
                inner.width,
            );
        }
    }
}

/// A legend of the features present on one layer (or the whole toolpath):
/// colored squares followed by feature names, wrapped over the rows.
#[derive(Clone, Debug)]
pub struct FeatureLegend<'a> {
    toolpath: &'a Toolpath,
    layer: Option<usize>,
    theme: &'a Theme,
}

impl<'a> FeatureLegend<'a> {
    /// Features of `layer` (or of the whole toolpath when `None`).
    pub fn new(toolpath: &'a Toolpath, layer: Option<usize>) -> FeatureLegend<'a> {
        FeatureLegend {
            toolpath,
            layer,
            theme: default_theme(),
        }
    }

    /// Draw with this theme.
    #[must_use]
    pub fn theme(mut self, theme: &'a Theme) -> Self {
        self.theme = theme;
        self
    }

    /// The features shown, in display order. For the whole toolpath they
    /// come from its feature statistics (no pass over millions of
    /// segments on every frame), else from the segments.
    pub fn features(&self) -> Vec<Feature> {
        let mut present = [false; Feature::COUNT];
        let mut mark = |feature: Feature| {
            if let Some(p) = present.get_mut(feature.index()) {
                *p = true;
            }
        };
        let stats = &self.toolpath.stats.features;
        if self.layer.is_none() && !stats.is_empty() {
            stats.iter().for_each(|s| mark(s.feature));
        } else {
            let segments: &[Segment] = match self.layer {
                Some(i) => self
                    .toolpath
                    .layers
                    .get(i)
                    .and_then(|l| self.toolpath.segments.get(l.segments.clone()))
                    .unwrap_or(&[]),
                None => &self.toolpath.segments,
            };
            segments
                .iter()
                .filter(|s| s.is_extrusion())
                .for_each(|s| mark(s.feature));
        }
        Feature::ALL
            .into_iter()
            .filter(|f| present.get(f.index()).copied().unwrap_or(false))
            .collect()
    }
}

impl Widget for FeatureLegend<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let width = usize::from(area.width);
        let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
        let mut used = 0;
        for feature in self.features() {
            let name = feature.name();
            let cols = 2 + name.chars().count();
            let sep = if used == 0 { 0 } else { 2 };
            if used > 0 && used + sep + cols > width {
                rows.push(Vec::new());
                used = 0;
            }
            let Some(row) = rows.last_mut() else { break };
            if used > 0 {
                row.push(Span::raw("  "));
                used += 2;
            }
            row.push(Span::styled(
                "■ ",
                self.theme.fg(self.theme.feature(feature)),
            ));
            row.push(Span::styled(name, self.theme.text()));
            used += cols;
        }
        for (i, spans) in rows.into_iter().enumerate() {
            let Ok(dy) = u16::try_from(i) else { break };
            if dy >= area.height {
                break;
            }
            buf.set_line(area.x, area.y + dy, &Line::from(spans), area.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testutil::{lines, render};
    use sprint_gcode::{AnalyzeOptions, GcodeFile, analyze_simple};

    /// Two layers: a 20 mm square perimeter and infill line at Z 0.2, a
    /// square at Z 0.4.
    fn two_layers() -> Toolpath {
        let text = "\
G21\nG90\nM83\nG28\n\
G1 Z0.2 F600\nG1 X100 Y100 F6000\n\
;TYPE:External perimeter\n\
G1 X120 Y100 E1 F1200\nG1 X120 Y120 E1\nG1 X100 Y120 E1\nG1 X100 Y100 E1\n\
;TYPE:Internal infill\n\
G1 X120 Y120 E1.4\n\
G1 Z0.4\n\
;TYPE:External perimeter\n\
G1 X120 Y100 E1\nG1 X120 Y120 E1\nG1 X100 Y120 E1\nG1 X100 Y100 E1\n";
        analyze_simple(
            &GcodeFile::from_text("t.gcode", text),
            &AnalyzeOptions::default(),
        )
    }

    fn braille_cells(buf: &Buffer) -> Vec<(u16, u16, Color)> {
        let mut out = Vec::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell
                    .symbol()
                    .chars()
                    .any(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
                {
                    out.push((x, y, cell.fg));
                }
            }
        }
        out
    }

    #[test]
    fn empty_toolpath_shows_message() {
        let tp = Toolpath::default();
        let canvas = LayerCanvas::new(&tp, 3, None, [220.0, 220.0]);
        assert_eq!(canvas.shown_layer(), None);
        assert_eq!(canvas.label_text(), "No layers");
        let buf = render(canvas, 30, 5);
        assert!(lines(&buf)[2].contains("No toolpath loaded"));
    }

    #[test]
    fn draws_layer_in_feature_colors() {
        let tp = two_layers();
        assert_eq!(tp.layers.len(), 2);
        let theme = Theme::dark();
        let canvas = LayerCanvas::new(&tp, 0, None, [220.0, 220.0])
            .fit(LayerFit::Object)
            .theme(&theme);
        let buf = render(canvas, 40, 20);
        let cells = braille_cells(&buf);
        assert!(cells.len() > 20, "{}", lines(&buf).join("\n"));
        let ext = theme.feature(Feature::ExternalPerimeter);
        let infill = theme.feature(Feature::InternalInfill);
        assert!(cells.iter().any(|c| c.2 == ext));
        assert!(cells.iter().any(|c| c.2 == infill));
        // Nothing is ghosted without progress.
        assert!(
            !cells
                .iter()
                .any(|c| c.2 == theme.feature_dim(Feature::ExternalPerimeter))
        );
    }

    #[test]
    fn progress_splits_bright_and_dim_and_marks_the_tool() {
        let tp = two_layers();
        let theme = Theme::dark();
        // Progress after the first two perimeter lines of layer 0 (lines
        // 7 and 8 are the first two extrusions).
        let canvas = LayerCanvas::new(&tp, 0, Some(8), [220.0, 220.0])
            .fit(LayerFit::Object)
            .theme(&theme);
        let buf = render(canvas, 40, 20);
        let cells = braille_cells(&buf);
        let ext = theme.feature(Feature::ExternalPerimeter);
        let ext_dim = theme.feature_dim(Feature::ExternalPerimeter);
        assert!(cells.iter().any(|c| c.2 == ext));
        assert!(cells.iter().any(|c| c.2 == ext_dim));
        let text = lines(&buf).join("\n");
        assert_eq!(text.matches(TOOL_MARKER).count(), 1, "{text}");
        // The tool sits at (120, 120): the top-right corner of the square.
        let (mx, my) = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .find(|&(x, y)| buf[(x, y)].symbol() == TOOL_MARKER)
            .expect("marker");
        assert!(mx > 20 && my < 10, "marker at {mx},{my}");
        assert_eq!(buf[(mx, my)].fg, theme.accent);
    }

    #[test]
    fn previous_layer_is_ghosted() {
        let tp = two_layers();
        let theme = Theme::dark();
        let with = render(
            LayerCanvas::new(&tp, 1, None, [220.0, 220.0])
                .fit(LayerFit::Object)
                .theme(&theme),
            40,
            20,
        );
        // The infill diagonal exists only on layer 0, so with the ghost
        // there are travel-colored cells inside the square.
        assert!(braille_cells(&with).iter().any(|c| c.2 == theme.travel));
        let without = render(
            LayerCanvas::new(&tp, 1, None, [220.0, 220.0])
                .fit(LayerFit::Object)
                .show_previous(false)
                .theme(&theme),
            40,
            20,
        );
        assert!(!braille_cells(&without).iter().any(|c| c.2 == theme.travel));
    }

    #[test]
    fn bed_fit_draws_outline_and_keeps_aspect() {
        let tp = two_layers();
        let canvas = LayerCanvas::new(&tp, 0, None, [200.0, 100.0]);
        let view = canvas.view(40, 10);
        // 40 cols x 10 rows = 80 x 40 dots: width must be twice the height.
        let w = view.x[1] - view.x[0];
        let h = view.y[1] - view.y[0];
        assert!((w / h - 2.0).abs() < 1e-9, "{w} x {h}");
        assert!(view.x[0] < 0.0 && view.x[1] > 200.0);
        let buf = render(canvas, 40, 10);
        assert!(!braille_cells(&buf).is_empty());
        // Centre origin shifts the bed.
        let centered = LayerCanvas::new(&tp, 0, None, [200.0, 200.0]).origin_center(true);
        let v = centered.view(20, 10);
        assert!(v.x[0] < -100.0 && v.x[1] > 100.0);
    }

    #[test]
    fn bad_bed_size_falls_back_to_object() {
        let tp = two_layers();
        let canvas = LayerCanvas::new(&tp, 0, None, [f32::NAN, -1.0]);
        let v = canvas.view(40, 20);
        assert!(v.x[0] > 90.0 && v.x[1] < 130.0, "{v:?}");
        // Object fit with no bounds at all falls back to the bed.
        let empty = Toolpath::default();
        let canvas = LayerCanvas::new(&empty, 0, None, [300.0, 300.0]).fit(LayerFit::Object);
        let v = canvas.view(40, 20);
        assert!(v.x[1] > 300.0);
    }

    #[test]
    fn out_of_range_layer_is_clamped_and_labelled() {
        let tp = two_layers();
        let canvas = LayerCanvas::new(&tp, 99, None, [220.0, 220.0]).show_label(true);
        assert_eq!(canvas.shown_layer(), Some(1));
        assert_eq!(canvas.label_text(), "Layer 2/2  Z 0.4 mm");
        let buf = render(canvas, 40, 10);
        assert!(lines(&buf)[0].starts_with("Layer 2/2  Z 0.4 mm"));
    }

    #[test]
    fn corrupt_ranges_and_values_do_not_panic() {
        let mut tp = two_layers();
        tp.layers[0].segments = 5..10_000;
        tp.segments[0].start = [f32::NAN, 0.0, 0.0];
        tp.bounds.min = [f32::NEG_INFINITY, 0.0, 0.0];
        for (w, h) in [(1, 1), (2, 1), (40, 20)] {
            let _ = render(
                LayerCanvas::new(&tp, 0, Some(u32::MAX), [220.0, 220.0]),
                w,
                h,
            );
            let _ = render(
                LayerCanvas::new(&tp, 1, Some(0), [0.0, 0.0])
                    .fit(LayerFit::Object)
                    .show_travel(true),
                w,
                h,
            );
        }
    }

    #[test]
    fn monochrome_still_distinguishes_ghosts() {
        let tp = two_layers();
        let theme = Theme::monochrome();
        let buf = render(
            LayerCanvas::new(&tp, 1, None, [220.0, 220.0])
                .fit(LayerFit::Object)
                .theme(&theme),
            40,
            20,
        );
        let text = lines(&buf).join("\n");
        // Ghosted previous layer uses the dot marker.
        assert!(text.contains('•'), "{text}");
        assert!(text.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
    }

    #[test]
    fn object_bounds_skip_custom_gcode() {
        let text = "\
M83\n;TYPE:Custom\nG1 Z0.3 F600\nG1 X0 Y-3\nG1 X60 Y-3 E9 F1000\n\
;TYPE:Perimeter\nG1 X100 Y100\nG1 X110 Y100 E1\nG1 X110 Y110 E1\n";
        let tp = analyze_simple(
            &GcodeFile::from_text("p.gcode", text),
            &AnalyzeOptions::default(),
        );
        let b = object_bounds(&tp);
        assert_eq!(b.min[0], 100.0);
        assert_eq!(b.max[1], 110.0);
        assert!(
            tp.bounds.min[1] < 0.0,
            "the purge line is in the full bounds"
        );
        // Explicit bounds win.
        let mut given = Bounds::empty();
        given.include([0.0, 0.0, 0.0]);
        given.include([10.0, 10.0, 0.0]);
        let v = LayerCanvas::new(&tp, 0, None, [220.0, 220.0])
            .fit(LayerFit::Object)
            .object_bounds(given)
            .view(20, 10);
        assert!(v.x[0] < 0.0 && v.x[1] < 30.0, "{v:?}");
        // Only custom extrusions: the full bounds.
        let text = ";TYPE:Custom\nM83\nG1 Z0.3\nG1 X10 Y10 E1 F600\n";
        let tp = analyze_simple(
            &GcodeFile::from_text("c.gcode", text),
            &AnalyzeOptions::default(),
        );
        assert_eq!(object_bounds(&tp), tp.bounds);
        assert!(object_bounds(&Toolpath::default()).is_empty());
    }

    #[test]
    fn whole_toolpath_legend_reads_the_feature_statistics() {
        let mut tp = two_layers();
        // The statistics answer without a pass over the segments.
        tp.segments.clear();
        assert_eq!(
            FeatureLegend::new(&tp, None).features(),
            vec![Feature::ExternalPerimeter, Feature::InternalInfill]
        );
        // Without statistics (a hand-built toolpath) the segments count.
        let mut tp = two_layers();
        tp.stats.features.clear();
        assert_eq!(
            FeatureLegend::new(&tp, None).features(),
            vec![Feature::ExternalPerimeter, Feature::InternalInfill]
        );
    }

    #[test]
    fn legend_lists_features() {
        let tp = two_layers();
        let legend = FeatureLegend::new(&tp, Some(0));
        assert_eq!(
            legend.features(),
            vec![Feature::ExternalPerimeter, Feature::InternalInfill]
        );
        assert_eq!(
            FeatureLegend::new(&tp, Some(1)).features(),
            vec![Feature::ExternalPerimeter]
        );
        assert!(FeatureLegend::new(&tp, Some(7)).features().is_empty());
        let buf = render(FeatureLegend::new(&tp, None), 50, 2);
        assert_eq!(
            lines(&buf)[0].trim_end(),
            "■ External perimeter  ■ Internal infill"
        );
        // Narrow: wraps onto the next row.
        let buf = render(FeatureLegend::new(&tp, None), 24, 2);
        assert_eq!(lines(&buf)[0].trim_end(), "■ External perimeter");
        assert_eq!(lines(&buf)[1].trim_end(), "■ Internal infill");
    }
}
