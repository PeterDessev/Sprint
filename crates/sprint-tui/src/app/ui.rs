//! Drawing the TUI: header, temperatures, position, job, layer view,
//! console, input line, key hints and the dialogs on top.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use sprint_host::{BedOrigin, ConnectionStatus, JobStatus};

use super::{App, Center, Focus, InputMode, JOG_STEPS, Modal};
use crate::commands::{COMMANDS, Group};
use crate::format;
use crate::report::{Tone, tone_style};
use crate::widgets::{
    CommandLine, CompletionPopup, ConsoleView, FeatureLegend, HeaterGauge, HeaterKind, HelpOverlay,
    HelpSection, JobProgress, JobTone, KeyHints, LayerCanvas, StatsPanel, TempChart,
};

/// Width from which the console gets its own column.
const WIDE: u16 = 150;
/// Temperature history shown in the chart.
const CHART_WINDOW: Duration = Duration::from_secs(600);

impl App {
    /// A bordered panel with a title, focused or not.
    fn panel(&self, title: impl Into<String>, focused: bool) -> Block<'static> {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(self.theme.border(focused))
            .title(Line::from(Span::styled(
                format!(" {} ", title.into()),
                if focused {
                    self.theme.title()
                } else {
                    self.theme.text().add_modifier(Modifier::BOLD)
                },
            )))
    }

    /// Draw the whole screen.
    pub(crate) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Block::new().style(self.theme.base()), area);
        if area.width < 24 || area.height < 8 {
            frame.render_widget(
                Paragraph::new("Sprint: enlarge the terminal")
                    .style(self.theme.dim())
                    .alignment(Alignment::Center),
                area,
            );
            return;
        }
        let input_h = if area.height >= 16 { 3 } else { 1 };
        let [header, body, input, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(input_h),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_header(frame, header);
        self.draw_body(frame, body);
        self.draw_input(frame, input);
        self.draw_hints(frame, hints, input_h >= 3);
        if self.jog.is_some() {
            self.draw_jog(frame);
        }
        self.draw_modal(frame);
    }

    // ----- header -----

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.theme;
        if self.estop_armed.is_some() {
            let style = Style::default()
                .fg(theme.background_or_black())
                .bg(theme.error)
                .add_modifier(Modifier::BOLD);
            let style = if theme.has_color() {
                style
            } else {
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            };
            frame.render_widget(
                Paragraph::new(
                    " ▲ EMERGENCY STOP: press F8 again to send M112 (any other key cancels)",
                )
                .style(style),
                area,
            );
            return;
        }
        frame.render_widget(Block::new().style(theme.surface()), area);
        let view = &self.view;
        let (dot, status_style) = match &view.status {
            ConnectionStatus::Connected => ("●", theme.success()),
            ConnectionStatus::Connecting | ConnectionStatus::Handshaking => ("●", theme.warning()),
            ConnectionStatus::Failed(_) => ("×", theme.error()),
            ConnectionStatus::Disconnected => ("○", theme.dim()),
        };
        let mut left = vec![
            Span::styled(" ◆ Sprint ", theme.accent()),
            Span::styled("│ ", theme.dim()),
            Span::styled(format!("{dot} {}", view.status.label()), status_style),
        ];
        if let Some(target) = &view.target {
            left.push(Span::styled(format!("  {target}"), theme.text()));
        }
        if let Some(fw) = &view.firmware {
            left.push(Span::styled(format!("  {fw}"), theme.dim()));
        }
        let mut right: Vec<Span> = Vec::new();
        if self.hidden_prompt.is_some() {
            // Hidden, but the printer still waits for the answer.
            right.push(Span::styled("▲ The printer asks ", theme.warning()));
            right.push(Span::styled("p", theme.key()));
            right.push(Span::raw("  "));
        }
        if view.halted {
            right.push(Span::styled(" HALTED ", theme.selected().bg(theme.error)));
            right.push(Span::raw(" "));
        } else if let Some(busy) = &view.busy {
            right.push(Span::styled(format!("busy: {busy}  "), theme.warning()));
        }
        if let Some(job) = &view.job {
            let tone = crate::report::job_tone(&job.status);
            let icon = match job.status {
                JobStatus::Printing | JobStatus::Starting | JobStatus::Resuming => "▶",
                JobStatus::Paused | JobStatus::Pausing => "‖",
                JobStatus::Completed => "●",
                _ => "■",
            };
            right.push(Span::styled(
                format!("{icon} {} ", job.status.label()),
                tone_style(theme, tone),
            ));
            right.push(Span::styled(
                format::percent(f64::from(job.progress)),
                theme.text().add_modifier(Modifier::BOLD),
            ));
            if let Some((layer, total)) = job.layer {
                right.push(Span::styled(
                    format!("  layer {}/{total}", (layer + 1).min(total.max(1))),
                    theme.dim(),
                ));
            }
            if let Some(eta) = job.eta.filter(|_| job.status.is_active()) {
                right.push(Span::styled(
                    format!("  ETA {}", format::clock_of(eta)),
                    theme.dim(),
                ));
            }
            right.push(Span::raw(" "));
        } else if let Some(loader) = &self.loader {
            right.push(Span::styled(
                format!(
                    "Analyzing {} {} ",
                    loader.name(),
                    format::percent(f64::from(loader.progress()))
                ),
                theme.warning(),
            ));
        } else if let Some(tune) = view.autotune.as_ref().filter(|t| t.running) {
            right.push(Span::styled(
                format!(
                    "PID autotune · {} · {} °C ",
                    tune.heater.name(),
                    format::trimmed(f64::from(tune.target), 1)
                ),
                theme.warning(),
            ));
        }
        let right_line = Line::from(right);
        let right_w = u16::try_from(right_line.width())
            .unwrap_or(area.width)
            .min(area.width);
        let [l, _, r] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(2),
            Constraint::Length(right_w),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(fit_line(left, l.width, theme)), l);
        frame.render_widget(Paragraph::new(right_line), r);
    }

    // ----- body -----

    fn draw_body(&mut self, frame: &mut Frame, area: Rect) {
        let left_w = if area.width < 64 {
            0
        } else {
            (area.width * 8 / 25).clamp(32, 44)
        };
        let [left, rest] =
            Layout::horizontal([Constraint::Length(left_w), Constraint::Fill(1)]).areas(area);
        let (center, console) = if area.width >= WIDE {
            let console_w = (rest.width * 2 / 5).clamp(44, 90);
            let [c, k] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(console_w)])
                .areas(rest);
            (c, k)
        } else {
            let console_h = (rest.height * 2 / 5)
                .clamp(5, 16)
                .min(rest.height.saturating_sub(6));
            let [c, k] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(console_h)]).areas(rest);
            (c, k)
        };
        if left_w > 0 {
            self.draw_left(frame, left);
        }
        self.draw_center(frame, center);
        self.draw_console(frame, console);
    }

    /// (kind, current, target, power) of every heater to show.
    fn heater_rows(&self) -> Vec<(HeaterKind, f32, Option<f32>, Option<f32>)> {
        let temps = &self.view.temps;
        let mut rows = Vec::new();
        let hotends = temps.hotends.len().max(self.view.extruders).max(1);
        for tool in 0..hotends.min(8) {
            let reading = temps.hotends.get(tool);
            let kind = HeaterKind::Hotend(u8::try_from(tool).unwrap_or(0));
            rows.push((
                kind,
                reading.map_or(f32::NAN, |r| r.current),
                reading.and_then(|r| r.target),
                reading.and_then(|r| r.power),
            ));
        }
        if temps.bed.is_some() || self.profile.heated_bed {
            let r = temps.bed.as_ref();
            rows.push((
                HeaterKind::Bed,
                r.map_or(f32::NAN, |r| r.current),
                r.and_then(|r| r.target),
                r.and_then(|r| r.power),
            ));
        }
        if temps.chamber.is_some() || self.profile.heated_chamber {
            let r = temps.chamber.as_ref();
            rows.push((
                HeaterKind::Chamber,
                r.map_or(f32::NAN, |r| r.current),
                r.and_then(|r| r.target),
                r.and_then(|r| r.power),
            ));
        }
        rows
    }

    fn draw_left(&self, frame: &mut Frame, area: Rect) {
        let heaters = u16::try_from(self.heater_rows().len()).unwrap_or(1);
        let gauges_h = heaters.saturating_add(2);
        // The rows go, in order of importance, to the heater gauges, the
        // job, the position, the overrides and a third row of the job;
        // the chart takes what is left if that is enough to read it, else
        // the link statistics do.
        let mut left = area.height.saturating_sub(gauges_h);
        let mut take = |rows: u16| {
            if left >= rows {
                left -= rows;
                rows
            } else {
                0
            }
        };
        let mut job_h = take(4);
        let pos_h = take(3);
        let ovr_h = take(3);
        if job_h > 0 {
            job_h += take(1);
        }
        // A blank row and six chart rows below the gauges.
        let (temp_h, link_h) = if left > 6 {
            (gauges_h + left, 0)
        } else if left >= 3 {
            (gauges_h, left.min(4))
        } else {
            (gauges_h, 0)
        };
        let [temps, position, job, overrides, link, _] = Layout::vertical([
            Constraint::Length(temp_h),
            Constraint::Length(pos_h),
            Constraint::Length(job_h),
            Constraint::Length(ovr_h),
            Constraint::Length(link_h),
            Constraint::Fill(1),
        ])
        .areas(area);
        self.draw_temperatures(frame, temps);
        if pos_h > 0 {
            self.draw_position(frame, position);
        }
        self.draw_job(frame, job);
        if ovr_h > 0 {
            self.draw_overrides(frame, overrides);
        }
        if link_h > 0 {
            self.draw_link(frame, link);
        }
    }

    /// Link statistics: throughput, round trip and errors.
    fn draw_link(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.theme;
        let view = &self.view;
        let block = self.panel("Link", false);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if !view.status.is_open() {
            let lines = vec![Line::from(vec![
                Span::styled("Not connected · ", theme.dim()),
                Span::styled("F2", theme.key()),
                Span::styled(" connects", theme.dim()),
            ])];
            frame.render_widget(Paragraph::new(lines), inner);
            return;
        }
        let link = &view.link;
        let count = |n: u64| format::count(usize::try_from(n).unwrap_or(usize::MAX));
        let label = |t: &'static str| Span::styled(t, theme.dim());
        let value = |t: String| Span::styled(t, theme.text());
        let errors = link.resends + link.errors + link.timeouts;
        let error_style = if errors > 0 {
            theme.warning()
        } else {
            theme.text()
        };
        let lines = vec![
            Line::from(vec![
                label("Sent "),
                value(count(link.lines_sent)),
                label("  "),
                value(format::fixed(f64::from(link.lines_per_sec), 0)),
                label("/s  RTT "),
                value(format::fixed(f64::from(link.rtt_avg_ms), 1)),
                label(" ms"),
            ]),
            Line::from(vec![
                label("Resend "),
                Span::styled(link.resends.to_string(), error_style),
                label("  Error "),
                Span::styled(link.errors.to_string(), error_style),
                label("  Timeout "),
                Span::styled(link.timeouts.to_string(), error_style),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_temperatures(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.theme;
        let block = self.panel("Temperatures", false);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let rows = self.heater_rows();
        let n = u16::try_from(rows.len()).unwrap_or(0);
        let label_w = rows
            .iter()
            .map(|(k, ..)| k.label().chars().count())
            .max()
            .unwrap_or(2);
        for (i, (kind, current, target, power)) in rows.iter().enumerate() {
            let Ok(dy) = u16::try_from(i) else { break };
            if dy >= inner.height {
                break;
            }
            let row = Rect::new(inner.x, inner.y + dy, inner.width, 1);
            frame.render_widget(
                HeaterGauge::new(*kind, *current, *target)
                    .power(*power)
                    .label_width(label_w)
                    .theme(theme),
                row,
            );
        }
        let chart_h = inner.height.saturating_sub(n + 1);
        if chart_h >= 6 {
            let chart = Rect::new(inner.x, inner.y + n + 1, inner.width, chart_h);
            if self.temps.points.is_empty() {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        "No temperature history yet",
                        theme.dim(),
                    )))
                    .alignment(Alignment::Center),
                    Rect::new(chart.x, chart.y + chart.height / 2, chart.width, 1),
                );
            } else {
                frame.render_widget(
                    TempChart::new(&self.temps.points)
                        .window(CHART_WINDOW)
                        .show_legend(false)
                        .theme(theme),
                    chart,
                );
            }
        }
    }

    fn draw_position(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.theme;
        let block = self.panel("Position", false);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let value = |v: Option<f32>| {
            v.filter(|v| v.is_finite()).map_or_else(
                || format::MISSING.to_string(),
                |v| format::fixed(f64::from(v), 2),
            )
        };
        let p = self.view.position.as_ref();
        let mut spans = Vec::new();
        let axes = [
            ("X", p.map(|p| p.x)),
            ("Y", p.map(|p| p.y)),
            ("Z", p.map(|p| p.z)),
            ("E", p.map(|p| p.e)),
        ];
        let width = usize::from(inner.width);
        // Aligned columns when there is room for X, Y and Z that way,
        // else packed tightly.
        let aligned = width >= 3 * 10 + 8;
        let mut used = 0;
        for (i, (label, v)) in axes.into_iter().enumerate() {
            let shown = if aligned {
                format!("{:>7}", value(v))
            } else {
                value(v)
            };
            let gap = if i == 0 {
                ""
            } else if aligned {
                "  "
            } else {
                " "
            };
            let cols = gap.len() + label.len() + 1 + shown.chars().count();
            if used + cols > width {
                break;
            }
            used += cols;
            spans.push(Span::styled(format!("{gap}{label} "), theme.dim()));
            spans.push(Span::styled(shown, theme.text()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), inner);
    }

    fn draw_job(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.theme;
        let mut block = self.panel("Job", false);
        let job = self.view.job.as_ref();
        if let Some((layer, total)) = job.and_then(|j| j.layer) {
            block = block.title(
                Line::from(Span::styled(
                    format!(" layer {}/{total} ", (layer + 1).min(total.max(1))),
                    theme.dim(),
                ))
                .right_aligned(),
            );
        }
        if let Some(message) = self
            .view
            .message
            .as_deref()
            .filter(|m| !m.trim().is_empty())
        {
            block = block.title_bottom(Line::from(Span::styled(
                format!(" {} ", message.trim()),
                theme.dim(),
            )));
        }
        let widget = if let Some(job) = job {
            let tone = match &job.status {
                JobStatus::Starting | JobStatus::Printing | JobStatus::Resuming => JobTone::Active,
                JobStatus::Pausing | JobStatus::Paused | JobStatus::Cancelling => JobTone::Paused,
                JobStatus::Completed => JobTone::Done,
                JobStatus::Cancelled | JobStatus::Failed(_) => JobTone::Failed,
            };
            JobProgress::new(job.name.as_str(), job.progress)
                .elapsed(Some(job.elapsed))
                .eta(job.eta.filter(|_| job.status.is_active()))
                .layer(job.layer)
                .status(job.status.label(), tone)
        } else if let Some(loader) = &self.loader {
            JobProgress::new(loader.name(), loader.progress()).status("Analyzing", JobTone::Paused)
        } else {
            // Nothing running: what is loaded and what to press.
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let lines = match &self.file {
                Some(file) => {
                    let stats = &file.toolpath.stats;
                    vec![
                        Line::from(vec![Span::styled(file.name().to_string(), theme.text())]),
                        Line::from(Span::styled(
                            format!(
                                "{} layers · {} · {}",
                                file.toolpath.layers.len(),
                                format::duration(f64::from(stats.estimated_time)),
                                format::length_mm(f64::from(stats.filament_length))
                            ),
                            theme.dim(),
                        )),
                        Line::from(vec![
                            Span::styled("Ready", theme.success()),
                            Span::styled(" · F5 prints", theme.dim()),
                        ]),
                    ]
                }
                None => vec![
                    Line::from(Span::styled("No file loaded", theme.dim())),
                    Line::from(vec![
                        Span::styled("F3", theme.key()),
                        Span::styled(" opens a G-code file", theme.dim()),
                    ]),
                ],
            };
            frame.render_widget(Paragraph::new(lines), inner);
            return;
        };
        frame.render_widget(widget.block(block).theme(theme), area);
    }

    fn draw_overrides(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.theme;
        let block = self.panel("Fan · Speed · Flow", false);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let pct = |v: f32| format!("{}%", format::trimmed(f64::from(v), 0));
        let (gap, speed) = match inner.width {
            0..31 => ("  ", "Spd "),
            31..34 => ("  ", "Speed "),
            _ => ("   ", "Speed "),
        };
        let mut spans = vec![
            Span::styled("Fan ", theme.dim()),
            Span::styled(crate::report::fan_percent(self.view.fan), theme.text()),
            Span::styled(format!("{gap}{speed}"), theme.dim()),
            Span::styled(pct(self.view.feedrate_pct), theme.text()),
            Span::styled(format!("{gap}Flow "), theme.dim()),
            Span::styled(pct(self.view.flow_pct), theme.text()),
        ];
        if self.view.extruders > 1 {
            spans.push(Span::styled(
                format!("   T{}", self.view.active_tool),
                theme.accent(),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), inner);
    }

    fn draw_center(&mut self, frame: &mut Frame, area: Rect) {
        self.canvas_area = area;
        let focused = self.focus == Focus::Layers && self.jog.is_none();
        let theme = self.theme.clone();
        let Some(file) = &self.file else {
            let block = self.panel("Layer view", focused);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            self.draw_welcome(frame, inner);
            return;
        };
        if self.center == Center::Stats {
            // The bounds measured at load: measuring walks every segment.
            let toolpath = &file.toolpath;
            let panel = StatsPanel::new(&toolpath.stats, &toolpath.metadata)
                .bounds(Some(file.part_bounds))
                .theme(&theme);
            let max = panel
                .line_count()
                .saturating_sub(usize::from(area.height.saturating_sub(2)));
            self.stats_scroll = self.stats_scroll.min(max);
            let block = self.panel(format!("Statistics · {}", file.name()), focused);
            frame.render_widget(panel.scroll(self.stats_scroll).block(block), area);
            return;
        }
        let shown = self.shown_layer();
        let printing = self.printing_layer();
        let job_active = self.view.job_active();
        let canvas = LayerCanvas::new(
            &file.toolpath,
            shown,
            self.progress_line(),
            self.profile.bed_size,
        )
        .fit(self.fit)
        .origin_center(self.profile.origin == BedOrigin::Center)
        .object_bounds(file.object_bounds)
        .show_travel(self.show_travel)
        .show_tool(job_active && printing == Some(shown))
        .theme(&theme);
        let mode = match (self.layer_view, printing) {
            (None, Some(_)) => "following the print",
            (None, None) => "←/→ browse layers",
            (Some(_), Some(_)) => "f follows the print",
            (Some(_), None) => "browsing",
        };
        let fit = match self.fit {
            crate::LayerFit::Bed => "bed",
            crate::LayerFit::Object => "object",
        };
        let label = canvas.label_text();
        // The right title only when both fit on the border.
        let right = format!(" {mode} · {fit} ");
        let room = usize::from(area.width).saturating_sub(label.chars().count() + 6);
        let right = if right.chars().count() <= room {
            right
        } else if mode.chars().count() + 2 <= room {
            format!(" {mode} ")
        } else {
            String::new()
        };
        let block = self
            .panel(label, focused)
            .title(Line::from(Span::styled(right, theme.dim())).right_aligned());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let legend_h = if inner.height >= 14 { 1 } else { 0 };
        let [view, legend] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(legend_h)]).areas(inner);
        frame.render_widget(canvas, view);
        if legend_h > 0 {
            frame.render_widget(
                FeatureLegend::new(&file.toolpath, Some(shown)).theme(&theme),
                legend,
            );
        }
    }

    /// The centre panel before a file is loaded.
    fn draw_welcome(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.theme;
        let mut lines: Vec<Line> = Vec::new();
        if let Some(loader) = &self.loader {
            let width = usize::from(area.width).clamp(10, 50) - 8;
            let progress = loader.progress();
            let filled = ((progress * width as f32).round() as usize).min(width);
            lines.push(Line::from(Span::styled(
                format!("Analyzing {}", loader.name()),
                theme.text(),
            )));
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled("━".repeat(filled), theme.accent()),
                Span::styled("━".repeat(width - filled), theme.dim()),
                Span::styled(
                    format!(" {}", format::percent(f64::from(progress))),
                    theme.accent(),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled("S P R I N T", theme.accent())));
            lines.push(Line::from(Span::styled(
                "a 3D printer host for the terminal",
                theme.dim(),
            )));
            lines.push(Line::default());
            for (key, what) in [
                ("F3", "open a G-code file"),
                ("F2", "connect a printer (or the virtual one)"),
                (":", "type a command, e.g. :connect sim"),
                ("F1", "all keys"),
            ] {
                lines.push(Line::from(vec![
                    Span::styled(format!("{key:>4}  "), theme.key()),
                    Span::styled(format!("{what:<40}"), theme.text()),
                ]));
            }
        }
        let height = u16::try_from(lines.len()).unwrap_or(0);
        let top = area.y + area.height.saturating_sub(height) / 2;
        let rect = Rect::new(area.x, top, area.width, height.min(area.height));
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), rect);
    }

    fn draw_console(&mut self, frame: &mut Frame, area: Rect) {
        self.console_area = area;
        let theme = self.theme.clone();
        let focused = self.focus == Focus::Console && self.jog.is_none();
        let view = ConsoleView::new(&self.console)
            .show_auto(self.show_auto)
            .search(self.search.as_deref())
            .theme(&theme);
        let mut flags = Vec::new();
        if let Some(query) = &self.search {
            flags.push(format!("“{query}” {} matches", view.match_count()));
        }
        if !self.console_state.is_following() {
            flags.push("scrolled".to_string());
        }
        if self.view.status.is_connected() && self.view.link.lines_per_sec >= 1.0 {
            flags.push(format!(
                "{} lines/s",
                format::fixed(f64::from(self.view.link.lines_per_sec), 0)
            ));
        }
        flags.push(if self.show_auto {
            "all traffic".to_string()
        } else {
            "auto hidden".to_string()
        });
        let block = self.panel("Console", focused).title(
            Line::from(Span::styled(
                format!(" {} ", flags.join(" · ")),
                theme.dim(),
            ))
            .right_aligned(),
        );
        frame.render_stateful_widget(view.block(block), area, &mut self.console_state);
    }

    // ----- input and hints -----

    fn draw_input(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.theme;
        let focused = self.focus == Focus::Input && self.jog.is_none() && self.modal.is_none();
        let (input, title, prompt, placeholder) = match self.input_mode {
            InputMode::Gcode => (
                &self.gcode_input,
                "G-code",
                "› ",
                if focused {
                    "type G-code, Enter sends (Tab completes)"
                } else {
                    "press i to type G-code, : for commands"
                },
            ),
            InputMode::Command => (
                &self.command_input,
                "Command",
                ": ",
                "a command such as connect sim, temp 210, mesh (help lists them)",
            ),
            InputMode::Search => (
                &self.search_input,
                "Search console",
                "/ ",
                "text to highlight",
            ),
        };
        let mut line = CommandLine::new(input)
            .prompt(prompt)
            .placeholder(placeholder)
            .focused(focused)
            .theme(theme);
        if area.height >= 3 {
            let mut block = self.panel(title, focused);
            // Status messages sit on the input's border, so the key hints
            // stay visible.
            let room = area
                .width
                .saturating_sub(u16::try_from(title.len()).unwrap_or(0) + 8);
            if let Some(toast) = self.toast_line(room) {
                block = block.title(toast.right_aligned());
            }
            line = line.block(block);
        }
        if let Some(pos) = line.cursor_position(area) {
            frame.set_cursor_position(pos);
        }
        frame.render_widget(line, area);
        if focused {
            let popup = CompletionPopup::new(input).theme(theme);
            if let Some(popup_area) = popup.area(area, frame.area()) {
                frame.render_widget(Clear, popup_area);
                frame.render_widget(popup, popup_area);
            }
        }
    }

    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.jog.is_some() {
            return vec![
                ("←→↑↓", "Move X/Y"),
                ("PgUp/PgDn", "Z"),
                ("+/-", "Step"),
                ("h", "Home"),
                ("x/y/z", "Home axis"),
                ("Esc", "Done"),
            ];
        }
        match (self.focus, self.input_mode) {
            (Focus::Input, InputMode::Search) => vec![("Enter", "Search"), ("Esc", "Back")],
            (Focus::Input, _) if self.active_input_ref().is_completing() => {
                vec![("Tab/↑↓", "Choose"), ("Enter", "Accept"), ("Esc", "Close")]
            }
            (Focus::Input, InputMode::Command) => vec![
                ("Enter", "Run"),
                ("Tab", "Complete"),
                ("↑↓", "History"),
                ("Esc", "Back"),
                ("F1", "Help"),
            ],
            (Focus::Input, InputMode::Gcode) => vec![
                ("Enter", "Send"),
                ("Tab", "Complete"),
                ("↑↓", "History"),
                ("PgUp/PgDn", "Scroll console"),
                ("Esc", "Back"),
                ("F1", "Help"),
            ],
            _ => {
                let pause = match self.view.job.as_ref().map(|j| &j.status) {
                    Some(JobStatus::Paused | JobStatus::Pausing) => "Resume",
                    _ => "Pause",
                };
                let mut hints = Vec::new();
                if self.hidden_prompt.is_some() {
                    hints.push(("p", "Printer asks"));
                }
                hints.extend([
                    ("F1", "Help"),
                    ("F2", "Connect"),
                    ("F3", "Open"),
                    ("F5", "Print"),
                    ("F6", pause),
                    ("F7", "Cancel"),
                    ("F8", "E-stop"),
                    ("F10", "Quit"),
                    (":", "Command"),
                    ("i", "G-code"),
                    ("j", "Jog"),
                    ("t", "Temps"),
                    ("Tab", "Focus"),
                ]);
                hints
            }
        }
    }

    fn active_input_ref(&self) -> &crate::widgets::CommandInput {
        match self.input_mode {
            InputMode::Gcode => &self.gcode_input,
            InputMode::Command => &self.command_input,
            InputMode::Search => &self.search_input,
        }
    }

    /// The status message as a line of at most `width` columns.
    fn toast_line(&self, width: u16) -> Option<Line<'static>> {
        let toast = self.toast.as_ref()?;
        let theme = &self.theme;
        let icon = match toast.tone {
            Tone::Good => "●",
            Tone::Warn => "▲",
            Tone::Bad => "×",
            _ => "›",
        };
        let style = match toast.tone {
            Tone::Plain => theme.text(),
            other => tone_style(theme, other),
        };
        let spans = vec![
            Span::styled(format!(" {icon} "), style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("{} ", toast.text), style),
        ];
        Some(fit_line(spans, width, theme))
    }

    fn draw_hints(&self, frame: &mut Frame, area: Rect, toast_shown: bool) {
        let theme = &self.theme;
        if !toast_shown && let Some(line) = self.toast_line(area.width) {
            frame.render_widget(Block::new().style(theme.surface()), area);
            frame.render_widget(Paragraph::new(line), area);
            return;
        }
        frame.render_widget(KeyHints::new(self.hints()).theme(theme), area);
    }

    // ----- overlays -----

    fn draw_jog(&self, frame: &mut Frame) {
        let theme = &self.theme;
        let step = JOG_STEPS[self.jog.unwrap_or(0).min(JOG_STEPS.len() - 1)];
        let canvas = self.canvas_area;
        let (w, h) = (36u16, 8u16);
        // Inside the layer view's border.
        if canvas.width < w + 2 || canvas.height < h + 2 {
            return;
        }
        let area = Rect::new(canvas.right() - w - 1, canvas.bottom() - h - 1, w, h);
        frame.render_widget(Clear, area);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .style(theme.surface())
            .title(Line::from(vec![
                Span::styled(" Jog ", theme.title()),
                Span::styled(
                    format!("· step {} mm ", format::trimmed(f64::from(step), 1)),
                    theme.dim(),
                ),
            ]));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let key = |k: &str| Span::styled(k.to_string(), theme.key());
        let dim = |t: &str| Span::styled(t.to_string(), theme.dim());
        let position = match &self.view.position {
            Some(p) => format!(
                "X {}  Y {}  Z {}",
                format::fixed(f64::from(p.x), 1),
                format::fixed(f64::from(p.y), 1),
                format::fixed(f64::from(p.z), 2)
            ),
            None => "position unknown".to_string(),
        };
        let lines = vec![
            Line::from(vec![dim("        "), key("↑"), dim(" Y+")]),
            Line::from(vec![
                dim("   X- "),
                key("←"),
                dim("   "),
                key("→"),
                dim(" X+    "),
                key("PgUp"),
                dim(" Z+"),
            ]),
            Line::from(vec![
                dim("        "),
                key("↓"),
                dim(" Y-        "),
                key("PgDn"),
                dim(" Z-"),
            ]),
            Line::from(vec![
                key(" +/-"),
                dim(" step  "),
                key("h"),
                dim(" home  "),
                key("Esc"),
                dim(" done"),
            ]),
            Line::default(),
            Line::from(Span::styled(format!(" {position}"), theme.text())),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_modal(&mut self, frame: &mut Frame) {
        let theme = self.theme.clone();
        let Some(modal) = &mut self.modal else {
            return;
        };
        match modal {
            Modal::Help { scroll } => {
                let sections = help_sections();
                let overlay = HelpOverlay::new(&sections)
                    .title("Sprint keys and commands")
                    .footer(Some("↑↓ scroll · Esc close".into()))
                    .theme(&theme);
                let inner_h = usize::from(overlay.area(frame.area()).height.saturating_sub(2));
                *scroll = (*scroll).min(overlay.content_height().saturating_sub(inner_h));
                frame.render_widget(overlay.scroll(*scroll), frame.area());
            }
            Modal::Connect(d) => d.draw(frame, &theme),
            Modal::Open(d) => d.draw(frame, &theme),
            Modal::Temps(d) => d.draw(frame, &theme),
            Modal::Confirm(d) => d.draw(frame, &theme),
            Modal::Report(d) => d.draw(frame, &theme),
            Modal::Prompt(d) => d.draw(frame, &theme),
        }
    }
}

/// `spans` cut to `width` columns, ending in a dim `…` when cut.
fn fit_line(spans: Vec<Span<'static>>, width: u16, theme: &crate::Theme) -> Line<'static> {
    let width = usize::from(width);
    let total: usize = spans.iter().map(Span::width).sum();
    if total <= width {
        return Line::from(spans);
    }
    let mut out = Vec::new();
    let mut used = 0;
    let room = width.saturating_sub(1);
    for span in spans {
        let w = span.width();
        if used + w <= room {
            used += w;
            out.push(span);
            continue;
        }
        let mut cut = String::new();
        for c in span.content.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if used + cw > room {
                break;
            }
            used += cw;
            cut.push(c);
        }
        out.push(Span::styled(cut, span.style));
        break;
    }
    if width > 0 {
        out.push(Span::styled("…", theme.dim()));
    }
    Line::from(out)
}

/// The help screen.
pub(crate) fn help_sections() -> Vec<HelpSection<'static>> {
    let mut sections = vec![
        HelpSection::new(
            "General",
            [
                ("F1 / ?", "This help"),
                ("F2", "Connect: virtual printer, serial port or TCP"),
                ("F3", "Open a G-code file"),
                ("Tab / Shift+Tab", "Move between layer view, console, input"),
                (":", "Command palette (shell commands, see below)"),
                ("i / Enter", "Type G-code (or start typing G28, M105, ...)"),
                ("F10 / Ctrl+Q / q", "Quit (asks while printing)"),
            ],
        ),
        HelpSection::new(
            "Printing",
            [
                ("F5", "Print the loaded file"),
                ("F6", "Pause / resume"),
                ("F7", "Cancel the print"),
                ("F8 F8", "EMERGENCY STOP (M112), press twice"),
                ("j", "Jog mode: arrows X/Y, PgUp/PgDn Z, +/- step, h home"),
                ("t", "Temperatures and presets"),
                ("p", "The printer's question again (after Esc hid it)"),
            ],
        ),
        HelpSection::new(
            "Layer view",
            [
                ("← → / [ ]", "Previous / next layer"),
                ("PgUp / PgDn", "Ten layers up / down"),
                ("Home / End", "First / last layer"),
                ("f", "Follow the printing layer"),
                ("b", "Fit to the bed or the object"),
                ("v", "Show travel moves"),
                ("s", "Statistics of the file"),
            ],
        ),
        HelpSection::new(
            "Console",
            [
                ("↑ ↓ PgUp PgDn", "Scroll (mouse wheel too)"),
                ("Home / End", "Oldest line / follow new output"),
                ("a", "Show automatic traffic (temperature polling)"),
                ("/", "Search; n clears it"),
                ("c", "Clear the console"),
            ],
        ),
        HelpSection::new(
            "Input line",
            [
                ("Enter", "Send / run"),
                ("Tab", "Complete G-code or command"),
                ("↑ ↓", "History"),
                ("Ctrl+A / E", "Start / end of line"),
                ("Ctrl+W / U / K", "Delete word / to start / to end"),
                ("Esc", "Back to the panels"),
            ],
        ),
    ];
    for group in Group::ALL {
        let entries: Vec<(String, String)> = COMMANDS
            .iter()
            .filter(|c| c.group == group)
            .map(|c| (format!(":{}", c.name), c.summary.to_string()))
            .collect();
        sections.push(HelpSection::new(
            format!("Commands: {}", group.title()),
            entries,
        ));
    }
    sections
}
