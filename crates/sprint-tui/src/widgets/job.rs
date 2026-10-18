//! Print job progress: file name, status, a progress bar, elapsed time,
//! ETA and layer.

use std::borrow::Cow;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use crate::format;
use crate::theme::{Theme, default_theme};
use crate::widgets::util::{self, sanitize};

/// How a job's status is colored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum JobTone {
    /// Running (accent color).
    #[default]
    Active,
    /// Paused or waiting (warning color).
    Paused,
    /// Finished successfully (success color).
    Done,
    /// Failed or cancelled (error color).
    Failed,
    /// No job (dim).
    Idle,
}

/// Job progress panel.
///
/// With three or more rows (inside the optional block):
///
/// ```text
/// calibration-cube.gcode                 Printing
/// ████████████████▌                         42.3%
/// Elapsed 0:21:13  ETA 0:28:55  Layer 43/100
/// ```
///
/// Two rows put the ETA next to the percentage; a single row shows
/// `name bar percent ETA`. Progress values outside `0..=1` (or NaN) are
/// clamped. Layers are given as (0-based index, total) and shown 1-based.
#[derive(Clone, Debug)]
pub struct JobProgress<'a> {
    name: Cow<'a, str>,
    progress: f32,
    elapsed: Option<Duration>,
    eta: Option<Duration>,
    layer: Option<(usize, usize)>,
    status: Option<Cow<'a, str>>,
    tone: JobTone,
    block: Option<Block<'a>>,
    theme: &'a Theme,
}

impl<'a> JobProgress<'a> {
    /// Progress of the job printing `name`, `progress` in `0..=1`.
    pub fn new(name: impl Into<Cow<'a, str>>, progress: f32) -> JobProgress<'a> {
        JobProgress {
            name: name.into(),
            progress,
            elapsed: None,
            eta: None,
            layer: None,
            status: None,
            tone: JobTone::Active,
            block: None,
            theme: default_theme(),
        }
    }

    /// Time printed so far.
    #[must_use]
    pub fn elapsed(mut self, elapsed: Option<Duration>) -> Self {
        self.elapsed = elapsed;
        self
    }

    /// Estimated time remaining.
    #[must_use]
    pub fn eta(mut self, eta: Option<Duration>) -> Self {
        self.eta = eta;
        self
    }

    /// Current layer as (0-based index, total layers).
    #[must_use]
    pub fn layer(mut self, layer: Option<(usize, usize)>) -> Self {
        self.layer = layer;
        self
    }

    /// Status text ("Printing", "Paused", ...) and its coloring.
    #[must_use]
    pub fn status(mut self, status: impl Into<Cow<'a, str>>, tone: JobTone) -> Self {
        self.status = Some(status.into());
        self.tone = tone;
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

    fn tone_style(&self) -> Style {
        let t = self.theme;
        match self.tone {
            JobTone::Active => t.accent(),
            JobTone::Paused => t.warning(),
            JobTone::Done => t.success(),
            JobTone::Failed => t.error(),
            JobTone::Idle => t.dim(),
        }
    }

    fn percent_text(&self) -> String {
        format::percent(f64::from(util::clamp01(self.progress)))
    }

    fn eta_text(&self) -> String {
        self.eta
            .map_or_else(|| format::MISSING.to_string(), format::clock_of)
    }

    /// Row 0: name on the left, status on the right.
    fn title_line(&self, width: usize) -> Line<'static> {
        let status = self
            .status
            .as_deref()
            .map(|s| util::truncate(&sanitize(s), width / 2).into_owned())
            .unwrap_or_default();
        let status_cols = util::width(&status);
        let name_room = width.saturating_sub(status_cols + usize::from(status_cols > 0));
        let name = util::truncate_start(&sanitize(&self.name), name_room).into_owned();
        let gap = width.saturating_sub(util::width(&name) + status_cols);
        Line::from(vec![
            Span::styled(name, self.theme.text()),
            Span::raw(" ".repeat(gap)),
            Span::styled(status, self.tone_style()),
        ])
    }

    /// A bar of `cells` columns.
    fn bar_spans(&self, cells: usize) -> [Span<'static>; 2] {
        let (filled, empty) = util::bar(self.progress, cells, '░');
        let fill_style = match self.tone {
            JobTone::Active => self.theme.fg(self.theme.accent),
            _ => self.tone_style(),
        };
        [
            Span::styled(filled, fill_style),
            Span::styled(empty, self.theme.dim()),
        ]
    }

    /// Bar followed by `suffix` (right-aligned), filling `width`.
    fn bar_line(&self, width: usize, suffix: String) -> Line<'static> {
        let suffix_cols = util::width(&suffix);
        let mut spans = Vec::new();
        if width > suffix_cols + 1 {
            let cells = width - suffix_cols - 1;
            spans.extend(self.bar_spans(cells));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(suffix, self.theme.accent()));
        Line::from(spans)
    }

    /// Row 2: elapsed, ETA and layer, dropping items that do not fit.
    fn info_line(&self, width: usize) -> Line<'static> {
        let mut items: Vec<(&'static str, String)> = Vec::new();
        items.push((
            "Elapsed ",
            self.elapsed
                .map_or_else(|| format::MISSING.to_string(), format::clock_of),
        ));
        items.push(("ETA ", self.eta_text()));
        if let Some((index, total)) = self.layer {
            let shown = index.saturating_add(1).min(total.max(1));
            items.push(("Layer ", format!("{shown}/{total}")));
        }
        let mut spans = Vec::new();
        let mut used = 0;
        for (label, value) in items {
            let sep = if spans.is_empty() { 0 } else { 2 };
            let cols = sep + label.len() + util::width(&value);
            if used + cols > width {
                break;
            }
            if sep > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(label, self.theme.dim()));
            spans.push(Span::styled(value, self.theme.text()));
            used += cols;
        }
        Line::from(spans)
    }

    /// The single-row layout: name, bar, percent, ETA.
    fn compact_line(&self, width: usize) -> Line<'static> {
        let pct = self.percent_text();
        let eta = format!("ETA {}", self.eta_text());
        let clean = sanitize(&self.name);
        // A third of the row, but at least a readable stub of the name.
        let name_room = (width / 3).max(util::width(&clean).min(12)).min(width);
        let name = util::truncate_start(&clean, name_room).into_owned();
        let mut spans = vec![Span::styled(name.clone(), self.theme.text())];
        let mut used = util::width(&name);
        let tail = [pct, eta];
        let tail_cols: usize = tail.iter().map(|t| util::width(t) + 1).sum();
        if width >= used + 1 + 4 + tail_cols {
            let cells = width - used - 1 - tail_cols;
            spans.push(Span::raw(" "));
            spans.extend(self.bar_spans(cells));
            used += 1 + cells;
        }
        for (i, item) in tail.into_iter().enumerate() {
            let cols = util::width(&item) + 1;
            if used + cols > width {
                break;
            }
            spans.push(Span::raw(" "));
            let style = if i == 0 {
                self.theme.accent()
            } else {
                self.theme.dim()
            };
            spans.push(Span::styled(item, style));
            used += cols;
        }
        Line::from(spans)
    }
}

impl Widget for JobProgress<'_> {
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
        let width = usize::from(inner.width);
        let lines: Vec<Line<'static>> = match inner.height {
            1 => vec![self.compact_line(width)],
            2 => vec![
                self.title_line(width),
                self.bar_line(
                    width,
                    format!("{}  ETA {}", self.percent_text(), self.eta_text()),
                ),
            ],
            _ => vec![
                self.title_line(width),
                self.bar_line(width, self.percent_text()),
                self.info_line(width),
            ],
        };
        for (row, line) in lines.iter().enumerate() {
            let y = inner
                .y
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
            if y >= inner.bottom() {
                break;
            }
            buf.set_line(inner.x, y, line, inner.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testutil::{lines, render};

    fn job() -> JobProgress<'static> {
        JobProgress::new("calibration-cube.gcode", 0.5)
            .elapsed(Some(Duration::from_secs(1273)))
            .eta(Some(Duration::from_secs(1735)))
            .layer(Some((42, 100)))
            .status("Printing", JobTone::Active)
    }

    #[test]
    fn three_row_layout() {
        let buf = render(job(), 44, 3);
        assert_eq!(
            lines(&buf),
            vec![
                "calibration-cube.gcode              Printing",
                "███████████████████░░░░░░░░░░░░░░░░░░░ 50.0%",
                "Elapsed 21:13  ETA 28:55  Layer 43/100      ",
            ]
        );
    }

    #[test]
    fn two_row_layout_puts_eta_next_to_percent() {
        let buf = render(job(), 40, 2);
        let rows = lines(&buf);
        assert!(rows[1].ends_with("50.0%  ETA 28:55"), "{rows:?}");
    }

    #[test]
    fn single_row_layout() {
        let buf = render(job(), 60, 1);
        let row = &lines(&buf)[0];
        assert!(row.starts_with("…ibration-cube.gcode █"), "{row}");
        assert!(row.ends_with("50.0% ETA 28:55"), "{row}");
        // Very narrow: name only.
        let buf = render(job(), 10, 1);
        assert_eq!(lines(&buf)[0], "…ube.gcode");
    }

    #[test]
    fn narrow_info_row_drops_items() {
        let buf = render(job(), 26, 3);
        assert_eq!(lines(&buf)[2].trim_end(), "Elapsed 21:13  ETA 28:55");
        // Long names keep their end (the file name).
        assert_eq!(lines(&buf)[0], "…ation-cube.gcode Printing");
    }

    #[test]
    fn bad_values_are_clamped() {
        let p = JobProgress::new("x", f32::NAN).layer(Some((usize::MAX, 0)));
        let buf = render(p, 30, 3);
        let rows = lines(&buf);
        assert!(rows[1].ends_with(" 0.0%"), "{rows:?}");
        assert!(rows[2].contains("ETA --"), "{rows:?}");
        assert!(rows[2].contains("Layer 1/0"), "{rows:?}");
        let buf = render(JobProgress::new("x", 3.0), 20, 3);
        assert!(lines(&buf)[1].ends_with("████ 100.0%"));
    }

    #[test]
    fn block_and_tiny_areas() {
        let p = job().block(Block::bordered().title("Job"));
        let buf = render(p, 30, 5);
        let rows = lines(&buf);
        assert!(rows[0].starts_with("┌Job"));
        assert!(rows[1].contains("Printing"));
        // A block with no room inside renders only the border.
        let buf = render(job().block(Block::bordered()), 2, 2);
        assert_eq!(lines(&buf), vec!["┌┐", "└┘"]);
    }

    #[test]
    fn tone_colors_the_status() {
        let theme = Theme::dark();
        let p = job().status("Failed", JobTone::Failed).theme(&theme);
        let buf = render(p, 40, 3);
        assert_eq!(buf[(39, 0)].fg, theme.error);
    }
}
