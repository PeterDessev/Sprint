//! Toolpath statistics: a summary of [`Stats`] and [`SlicerMetadata`] as
//! label/value rows plus a per-feature time table, drawn by
//! [`StatsPanel`] and reused by the command-line report.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};
use sprint_gcode::{Bounds, Feature, SlicerMetadata, Stats, ThumbnailFormat, Toolpath};

use crate::format::{self, MISSING};
use crate::theme::{Theme, default_theme};
use crate::widgets::layer_canvas::object_bounds;
use crate::widgets::util::{self, sanitize};

/// Most object names listed in the summary.
const MAX_OBJECT_NAMES: usize = 5;

/// One line of the summary: a label, a value and an optional note (for
/// example the slicer's figure next to Sprint's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatRow {
    /// What the row is about ("Print time").
    pub label: &'static str,
    /// The main value ("1h 02m 03s").
    pub value: String,
    /// Secondary information ("slicer 58m 01s (+6.9%)").
    pub note: Option<String>,
}

fn row(label: &'static str, value: impl Into<String>, note: Option<String>) -> StatRow {
    StatRow {
        label,
        value: value.into(),
        note,
    }
}

/// Text from a file made safe for a terminal (no control characters).
fn clean(text: &str) -> String {
    sanitize(text.trim()).into_owned()
}

/// A positive finite value.
fn positive(v: Option<f32>) -> Option<f64> {
    v.map(f64::from).filter(|v| v.is_finite() && *v > 0.0)
}

/// Millimetres with `decimals` decimals and the unit.
fn mm(v: f64, decimals: usize) -> String {
    format!("{} mm", format::fixed(v, decimals))
}

/// A speed in whole mm/s.
fn speed(v: f32) -> String {
    format::fixed(f64::from(v), 0)
}

/// "210 °C" style temperature list.
fn temps(values: &[f32]) -> String {
    let list: Vec<String> = values
        .iter()
        .map(|t| format::trimmed(f64::from(*t), 1))
        .collect();
    format!("{} °C", list.join(", "))
}

/// Distinct positive values in first-seen order.
fn distinct_positive(values: impl IntoIterator<Item = f32>) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for v in values {
        if v.is_finite() && v > 0.0 && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

fn thumbnail_format(format: ThumbnailFormat) -> &'static str {
    match format {
        ThumbnailFormat::Png => "PNG",
        ThumbnailFormat::Jpeg => "JPEG",
        ThumbnailFormat::Qoi => "QOI",
    }
}

/// Bounds of the printed part itself: extrusions except start/end G-code
/// (purge lines), skirt/brim and wipe tower. Falls back to
/// [`object_bounds`] when that leaves nothing.
pub fn part_bounds(toolpath: &Toolpath) -> Bounds {
    let mut bounds = Bounds::empty();
    let excluded = [Feature::Custom, Feature::Skirt, Feature::WipeTower];
    for s in toolpath
        .segments
        .iter()
        .filter(|s| s.is_extrusion() && !excluded.contains(&s.feature))
    {
        if s.start.iter().chain(&s.end).all(|v| v.is_finite()) {
            bounds.include(s.start);
            bounds.include(s.end);
        }
    }
    if bounds.is_empty() {
        object_bounds(toolpath)
    } else {
        bounds
    }
}

/// The summary rows for a toolpath's statistics and slicer metadata, in
/// display order. `bounds` adds size and bounds rows: pass
/// [`part_bounds`] for the size of the part (the extrusion bounds
/// include purge lines and skirts). The height is the top Z for parts
/// that start on the bed. Text from the file is sanitized; missing data
/// gives short placeholders or leaves rows out.
pub fn summary_rows(
    stats: &Stats,
    metadata: &SlicerMetadata,
    bounds: Option<&Bounds>,
) -> Vec<StatRow> {
    let mut rows = Vec::new();

    let slicer = metadata
        .slicer
        .as_deref()
        .map(clean)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let flavor = metadata
        .flavor
        .as_deref()
        .map(clean)
        .filter(|f| !f.is_empty())
        .map(|f| format!("flavor {f}"));
    rows.push(row("Slicer", slicer, flavor));

    let ours = f64::from(stats.estimated_time);
    let note = positive(metadata.estimated_time).map(|theirs| {
        format!(
            "slicer {} (Sprint {})",
            format::duration(theirs),
            format::relative_difference(ours, theirs)
        )
    });
    rows.push(row("Print time", format::duration(ours), note));

    let filament = format!(
        "{}, {} cm³, {} g",
        format::length_mm(f64::from(stats.filament_length)),
        format::fixed(f64::from(stats.filament_volume) / 1000.0, 1),
        format::fixed(f64::from(stats.filament_weight), 1),
    );
    let mut slicer_filament = Vec::new();
    if let Some(len) = positive(metadata.filament_used_mm) {
        slicer_filament.push(format::length_mm(len));
    }
    if let Some(g) = positive(metadata.filament_used_g) {
        slicer_filament.push(format!("{} g", format::fixed(g, 1)));
    }
    let note =
        (!slicer_filament.is_empty()).then(|| format!("slicer {}", slicer_filament.join(", ")));
    rows.push(row("Filament", filament, note));

    let mut material = Vec::new();
    if let Some(kind) = metadata
        .filament_type
        .as_deref()
        .map(clean)
        .filter(|k| !k.is_empty())
    {
        material.push(kind);
    }
    if let Some(d) = positive(metadata.filament_diameter) {
        material.push(format!("{} mm filament", format::trimmed(d, 2)));
    }
    if let Some(density) = positive(metadata.filament_density) {
        material.push(format!("{} g/cm³", format::trimmed(density, 2)));
    }
    if let Some(nozzle) = positive(metadata.nozzle_diameter) {
        material.push(format!("{} mm nozzle", format::trimmed(nozzle, 2)));
    }
    if !material.is_empty() {
        rows.push(row("Material", material.join(", "), None));
    }

    if let Some(b) =
        bounds.filter(|b| !b.is_empty() && b.min.iter().chain(&b.max).all(|v| v.is_finite()))
    {
        let mut size = b.size().map(f64::from);
        // A part printed on the bed is as tall as its top layer, not the
        // distance from the first layer's Z to the top.
        if b.min[2] >= 0.0 {
            size[2] = f64::from(b.max[2]);
        }
        rows.push(row(
            "Size",
            format!(
                "{} x {} x {} mm",
                format::fixed(size[0], 1),
                format::fixed(size[1], 1),
                format::fixed(size[2], 1)
            ),
            None,
        ));
        let axis = |name: &str, i: usize| {
            format!(
                "{name} {}..{}",
                format::fixed(f64::from(b.min[i]), 1),
                format::fixed(f64::from(b.max[i]), 1)
            )
        };
        rows.push(row(
            "Bounds",
            format!("{}, {}, {}", axis("X", 0), axis("Y", 1), axis("Z", 2)),
            None,
        ));
    }

    let mut layers = format::count(stats.layer_count);
    if stats.layer_count > 0 {
        if stats.layer_height.is_finite() && stats.layer_height > 0.0 {
            layers.push_str(&format!(", {}", mm(f64::from(stats.layer_height), 2)));
        }
        if stats.first_layer_height.is_finite()
            && stats.first_layer_height > 0.0
            && stats.first_layer_height != stats.layer_height
        {
            layers.push_str(&format!(
                " (first {})",
                mm(f64::from(stats.first_layer_height), 2)
            ));
        }
    }
    let note = metadata
        .layer_count
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n != stats.layer_count)
        .map(|n| format!("slicer {}", format::count(n)));
    rows.push(row("Layers", layers, note));

    if stats.extrude_moves > 0 {
        rows.push(row(
            "Print speed",
            format!(
                "{}-{} mm/s, avg {} mm/s",
                speed(stats.min_print_speed),
                speed(stats.max_print_speed),
                speed(stats.avg_print_speed)
            ),
            None,
        ));
    }
    if stats.travel_moves > 0 {
        rows.push(row(
            "Travel",
            format!(
                "{}, {}, up to {} mm/s",
                format::length_mm(f64::from(stats.travel_distance)),
                format::duration(f64::from(stats.travel_time)),
                speed(stats.max_travel_speed)
            ),
            None,
        ));
    }
    rows.push(row(
        "Moves",
        format!(
            "{} extrusions, {} travels, {} retractions",
            format::count(stats.extrude_moves),
            format::count(stats.travel_moves),
            format::count(stats.retractions)
        ),
        None,
    ));

    let tools = if stats.tools_used.is_empty() {
        "none".to_string()
    } else {
        stats
            .tools_used
            .iter()
            .map(|&t| {
                let used = stats.filament_per_tool.get(usize::from(t)).copied();
                match used.filter(|_| stats.tools_used.len() > 1) {
                    Some(mm) => format!("T{t} {}", format::length_mm(f64::from(mm))),
                    None => format!("T{t}"),
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let note = (stats.tool_changes > 0).then(|| {
        let n = stats.tool_changes;
        format!(
            "{} tool change{}",
            format::count(n),
            if n == 1 { "" } else { "s" }
        )
    });
    rows.push(row("Tools", tools, note));

    let mut tool_ids: Vec<u8> = Vec::new();
    for (tool, _) in &stats.hotend_temps {
        if !tool_ids.contains(tool) {
            tool_ids.push(*tool);
        }
    }
    let per_tool: Vec<(u8, Vec<f32>)> = tool_ids
        .iter()
        .map(|&tool| {
            let values = stats
                .hotend_temps
                .iter()
                .filter(|(t, _)| *t == tool)
                .map(|(_, c)| *c);
            (tool, distinct_positive(values))
        })
        .filter(|(_, v)| !v.is_empty())
        .collect();
    let hotend = match per_tool.as_slice() {
        [] => positive(metadata.hotend_temp).map(|t| {
            (
                format!("{} °C", format::trimmed(t, 1)),
                Some("from slicer settings".to_string()),
            )
        }),
        [(_, values)] => Some((temps(values), None)),
        many => Some((
            many.iter()
                .map(|(tool, values)| format!("T{tool} {}", temps(values)))
                .collect::<Vec<_>>()
                .join(", "),
            None,
        )),
    };
    if let Some((value, note)) = hotend {
        rows.push(row("Hotend", value, note));
    }
    let bed = distinct_positive(stats.bed_temps.iter().copied());
    if !bed.is_empty() {
        rows.push(row("Bed", temps(&bed), None));
    } else if let Some(t) = positive(metadata.bed_temp) {
        rows.push(row(
            "Bed",
            format!("{} °C", format::trimmed(t, 1)),
            Some("from slicer settings".to_string()),
        ));
    }

    let thumbs = &metadata.thumbnails;
    let value = if thumbs.is_empty() {
        "none".to_string()
    } else {
        let list: Vec<String> = thumbs
            .iter()
            .map(|t| format!("{}x{} {}", t.width, t.height, thumbnail_format(t.format)))
            .collect();
        format!("{}: {}", thumbs.len(), list.join(", "))
    };
    rows.push(row("Thumbnails", value, None));

    if !metadata.object_names.is_empty() {
        let names: Vec<String> = metadata
            .object_names
            .iter()
            .take(MAX_OBJECT_NAMES)
            .map(|n| clean(n))
            .collect();
        let more = metadata.object_names.len() > MAX_OBJECT_NAMES;
        rows.push(row(
            "Objects",
            format!(
                "{}: {}{}",
                format::count(metadata.object_names.len()),
                names.join(", "),
                if more { ", …" } else { "" }
            ),
            None,
        ));
    }

    let mut flags = vec![if stats.uses_relative_e {
        "relative E".to_string()
    } else {
        "absolute E".to_string()
    }];
    if stats.uses_arcs {
        flags.push("arcs (G2/G3)".to_string());
    }
    if stats.uses_firmware_retraction {
        flags.push("firmware retraction (G10/G11)".to_string());
    }
    if stats.heat_waits > 0 {
        let n = stats.heat_waits;
        flags.push(format!(
            "{} heat wait{}",
            format::count(n),
            if n == 1 { "" } else { "s" }
        ));
    }
    rows.push(row("Flags", flags.join(", "), None));
    rows
}

/// One row of the per-feature table.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureRow {
    /// Display name ("External perimeter", "Travel", "Other").
    pub name: &'static str,
    /// The feature, or `None` for the travel and other-time rows.
    pub feature: Option<Feature>,
    /// Estimated seconds.
    pub time: f32,
    /// Share of the estimated print time, `0..=1` (0 when unknown).
    pub share: f32,
    /// Filament used, mm.
    pub filament_mm: f32,
    /// Path length, mm.
    pub distance_mm: f32,
}

/// The per-feature table: features by descending time, then travel and
/// (when there is any) other time — E-only moves, firmware retraction and
/// dwells. Shares are of the estimated print time.
pub fn feature_rows(stats: &Stats) -> Vec<FeatureRow> {
    let total = stats.estimated_time;
    let share = |t: f32| {
        if total.is_finite() && total > 0.0 && t.is_finite() {
            (t / total).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let mut rows: Vec<FeatureRow> = stats
        .features
        .iter()
        .map(|f| FeatureRow {
            name: f.feature.name(),
            feature: Some(f.feature),
            time: f.time,
            share: share(f.time),
            filament_mm: f.extruded,
            distance_mm: f.distance,
        })
        .collect();
    rows.sort_by(|a, b| b.time.total_cmp(&a.time));
    if stats.travel_moves > 0 || stats.travel_time > 0.0 {
        rows.push(FeatureRow {
            name: "Travel",
            feature: None,
            time: stats.travel_time,
            share: share(stats.travel_time),
            filament_mm: 0.0,
            distance_mm: stats.travel_distance,
        });
    }
    if stats.other_time.is_finite() && stats.other_time > 0.0 {
        rows.push(FeatureRow {
            name: "Other",
            feature: None,
            time: stats.other_time,
            share: share(stats.other_time),
            filament_mm: 0.0,
            distance_mm: 0.0,
        });
    }
    rows
}

/// Columns of the feature table values: time, share, filament.
const TIME_COLS: usize = 11;
const SHARE_COLS: usize = 7;
const FILAMENT_COLS: usize = 10;
/// Narrowest share bar worth drawing.
const MIN_BAR: usize = 4;

/// A panel with the toolpath statistics: the [`summary_rows`] as aligned
/// label/value lines (notes dimmed after the value when they fit), then
/// the [`feature_rows`] as a table with colored feature markers, time,
/// share, filament and a share bar when there is room.
#[derive(Clone, Debug)]
pub struct StatsPanel<'a> {
    stats: &'a Stats,
    metadata: &'a SlicerMetadata,
    bounds: Option<Bounds>,
    show_features: bool,
    scroll: usize,
    block: Option<Block<'a>>,
    theme: &'a Theme,
}

impl<'a> StatsPanel<'a> {
    /// A panel for these statistics and metadata.
    pub fn new(stats: &'a Stats, metadata: &'a SlicerMetadata) -> StatsPanel<'a> {
        StatsPanel {
            stats,
            metadata,
            bounds: None,
            show_features: true,
            scroll: 0,
            block: None,
            theme: default_theme(),
        }
    }

    /// A panel for a toolpath: its statistics, metadata and
    /// [`part_bounds`] (computed here, a pass over the segments).
    pub fn from_toolpath(toolpath: &'a Toolpath) -> StatsPanel<'a> {
        StatsPanel::new(&toolpath.stats, &toolpath.metadata).bounds(Some(part_bounds(toolpath)))
    }

    /// Bounds for the size and bounds rows (see [`summary_rows`]).
    #[must_use]
    pub fn bounds(mut self, bounds: Option<Bounds>) -> Self {
        self.bounds = bounds;
        self
    }

    /// Show the per-feature table (default true).
    #[must_use]
    pub fn show_features(mut self, show: bool) -> Self {
        self.show_features = show;
        self
    }

    /// Skip this many lines (for scrolling; clamped to the content).
    #[must_use]
    pub fn scroll(mut self, lines: usize) -> Self {
        self.scroll = lines;
        self
    }

    /// Surround the panel with a block.
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

    /// Number of content lines (for scroll limits).
    pub fn line_count(&self) -> usize {
        let summary = summary_rows(self.stats, self.metadata, self.bounds.as_ref()).len();
        let features = feature_rows(self.stats).len();
        if self.show_features && features > 0 {
            summary + 2 + features
        } else {
            summary
        }
    }

    /// Every content line for a width of `width` columns.
    fn content(&self, width: usize) -> Vec<Line<'static>> {
        let theme = self.theme;
        let rows = summary_rows(self.stats, self.metadata, self.bounds.as_ref());
        let label_cols = rows.iter().map(|r| r.label.len()).max().unwrap_or(0) + 2;
        let mut out = Vec::new();
        for r in &rows {
            let value_room = width.saturating_sub(label_cols);
            let value = util::truncate(&r.value, value_room).into_owned();
            let mut spans = vec![
                Span::styled(format!("{:<label_cols$}", r.label), theme.dim()),
                Span::styled(value.clone(), theme.text()),
            ];
            if let Some(note) = &r.note {
                let used = label_cols + util::width(&value);
                let room = width.saturating_sub(used + 2);
                // Only whole notes, or cut ones that still say something.
                if util::width(note) <= room || room >= 12 {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        util::truncate(note, room).into_owned(),
                        theme.dim(),
                    ));
                }
            }
            out.push(Line::from(spans));
        }
        let features = feature_rows(self.stats);
        if !self.show_features || features.is_empty() {
            return out;
        }
        out.push(Line::default());
        let name_cols = features
            .iter()
            .map(|f| f.name.len())
            .max()
            .unwrap_or(0)
            .max("Feature".len());
        let values_cols = TIME_COLS + SHARE_COLS + FILAMENT_COLS;
        let fixed = 2 + name_cols + values_cols;
        let bar_cols = width.saturating_sub(fixed + 2);
        let show_bar = bar_cols >= MIN_BAR;
        out.push(Line::from(Span::styled(
            format!(
                "  {:<name_cols$}{:>TIME_COLS$}{:>SHARE_COLS$}{:>FILAMENT_COLS$}",
                "Feature", "Time", "Share", "Filament"
            ),
            theme.accent(),
        )));
        for f in &features {
            let color = match f.feature {
                Some(feature) => theme.feature(feature),
                None if f.name == "Travel" => theme.travel,
                None => theme.text_dim,
            };
            let marker_style: Style = theme.fg(color);
            let filament = if f.feature.is_some() {
                format::length_mm(f64::from(f.filament_mm))
            } else {
                MISSING.to_string()
            };
            let mut spans = vec![
                Span::styled("■ ", marker_style),
                Span::styled(format!("{:<name_cols$}", f.name), theme.text()),
                Span::styled(
                    format!(
                        "{:>TIME_COLS$}{:>SHARE_COLS$}{:>FILAMENT_COLS$}",
                        format::duration(f64::from(f.time)),
                        format::percent(f64::from(f.share)),
                        filament
                    ),
                    theme.text(),
                ),
            ];
            if show_bar {
                let (filled, empty) = util::bar(f.share, bar_cols, ' ');
                spans.push(Span::raw("  "));
                spans.push(Span::styled(filled, marker_style));
                spans.push(Span::raw(empty));
            }
            out.push(Line::from(spans));
        }
        out
    }
}

impl Widget for StatsPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = match &self.block {
            Some(block) => {
                block.render(area, buf);
                block.inner(area)
            }
            None => area,
        };
        if inner.is_empty() {
            return;
        }
        let lines = self.content(usize::from(inner.width));
        let height = usize::from(inner.height);
        let scroll = self.scroll.min(lines.len().saturating_sub(height));
        for (row, line) in lines.iter().skip(scroll).take(height).enumerate() {
            let Ok(dy) = u16::try_from(row) else { break };
            buf.set_line(inner.x, inner.y + dy, line, inner.width);
        }
    }
}

/// Columns of the widest label in `rows`, for plain-text reports that
/// align values like the panel does.
pub fn label_width(rows: &[StatRow]) -> usize {
    rows.iter().map(|r| r.label.len()).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testutil::{lines, render};
    use sprint_gcode::{AnalyzeOptions, FeatureStat, GcodeFile, Thumbnail, analyze_simple};

    fn stats() -> Stats {
        Stats {
            estimated_time: 3723.0,
            filament_length: 4321.0,
            filament_per_tool: vec![4000.0, 321.0],
            filament_volume: 10_400.0,
            filament_weight: 12.94,
            extrude_moves: 12_345,
            travel_moves: 2_345,
            retractions: 345,
            tool_changes: 3,
            travel_distance: 12_300.0,
            extrude_distance: 50_000.0,
            max_print_speed: 200.0,
            min_print_speed: 15.0,
            avg_print_speed: 61.4,
            max_travel_speed: 250.0,
            layer_count: 100,
            first_layer_height: 0.3,
            layer_height: 0.2,
            features: vec![
                FeatureStat {
                    feature: Feature::Perimeter,
                    time: 600.0,
                    extruded: 1200.0,
                    distance: 9000.0,
                    segments: 10,
                },
                FeatureStat {
                    feature: Feature::ExternalPerimeter,
                    time: 1800.0,
                    extruded: 2000.0,
                    distance: 20000.0,
                    segments: 10,
                },
            ],
            tools_used: vec![0, 1],
            hotend_temps: vec![(0, 215.0), (0, 210.0), (1, 220.0), (0, 0.0)],
            bed_temps: vec![60.0, 0.0],
            uses_relative_e: true,
            uses_arcs: true,
            uses_firmware_retraction: false,
            heat_waits: 2,
            travel_time: 900.0,
            other_time: 423.0,
        }
    }

    fn metadata() -> SlicerMetadata {
        SlicerMetadata {
            slicer: Some("PrusaSlicer 2.7.1\u{1b}[31m".to_string()),
            estimated_time: Some(3600.0),
            filament_used_mm: Some(4300.0),
            filament_used_g: Some(12.8),
            filament_type: Some("PLA".to_string()),
            filament_diameter: Some(1.75),
            nozzle_diameter: Some(0.4),
            layer_count: Some(101),
            flavor: Some("marlin2".to_string()),
            thumbnails: vec![
                Thumbnail {
                    width: 16,
                    height: 16,
                    ..Default::default()
                },
                Thumbnail {
                    width: 220,
                    height: 124,
                    format: ThumbnailFormat::Qoi,
                    ..Default::default()
                },
            ],
            object_names: (1..=7).map(|i| format!("part{i}")).collect(),
            ..Default::default()
        }
    }

    fn value<'r>(rows: &'r [StatRow], label: &str) -> &'r StatRow {
        rows.iter()
            .find(|r| r.label == label)
            .unwrap_or_else(|| panic!("no {label} row in {rows:?}"))
    }

    #[test]
    fn summary_has_every_fact() {
        let mut bounds = Bounds::empty();
        bounds.include([115.0, 95.0, 0.2]);
        bounds.include([135.0, 115.0, 20.0]);
        let rows = summary_rows(&stats(), &metadata(), Some(&bounds));
        let slicer = value(&rows, "Slicer");
        assert_eq!(slicer.value, "PrusaSlicer 2.7.1·[31m");
        assert_eq!(slicer.note.as_deref(), Some("flavor marlin2"));
        let time = value(&rows, "Print time");
        assert_eq!(time.value, "1h 02m 03s");
        assert_eq!(
            time.note.as_deref(),
            Some("slicer 1h 00m 00s (Sprint +3.4%)")
        );
        let filament = value(&rows, "Filament");
        assert_eq!(filament.value, "4.32 m, 10.4 cm³, 12.9 g");
        assert_eq!(filament.note.as_deref(), Some("slicer 4.30 m, 12.8 g"));
        assert_eq!(
            value(&rows, "Material").value,
            "PLA, 1.75 mm filament, 0.4 mm nozzle"
        );
        assert_eq!(value(&rows, "Size").value, "20.0 x 20.0 x 20.0 mm");
        assert_eq!(
            value(&rows, "Bounds").value,
            "X 115.0..135.0, Y 95.0..115.0, Z 0.2..20.0"
        );
        let layers = value(&rows, "Layers");
        assert_eq!(layers.value, "100, 0.20 mm (first 0.30 mm)");
        assert_eq!(layers.note.as_deref(), Some("slicer 101"));
        assert_eq!(
            value(&rows, "Print speed").value,
            "15-200 mm/s, avg 61 mm/s"
        );
        assert_eq!(
            value(&rows, "Travel").value,
            "12.30 m, 15m 00s, up to 250 mm/s"
        );
        assert_eq!(
            value(&rows, "Moves").value,
            "12,345 extrusions, 2,345 travels, 345 retractions"
        );
        let tools = value(&rows, "Tools");
        assert_eq!(tools.value, "T0 4.00 m, T1 321.0 mm");
        assert_eq!(tools.note.as_deref(), Some("3 tool changes"));
        assert_eq!(value(&rows, "Hotend").value, "T0 215, 210 °C, T1 220 °C");
        assert_eq!(value(&rows, "Bed").value, "60 °C");
        assert_eq!(
            value(&rows, "Thumbnails").value,
            "2: 16x16 PNG, 220x124 QOI"
        );
        assert_eq!(
            value(&rows, "Objects").value,
            "7: part1, part2, part3, part4, part5, …"
        );
        assert_eq!(
            value(&rows, "Flags").value,
            "relative E, arcs (G2/G3), 2 heat waits"
        );
    }

    #[test]
    fn summary_of_nothing_uses_placeholders() {
        let rows = summary_rows(&Stats::default(), &SlicerMetadata::default(), None);
        assert_eq!(value(&rows, "Slicer").value, "unknown");
        assert_eq!(value(&rows, "Print time").value, "0s");
        assert_eq!(value(&rows, "Print time").note, None);
        assert_eq!(value(&rows, "Layers").value, "0");
        assert_eq!(value(&rows, "Tools").value, "none");
        assert_eq!(value(&rows, "Thumbnails").value, "none");
        assert_eq!(value(&rows, "Flags").value, "absolute E");
        for absent in [
            "Material",
            "Size",
            "Bounds",
            "Print speed",
            "Travel",
            "Hotend",
            "Bed",
            "Objects",
        ] {
            assert!(rows.iter().all(|r| r.label != absent), "{absent}");
        }
        // Empty bounds add nothing either.
        let rows = summary_rows(
            &Stats::default(),
            &SlicerMetadata::default(),
            Some(&Bounds::empty()),
        );
        assert!(rows.iter().all(|r| r.label != "Size"));
    }

    #[test]
    fn bad_numbers_do_not_leak_nan() {
        let stats = Stats {
            estimated_time: f32::NAN,
            filament_length: f32::INFINITY,
            filament_weight: f32::NAN,
            layer_count: 3,
            layer_height: f32::NAN,
            first_layer_height: -1.0,
            ..Default::default()
        };
        let meta = SlicerMetadata {
            estimated_time: Some(f32::NAN),
            hotend_temp: Some(205.0),
            bed_temp: Some(f32::INFINITY),
            ..Default::default()
        };
        let rows = summary_rows(&stats, &meta, None);
        for r in &rows {
            assert!(
                !r.value.contains("NaN") && !r.value.contains("inf"),
                "{r:?}"
            );
        }
        assert_eq!(value(&rows, "Print time").value, MISSING);
        assert_eq!(value(&rows, "Layers").value, "3");
        let hotend = value(&rows, "Hotend");
        assert_eq!(hotend.value, "205 °C");
        assert_eq!(hotend.note.as_deref(), Some("from slicer settings"));
        assert!(rows.iter().all(|r| r.label != "Bed"));
    }

    #[test]
    fn feature_rows_sorted_with_travel_and_other() {
        let rows = feature_rows(&stats());
        let names: Vec<&str> = rows.iter().map(|r| r.name).collect();
        assert_eq!(
            names,
            ["External perimeter", "Perimeter", "Travel", "Other"]
        );
        assert!((rows[0].share - 1800.0 / 3723.0).abs() < 1e-6);
        assert_eq!(rows[2].feature, None);
        assert_eq!(rows[2].distance_mm, 12_300.0);
        let total: f32 = rows.iter().map(|r| r.share).sum();
        assert!((total - 1.0).abs() < 1e-5, "{total}");
        // Zero total time: no shares, no panics.
        let none = feature_rows(&Stats {
            estimated_time: 0.0,
            ..stats()
        });
        assert!(none.iter().all(|r| r.share == 0.0));
        assert!(feature_rows(&Stats::default()).is_empty());
    }

    #[test]
    fn panel_renders_summary_and_table() {
        let theme = Theme::dark();
        let (stats, meta) = (stats(), metadata());
        let panel = StatsPanel::new(&stats, &meta).theme(&theme);
        assert_eq!(panel.line_count(), 14 + 2 + 4);
        let buf = render(panel, 90, 24);
        let rows = lines(&buf);
        assert!(
            rows[0].starts_with("Slicer       PrusaSlicer 2.7.1·[31m  flavor marlin2"),
            "{rows:?}"
        );
        assert!(
            rows[1].starts_with("Print time   1h 02m 03s  slicer 1h 00m 00s (Sprint +3.4%)"),
            "{rows:?}"
        );
        let header = rows
            .iter()
            .position(|r| r.contains("Feature"))
            .expect("table header");
        assert!(rows[header].contains("Time") && rows[header].contains("Filament"));
        let ext = &rows[header + 1];
        assert!(ext.starts_with("■ External perimeter"), "{ext}");
        assert!(
            ext.contains("30m 00s") && ext.contains("48.3%") && ext.contains("2.00 m"),
            "{ext}"
        );
        assert!(ext.contains('█'), "share bar: {ext}");
        assert!(rows[header + 3].contains("Travel") && rows[header + 3].contains("--"));
        // Feature markers use feature colors.
        let y = u16::try_from(header + 1).unwrap_or(0);
        assert_eq!(buf[(0, y)].fg, theme.feature(Feature::ExternalPerimeter));
        assert_eq!(buf[(0, 0)].fg, theme.text_dim);
    }

    #[test]
    fn narrow_panels_drop_notes_and_bars() {
        let (stats, meta) = (stats(), metadata());
        let buf = render(StatsPanel::new(&stats, &meta), 40, 30);
        let rows = lines(&buf);
        assert!(
            rows[1].starts_with("Print time   1h 02m 03s  slicer"),
            "{rows:?}"
        );
        let header = rows
            .iter()
            .position(|r| r.contains("Feature"))
            .expect("table header");
        assert!(!rows[header + 1].contains('█'));
        let _ = render(StatsPanel::new(&stats, &meta), 12, 3);
        let _ = render(
            StatsPanel::new(&stats, &meta).block(Block::bordered()),
            1,
            1,
        );
    }

    #[test]
    fn scrolling_and_hiding_the_table() {
        let (stats, meta) = (stats(), metadata());
        let buf = render(StatsPanel::new(&stats, &meta).scroll(1), 90, 3);
        assert!(lines(&buf)[0].starts_with("Print time"));
        let panel = StatsPanel::new(&stats, &meta).show_features(false);
        assert_eq!(panel.line_count(), 14);
        let buf = render(panel.scroll(1000), 90, 5);
        assert!(lines(&buf)[4].starts_with("Flags"), "{:?}", lines(&buf));
    }

    #[test]
    fn panel_from_a_real_toolpath() {
        let text = "\
;TYPE:Perimeter\nM83\nM104 S205\nM140 S60\nG1 Z0.2 F600\nG1 X10 Y10\n\
G1 X30 Y10 E1 F1200\nG1 X30 Y30 E1\nG1 Z0.4\nG1 X10 Y30 E1\n";
        let tp = analyze_simple(
            &GcodeFile::from_text("t.gcode", text),
            &AnalyzeOptions::default(),
        );
        let panel = StatsPanel::from_toolpath(&tp);
        let buf = render(panel, 80, 30);
        let text = lines(&buf).join("\n");
        assert!(text.contains("Size         20.0 x 20.0 x 0.4 mm"), "{text}");
        assert!(text.contains("Hotend       205 °C"), "{text}");
        assert!(text.contains("■ Perimeter"), "{text}");
    }

    #[test]
    fn part_bounds_leave_out_skirt_and_purge() {
        let text = "\
M83\n;TYPE:Custom\nG1 Z0.2 F600\nG1 X0 Y-3\nG1 X60 Y-3 E9 F1000\n\
;TYPE:Skirt\nG1 X90 Y90\nG1 X140 Y90 E2\n\
;TYPE:External perimeter\nG1 X100 Y100\nG1 X120 Y100 E1\nG1 X120 Y120 E1\n";
        let tp = analyze_simple(
            &GcodeFile::from_text("p.gcode", text),
            &AnalyzeOptions::default(),
        );
        let part = part_bounds(&tp);
        assert_eq!((part.min[0], part.max[0]), (100.0, 120.0));
        assert_eq!((part.min[1], part.max[1]), (100.0, 120.0));
        // The skirt still counts for the view fit.
        assert_eq!(object_bounds(&tp).max[0], 140.0);
        // Only a skirt: fall back to the object bounds.
        let text = ";TYPE:Skirt\nM83\nG1 Z0.2\nG1 X10 Y10 E1 F600\n";
        let tp = analyze_simple(
            &GcodeFile::from_text("s.gcode", text),
            &AnalyzeOptions::default(),
        );
        assert_eq!(part_bounds(&tp), object_bounds(&tp));
        assert!(part_bounds(&Toolpath::default()).is_empty());
    }

    #[test]
    fn label_width_matches_rows() {
        let rows = summary_rows(&stats(), &metadata(), None);
        assert_eq!(label_width(&rows), "Print speed".len());
        assert_eq!(label_width(&[]), 0);
    }
}
