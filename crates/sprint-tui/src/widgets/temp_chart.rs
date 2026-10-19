//! Temperature history chart.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, LegendPosition, Paragraph, Widget};

use crate::format;
use crate::theme::{Theme, default_theme};
use crate::widgets::heater::HeaterKind;

/// One temperature sample.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TempPoint {
    /// Sample time in seconds on any monotonic clock (for example seconds
    /// since connecting).
    pub t: f64,
    /// (current, target) per hotend; index = tool. A target of 0 is "off".
    pub hotends: Vec<(f32, f32)>,
    /// Bed (current, target), when the printer has one.
    pub bed: Option<(f32, f32)>,
    /// Chamber (current, target), when the printer has one.
    pub chamber: Option<(f32, f32)>,
}

impl TempPoint {
    /// The (current, target) reading of one heater, if present.
    pub fn heater(&self, heater: HeaterKind) -> Option<(f32, f32)> {
        match heater {
            HeaterKind::Hotend(tool) => self.hotends.get(usize::from(tool)).copied(),
            HeaterKind::Bed => self.bed,
            HeaterKind::Chamber => self.chamber,
        }
    }
}

/// Shortest window the chart shows.
const MIN_WINDOW_SECS: f64 = 10.0;
/// Most hotends drawn (the rest are ignored).
const MAX_HOTENDS: usize = 8;

/// Current and target temperatures of every heater over the last minutes,
/// as lines on a Braille chart. Time runs left to right and ends at "now"
/// (the newest sample unless set). Targets are drawn only while set
/// (non-zero); NaN readings leave gaps. The legend shows each heater's
/// latest reading.
#[derive(Clone, Debug)]
pub struct TempChart<'a> {
    points: &'a [TempPoint],
    window: Duration,
    now: Option<f64>,
    show_targets: bool,
    show_legend: bool,
    block: Option<Block<'a>>,
    theme: &'a Theme,
}

/// A line to draw: points plus how to label and color it.
struct Series {
    heater: HeaterKind,
    target: bool,
    /// Contiguous runs of valid points (a gap starts a new run).
    runs: Vec<Vec<(f64, f64)>>,
    latest: Option<f32>,
}

impl<'a> TempChart<'a> {
    /// A chart of `points`, oldest first.
    pub fn new(points: &'a [TempPoint]) -> TempChart<'a> {
        TempChart {
            points,
            window: Duration::from_secs(600),
            now: None,
            show_targets: true,
            show_legend: true,
            block: None,
            theme: default_theme(),
        }
    }

    /// How much history to show (default 10 minutes, at least 10 s).
    #[must_use]
    pub fn window(mut self, window: Duration) -> Self {
        self.window = window;
        self
    }

    /// The time at the right edge (default: the newest sample's time).
    #[must_use]
    pub fn now(mut self, now: f64) -> Self {
        self.now = Some(now);
        self
    }

    /// Whether to draw target lines (default true).
    #[must_use]
    pub fn show_targets(mut self, show: bool) -> Self {
        self.show_targets = show;
        self
    }

    /// Whether to draw the legend with the latest readings (default true;
    /// leave it out when heater gauges sit next to the chart).
    #[must_use]
    pub fn show_legend(mut self, show: bool) -> Self {
        self.show_legend = show;
        self
    }

    /// Surround the chart with a block.
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

    fn window_secs(&self) -> f64 {
        self.window.as_secs_f64().max(MIN_WINDOW_SECS)
    }

    /// The right edge of the time axis.
    fn end_time(&self) -> Option<f64> {
        self.now.filter(|t| t.is_finite()).or_else(|| {
            self.points
                .iter()
                .rev()
                .map(|p| p.t)
                .find(|t| t.is_finite())
        })
    }

    /// Heaters that appear in the samples, in display order.
    fn heaters(points: &[&TempPoint]) -> Vec<HeaterKind> {
        let hotends = points
            .iter()
            .map(|p| p.hotends.len())
            .max()
            .unwrap_or(0)
            .min(MAX_HOTENDS);
        let mut heaters: Vec<HeaterKind> = (0..hotends)
            .filter_map(|i| u8::try_from(i).ok())
            .map(HeaterKind::Hotend)
            .collect();
        if points.iter().any(|p| p.bed.is_some()) {
            heaters.push(HeaterKind::Bed);
        }
        if points.iter().any(|p| p.chamber.is_some()) {
            heaters.push(HeaterKind::Chamber);
        }
        heaters
    }

    /// Collect the current (and target) series of every heater.
    fn series(&self, points: &[&TempPoint], now: f64) -> Vec<Series> {
        let mut out = Vec::new();
        for heater in Self::heaters(points) {
            for target in [false, true] {
                if target && !self.show_targets {
                    continue;
                }
                let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
                let mut run: Vec<(f64, f64)> = Vec::new();
                let mut latest = None;
                for p in points {
                    let value = p.heater(heater).map(|(c, t)| if target { t } else { c });
                    let valid = value.filter(|v| v.is_finite() && (!target || *v > 0.0));
                    match valid {
                        Some(v) => {
                            run.push((p.t - now, f64::from(v)));
                            latest = Some(v);
                        }
                        None => {
                            if !run.is_empty() {
                                runs.push(std::mem::take(&mut run));
                            }
                            if target {
                                latest = None;
                            }
                        }
                    }
                }
                if !run.is_empty() {
                    runs.push(run);
                }
                if !runs.is_empty() {
                    out.push(Series {
                        heater,
                        target,
                        runs,
                        latest,
                    });
                }
            }
        }
        out
    }

    fn legend_name(series: &Series) -> String {
        let label = series.heater.label();
        match (series.target, series.latest) {
            (false, Some(v)) => format!("{label} {}°C", format::temperature(v)),
            (false, None) => format!("{label} --"),
            (true, Some(v)) => format!("{label} target {}", format::trimmed(f64::from(v), 1)),
            (true, None) => format!("{label} target off"),
        }
    }

    fn render_empty(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let y = area.y + area.height / 2;
        Paragraph::new(Line::from(Span::styled(
            "No temperature data yet",
            self.theme.dim(),
        )))
        .alignment(Alignment::Center)
        .render(Rect::new(area.x, y, area.width, 1), buf);
    }
}

/// A round upper bound for the temperature axis.
fn nice_max(max: f64) -> f64 {
    let padded = if max.is_finite() {
        max.max(40.0) + 10.0
    } else {
        50.0
    };
    (padded / 50.0).ceil() * 50.0
}

/// Axis label for a time offset in seconds before now.
fn time_label(secs_before: f64) -> String {
    if secs_before <= 0.0 {
        "now".to_string()
    } else if secs_before >= 120.0 {
        format!("-{}m", format::trimmed(secs_before / 60.0, 1))
    } else {
        format!("-{}s", format::trimmed(secs_before, 0))
    }
}

impl Widget for TempChart<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = match &self.block {
            Some(block) => {
                block.clone().render(area, buf);
                block.inner(area)
            }
            None => area,
        };
        let Some(now) = self.end_time() else {
            self.render_empty(inner, buf);
            return;
        };
        let window = self.window_secs();
        let start = now - window;
        let visible: Vec<&TempPoint> = self
            .points
            .iter()
            .filter(|p| p.t.is_finite() && p.t >= start && p.t <= now)
            .collect();
        let series = self.series(&visible, now);
        if series.is_empty() {
            self.render_empty(inner, buf);
            return;
        }
        let max = series
            .iter()
            .flat_map(|s| s.runs.iter().flatten())
            .map(|(_, v)| *v)
            .fold(f64::NEG_INFINITY, f64::max);
        let y_max = nice_max(max);
        let theme = self.theme;
        let names: Vec<String> = series.iter().map(Self::legend_name).collect();
        let mut datasets = Vec::new();
        for (s, name) in series.iter().zip(&names) {
            let color = if s.target {
                theme.heater_target(s.heater)
            } else {
                theme.heater(s.heater)
            };
            // Without colors, targets are told apart by their dotted marker.
            let marker = if s.target && !theme.has_color() {
                Marker::Dot
            } else {
                Marker::Braille
            };
            for (i, run) in s.runs.iter().enumerate() {
                let mut ds = Dataset::default()
                    .data(run)
                    .graph_type(GraphType::Line)
                    .marker(marker)
                    .style(theme.fg(color));
                if i == 0 {
                    ds = ds.name(name.clone());
                }
                datasets.push(ds);
            }
        }
        let x_labels = vec![
            Span::styled(time_label(window), theme.dim()),
            Span::styled(time_label(window / 2.0), theme.dim()),
            Span::styled(time_label(0.0), theme.dim()),
        ];
        let y_labels = vec![
            Span::styled("0", theme.dim()),
            Span::styled(format::trimmed(y_max / 2.0, 0), theme.dim()),
            Span::styled(format::trimmed(y_max, 0), theme.dim()),
        ];
        Chart::new(datasets)
            .x_axis(
                Axis::default()
                    .bounds([-window, 0.0])
                    .labels(x_labels)
                    .style(theme.fg(theme.border)),
            )
            .y_axis(
                Axis::default()
                    .bounds([0.0, y_max])
                    .labels(y_labels)
                    .style(theme.fg(theme.border)),
            )
            .legend_position(self.show_legend.then_some(LegendPosition::TopLeft))
            .hidden_legend_constraints((Constraint::Ratio(2, 3), Constraint::Ratio(2, 3)))
            // The plot clears its area; keep the screen background.
            .style(theme.fg(theme.text).bg(theme.background))
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testutil::{lines, render};

    fn heating(n: usize) -> Vec<TempPoint> {
        (0..n)
            .map(|i| {
                let t = i as f64 * 10.0;
                let f = i as f32 / n as f32;
                TempPoint {
                    t,
                    hotends: vec![(20.0 + 190.0 * f, 210.0)],
                    bed: Some((20.0 + 40.0 * f, 60.0)),
                    chamber: None,
                }
            })
            .collect()
    }

    #[test]
    fn empty_data_shows_message() {
        let buf = render(TempChart::new(&[]), 40, 5);
        assert!(lines(&buf)[2].contains("No temperature data yet"));
        // Samples with only NaN values count as empty too.
        let nan = [TempPoint {
            t: 1.0,
            hotends: vec![(f32::NAN, 0.0)],
            ..Default::default()
        }];
        let buf = render(TempChart::new(&nan), 40, 5);
        assert!(lines(&buf)[2].contains("No temperature data yet"));
    }

    #[test]
    fn draws_axes_labels_and_legend() {
        let points = heating(60);
        let chart = TempChart::new(&points).block(Block::bordered().title("Temps"));
        let buf = render(chart, 70, 20);
        let text = lines(&buf).join("\n");
        assert!(text.contains("Temps"));
        assert!(text.contains("-10m"), "{text}");
        assert!(text.contains("-5m"));
        assert!(text.contains("now"));
        assert!(text.contains("250"), "y axis max: {text}");
        // Legend with the latest readings.
        assert!(text.contains("T0 206.8°C"), "{text}");
        assert!(text.contains("T0 target 210"));
        assert!(text.contains("Bed 59.3°C"));
        assert!(text.contains("Bed target 60"));
        // Braille dots were drawn.
        assert!(text.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
    }

    #[test]
    fn series_split_at_gaps_and_off_targets() {
        let mut points = heating(10);
        points[4].hotends[0] = (f32::NAN, 0.0);
        points[5].hotends[0].1 = 0.0;
        let refs: Vec<&TempPoint> = points.iter().collect();
        let series = TempChart::new(&points).series(&refs, 90.0);
        let current = &series[0];
        assert!(!current.target);
        assert_eq!(current.runs.len(), 2);
        assert_eq!(current.runs[0].len() + current.runs[1].len(), 9);
        let target = &series[1];
        assert!(target.target);
        assert_eq!(target.runs.len(), 2);
        assert_eq!(target.runs[0].len(), 4);
        assert_eq!(target.runs[1].len(), 4);
        // x is relative to now.
        assert_eq!(current.runs[0][0].0, -90.0);
    }

    #[test]
    fn legend_can_be_hidden() {
        let points = heating(60);
        let buf = render(TempChart::new(&points).show_legend(false), 70, 20);
        let text = lines(&buf).join("\n");
        assert!(!text.contains("T0 206.8°C"), "{text}");
        assert!(text.contains("now"), "{text}");
    }

    #[test]
    fn window_filters_old_points_and_hides_targets() {
        let points = heating(60);
        let chart = TempChart::new(&points)
            .window(Duration::from_secs(60))
            .show_targets(false);
        let buf = render(chart, 60, 16);
        let text = lines(&buf).join("\n");
        assert!(text.contains("-60s"), "{text}");
        assert!(!text.contains("target"));
        let refs: Vec<&TempPoint> = points.iter().filter(|p| p.t >= 530.0).collect();
        let series = TempChart::new(&points)
            .show_targets(false)
            .series(&refs, 590.0);
        assert_eq!(series.len(), 2);
    }

    #[test]
    fn explicit_now_and_bad_times() {
        let mut points = heating(5);
        points.push(TempPoint {
            t: f64::NAN,
            hotends: vec![(500.0, 0.0)],
            ..Default::default()
        });
        // NaN time is skipped for "now"; samples after now are not drawn.
        let chart = TempChart::new(&points).now(20.0);
        assert_eq!(chart.end_time(), Some(20.0));
        let chart = TempChart::new(&points);
        assert_eq!(chart.end_time(), Some(40.0));
        let buf = render(chart.window(Duration::ZERO), 50, 12);
        assert!(lines(&buf).join("\n").contains("-10s"));
    }

    #[test]
    fn helpers() {
        assert_eq!(nice_max(205.0), 250.0);
        assert_eq!(nice_max(0.0), 50.0);
        assert_eq!(nice_max(f64::NAN), 50.0);
        assert_eq!(time_label(0.0), "now");
        assert_eq!(time_label(300.0), "-5m");
        assert_eq!(time_label(90.0), "-90s");
        assert_eq!(time_label(150.0), "-2.5m");
    }

    #[test]
    fn tiny_area_does_not_panic() {
        let points = heating(5);
        for (w, h) in [(0, 0), (1, 1), (3, 2), (10, 3)] {
            let _ = render(TempChart::new(&points), w, h);
        }
    }
}
