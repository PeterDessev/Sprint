//! Temperature graph tab: heater history, targets, power and statistics.
//!
//! The chart plots every heater of `state.temp_history` (about one sample a
//! second, the last 30 minutes): the reading as a solid line and the target
//! as a dashed one in the heater's color, with the heater power in a lower
//! plot that shares the time axis. It follows the newest readings over a
//! chosen window until the user drags or zooms (then "Follow" returns).
//! Below, a table summarises the visible window per heater and flags
//! heaters that have held their target. The history can be exported as
//! CSV; the save dialog and the write run off the UI thread.

mod data;
mod export;

use egui::{
    Align, Color32, Grid, Id, LayerId, Layout, PopupAnchor, Response, RichText, ScrollArea, Sense,
    TextStyle, Ui, vec2,
};
use egui_plot::{
    ColorConflictHandling, Corner, Legend, Line, LineStyle, Plot, PlotPoints, PlotResponse,
    uniform_grid_spacer,
};
use sprint_host::TempSample;

use crate::context::AppCtx;
use crate::icons;
use crate::theme::{self, Palette, TextRole, Tone, with_alpha};
use crate::widgets::{
    ActionButton, Badge, Card, EmptyState, SegmentedControl, Toggle, card_frame, format_temperature,
};
use data::{Channel, Point};
use export::{Export, Outcome};

/// Plots sharing the time axis and the hover cursor.
const LINK: &str = "sprint-temp-graph-link";
/// Suggested file name of the CSV export.
const CSV_NAME: &str = "sprint-temperatures.csv";
/// Height of the power plot.
const POWER_HEIGHT: f32 = 120.0;
/// The temperature plot never gets shorter than this.
const MIN_MAIN_HEIGHT: f32 = 220.0;
/// Width of the value axes (keeps both plots aligned).
const Y_AXIS_WIDTH: f32 = 44.0;

/// How much history the chart shows while following.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Window {
    Min2,
    Min5,
    Min10,
    Min30,
    All,
}

impl Window {
    /// Every choice with its label.
    const CHOICES: [(Window, &'static str); 5] = [
        (Window::Min2, "2 min"),
        (Window::Min5, "5 min"),
        (Window::Min10, "10 min"),
        (Window::Min30, "30 min"),
        (Window::All, "All"),
    ];

    /// The window length in seconds (`None` = all of the history).
    fn secs(self) -> Option<f64> {
        match self {
            Window::Min2 => Some(120.0),
            Window::Min5 => Some(300.0),
            Window::Min10 => Some(600.0),
            Window::Min30 => Some(1800.0),
            Window::All => None,
        }
    }
}

/// Temperature graph tab: heater history, targets, power and statistics.
pub struct TemperatureGraphTab {
    window: Window,
    /// The view follows the newest readings (else the user's zoom/pan).
    follow: bool,
    show_power: bool,
    export: Option<Export>,
}

impl Default for TemperatureGraphTab {
    fn default() -> Self {
        TemperatureGraphTab {
            window: Window::Min10,
            follow: true,
            show_power: true,
            export: None,
        }
    }
}

/// One heater's data for this frame.
struct Series {
    channel: Channel,
    name: String,
    color: Color32,
    points: Vec<Point>,
}

/// The chart color of a heater: hotend / bed / chamber colors, further
/// hotends from the categorical palette.
fn channel_color(p: &Palette, channel: Channel) -> Color32 {
    const EXTRA: [usize; 7] = [4, 5, 3, 6, 1, 2, 7];
    match channel {
        Channel::Hotend(0) => p.hotend,
        Channel::Hotend(i) => p.plot_color(EXTRA[(i - 1) % EXTRA.len()]),
        Channel::Bed => p.bed,
        Channel::Chamber => p.chamber,
    }
}

/// A value-axis label: "50°", "52.5°".
fn degrees(value: f64) -> String {
    if value.fract().abs() < 1e-6 {
        format!("{value:.0}°")
    } else {
        format!("{value:.1}°")
    }
}

impl TemperatureGraphTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        self.poll_export(ctx);
        let history = data::usable(&ctx.state.temp_history);
        let history = history.as_slice();
        if history.is_empty() {
            empty(ui, ctx.connected());
            return;
        }
        let p = theme::ui_palette(ui);
        let channels = data::channels(history);
        let hotends = channels
            .iter()
            .filter(|c| matches!(c, Channel::Hotend(_)))
            .count();
        let series: Vec<Series> = channels
            .iter()
            .map(|&channel| Series {
                channel,
                name: channel.name(hotends),
                color: channel_color(&p, channel),
                points: data::points(history, channel),
            })
            .collect();
        let first = history.first().map_or(0.0, |s| s.t);
        let last = history.last().map_or(0.0, |s| s.t);
        let has_power = series
            .iter()
            .any(|s| s.points.iter().any(|pt| pt.power.is_some()));
        let height = ui.available_height();
        let connected = ctx.connected();
        ScrollArea::vertical()
            .id_salt("sprint-temp-graph-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = theme::GAP;
                self.toolbar(ui, history, connected, has_power, last - first);
                let show_power = self.show_power && has_power;
                let stats_height = 92.0 + 31.0 * series.len() as f32;
                let power_height = if show_power { POWER_HEIGHT + 26.0 } else { 0.0 };
                let main_height =
                    (height - 44.0 - stats_height - power_height - 48.0).max(MIN_MAIN_HEIGHT);
                let follow_x = data::follow_range(self.window.secs(), first, last);
                let follow_y = data::y_range(
                    series.iter().map(|s| s.points.as_slice()),
                    follow_x.0,
                    follow_x.1,
                );
                let follow = self.follow.then_some((follow_x, follow_y));
                let mut visible = follow_x;
                let mut moved = false;
                let mut reset = false;
                let mut hover = None;
                card_frame(&p).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let main = main_plot(ui, &series, main_height, show_power, follow);
                    let bounds = main.transform.bounds();
                    visible = (bounds.min()[0], bounds.max()[0]);
                    moved |= user_moved(ui, &main.response);
                    reset |= main.response.double_clicked();
                    hover = hover.or_else(|| hovered_time(&main));
                    if show_power {
                        ui.add_space(6.0);
                        caption(ui, &p, "Heater power");
                        let power = power_plot(ui, &series, follow.map(|f| f.0));
                        moved |= user_moved(ui, &power.response);
                        reset |= power.response.double_clicked();
                        hover = hover.or_else(|| hovered_time(&power));
                    }
                });
                if reset {
                    self.follow = true;
                } else if moved {
                    self.follow = false;
                }
                if let Some((t, layer, id)) = hover {
                    hover_readout(ui, history, &series, t, layer, id);
                }
                stats_card(ui, &series, visible, (first, last));
            });
    }

    /// Title, status and the view controls.
    fn toolbar(
        &mut self,
        ui: &mut Ui,
        history: &[&TempSample],
        connected: bool,
        has_power: bool,
        span: f64,
    ) {
        let p = theme::ui_palette(ui);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let exporting = self.export.is_some();
                let export = ui.add_enabled(
                    !exporting,
                    ActionButton::new(if exporting {
                        "Exporting…"
                    } else {
                        "Export CSV"
                    })
                    .icon(icons::DOWNLOAD)
                    .tooltip("Save the whole history (up to 30 minutes) as a CSV file"),
                );
                if export.clicked() {
                    self.export = Some(Export::start(
                        ui.ctx(),
                        data::history_csv(history),
                        CSV_NAME.to_owned(),
                    ));
                }
                if has_power {
                    ui.add(Toggle::new(&mut self.show_power).label("Power"))
                        .on_hover_text("Show the heater power below the chart");
                }
                let follow = ui.add(
                    ActionButton::new("Follow")
                        .icon(icons::LINK)
                        .selected(self.follow)
                        .tooltip(if self.follow {
                            "Following the newest readings. Drag or zoom the chart to look around."
                        } else {
                            "Return to the newest readings"
                        }),
                );
                if follow.clicked() {
                    self.follow = true;
                }
                let mut window = self.window;
                let r = ui.add(
                    SegmentedControl::new(&mut window, &Window::CHOICES)
                        .id_salt("sprint-temp-graph-window"),
                );
                if r.changed() {
                    self.window = window;
                    self.follow = true;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new(icons::CHART).color(p.text_dim).size(16.0));
                    ui.label(
                        RichText::new("Heater history")
                            .font(theme::font(ui, TextRole::Title))
                            .color(p.text),
                    );
                    if connected {
                        ui.add(Badge::new("Live").tone(Tone::Success));
                    } else {
                        ui.add(Badge::new("Offline").tone(Tone::Neutral));
                    }
                    ui.label(
                        RichText::new(format!("{} recorded", data::format_span(span)))
                            .color(p.text_dim)
                            .size(12.0),
                    );
                });
            });
        });
    }

    /// Report a finished export.
    fn poll_export(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(outcome) = self.export.as_ref().and_then(Export::poll) else {
            return;
        };
        self.export = None;
        match outcome {
            Outcome::Saved(path) => {
                ctx.toasts
                    .success("Temperature history exported")
                    .message(path.display().to_string());
            }
            Outcome::Cancelled => {}
            Outcome::Failed(message) => {
                ctx.toasts.error("Export failed").message(message);
            }
        }
    }
}

/// The placeholder without any history.
fn empty(ui: &mut Ui, connected: bool) {
    let space = (ui.available_height() * 0.22).clamp(0.0, 220.0);
    ui.add_space(space);
    if connected {
        let _ = EmptyState::new(icons::HOURGLASS, "Waiting for temperature reports")
            .message("Readings appear here a few seconds after connecting.")
            .show(ui);
    } else {
        let _ = EmptyState::new(icons::CHART, "No temperature history")
            .message(
                "Connect to a printer to graph its heaters. Sprint keeps the last \
                 30 minutes of readings.",
            )
            .show(ui);
    }
}

fn caption(ui: &mut Ui, p: &Palette, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::font(ui, TextRole::Caption))
            .color(p.text_dim),
    );
}

/// The temperature plot. `follow` holds the (x, y) ranges to show.
fn main_plot(
    ui: &mut Ui,
    series: &[Series],
    height: f32,
    show_power: bool,
    follow: Option<((f64, f64), (f64, f64))>,
) -> PlotResponse<()> {
    Plot::new("sprint-temp-graph-main")
        .height(height)
        .show_background(false)
        .grid_color(grid_color(ui))
        .legend(
            Legend::default()
                .position(Corner::LeftTop)
                .follow_insertion_order(true)
                .color_conflict_handling(ColorConflictHandling::PickLast)
                .text_style(TextStyle::Small)
                .background_alpha(0.85),
        )
        .show_x(true)
        .show_y(false)
        .allow_scroll(false)
        .allow_axis_zoom_drag(false)
        .allow_double_click_reset(false)
        .x_grid_spacer(uniform_grid_spacer(|input| {
            data::time_steps(input.base_step_size)
        }))
        .x_axis_formatter(|mark, _| data::format_clock(mark.value))
        .y_grid_spacer(uniform_grid_spacer(|input| {
            data::value_steps(input.base_step_size)
        }))
        .y_axis_formatter(|mark, _| degrees(mark.value))
        .y_axis_min_width(Y_AXIS_WIDTH)
        .show_axes([!show_power, true])
        .link_axis(LINK, [true, false])
        .link_cursor(LINK, [true, false])
        .show(ui, |plot| {
            if let Some(((x0, x1), (y0, y1))) = follow {
                plot.set_plot_bounds_x(x0..=x1);
                plot.set_plot_bounds_y(y0..=y1);
            }
            // Targets first so the readings draw on top (and give the
            // legend its color).
            for s in series {
                for run in data::runs(&s.points, data::target_value) {
                    plot.line(
                        Line::new(s.name.as_str(), PlotPoints::from(data::simplify_flat(run)))
                            .color(with_alpha(s.color, 0.75))
                            .width(1.25)
                            .style(LineStyle::Dashed { length: 7.0 })
                            .allow_hover(false),
                    );
                }
            }
            for s in series {
                for run in data::runs(&s.points, data::current_value) {
                    plot.line(
                        Line::new(s.name.as_str(), PlotPoints::from(run))
                            .color(s.color)
                            .width(2.0),
                    );
                }
            }
        })
}

/// The heater power plot (0–100 %), sharing the time axis.
fn power_plot(ui: &mut Ui, series: &[Series], follow: Option<(f64, f64)>) -> PlotResponse<()> {
    Plot::new("sprint-temp-graph-power")
        .height(POWER_HEIGHT)
        .show_background(false)
        .grid_color(grid_color(ui))
        .show_x(true)
        .show_y(false)
        .allow_scroll(false)
        .allow_zoom([true, false])
        .allow_drag([true, false])
        .allow_boxed_zoom(false)
        .allow_axis_zoom_drag(false)
        .allow_double_click_reset(false)
        .x_grid_spacer(uniform_grid_spacer(|input| {
            data::time_steps(input.base_step_size)
        }))
        .y_grid_spacer(uniform_grid_spacer(|_| [25.0, 50.0, 100.0]))
        .x_axis_formatter(|mark, _| data::format_clock(mark.value))
        .y_axis_formatter(|mark, _| format!("{:.0}%", mark.value))
        .y_axis_min_width(Y_AXIS_WIDTH)
        .link_axis(LINK, [true, false])
        .link_cursor(LINK, [true, false])
        .show(ui, |plot| {
            if let Some((x0, x1)) = follow {
                plot.set_plot_bounds_x(x0..=x1);
            }
            plot.set_plot_bounds_y(-4.0..=104.0);
            for s in series {
                for run in data::runs(&s.points, data::power_value) {
                    plot.line(
                        Line::new(format!("{} power", s.name), PlotPoints::from(run))
                            .color(s.color)
                            .width(1.5)
                            .fill(0.0)
                            .fill_alpha(0.08),
                    );
                }
            }
        })
}

/// Grid lines: a muted tone of the theme (egui_plot fades the finer ones).
fn grid_color(ui: &Ui) -> Color32 {
    let p = theme::ui_palette(ui);
    with_alpha(p.text_dim, if p.dark { 0.45 } else { 0.5 })
}

/// Whether the user panned or zoomed a plot this frame.
fn user_moved(ui: &Ui, response: &Response) -> bool {
    response.dragged() || (response.hovered() && ui.input(|i| i.zoom_delta() != 1.0))
}

/// The time under the pointer, with the plot's layer and id.
fn hovered_time(plot: &PlotResponse<()>) -> Option<(f64, LayerId, Id)> {
    let pos = plot.response.hover_pos()?;
    let t = plot.transform.value_from_position(pos).x;
    t.is_finite()
        .then_some((t, plot.response.layer_id, plot.response.id))
}

/// A tooltip with every heater's reading at the hovered time.
fn hover_readout(
    ui: &Ui,
    history: &[&TempSample],
    series: &[Series],
    t: f64,
    layer: LayerId,
    id: Id,
) {
    let Some(sample) = data::nearest(history, t) else {
        return;
    };
    egui::Tooltip::always_open(ui.ctx().clone(), layer, id, PopupAnchor::Pointer)
        .gap(14.0)
        .show(|ui| {
            let p = theme::ui_palette(ui);
            ui.label(
                RichText::new(data::format_clock(sample.t))
                    .font(theme::font(ui, TextRole::Title))
                    .color(p.text),
            );
            Grid::new("sprint-temp-graph-hover")
                .num_columns(4)
                .spacing(vec2(12.0, 4.0))
                .show(ui, |ui| {
                    for s in series {
                        let Some((current, target)) = s.channel.reading(sample) else {
                            continue;
                        };
                        swatch_label(ui, s.color, &s.name);
                        ui.label(
                            RichText::new(format!("{} °C", format_temperature(current)))
                                .color(p.text)
                                .strong(),
                        );
                        let target = if target > 0.0 && target.is_finite() {
                            format!("→ {}", format_temperature(target).trim_end_matches(".0"))
                        } else {
                            "off".to_owned()
                        };
                        ui.label(RichText::new(target).color(p.text_dim));
                        match s.channel.power(sample) {
                            Some(power) => ui.label(
                                RichText::new(format!("{:.0}%", power.clamp(0.0, 1.0) * 100.0))
                                    .color(p.text_dim),
                            ),
                            None => ui.label(""),
                        };
                        ui.end_row();
                    }
                });
        });
}

/// A colored dot and a name.
fn swatch_label(ui: &mut Ui, color: Color32, name: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(vec2(9.0, 9.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.5, color);
        ui.label(name);
    });
}

/// What a heater is doing, for the status column.
fn status(points: &[Point]) -> (&'static str, Tone) {
    let Some(last) = points.last() else {
        return ("No data", Tone::Neutral);
    };
    if data::is_stable(points, data::STABLE_SECS, data::STABLE_BAND) {
        return ("Stable", Tone::Success);
    }
    if !last.target.is_finite() || last.target <= 0.0 {
        return ("Off", Tone::Neutral);
    }
    if !last.current.is_finite() {
        return ("No reading", Tone::Neutral);
    }
    let diff = last.current - last.target;
    if diff.abs() <= 2.0 {
        ("Settling", Tone::Info)
    } else if diff < 0.0 {
        ("Heating", Tone::Warning)
    } else {
        ("Cooling", Tone::Info)
    }
}

/// Width of a numeric column in the statistics table.
const NUMBER_WIDTH: f32 = 64.0;

/// A fixed-width table cell with right-aligned text.
fn number_cell(ui: &mut Ui, text: &str, font: egui::FontId, color: Color32) {
    let (rect, response) = ui.allocate_exact_size(vec2(NUMBER_WIDTH, 18.0), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    ui.painter().text(
        rect.right_center(),
        egui::Align2::RIGHT_CENTER,
        text,
        font,
        color,
    );
}

/// Statistics of the visible window, one row per heater.
fn stats_card(ui: &mut Ui, series: &[Series], (from, to): (f64, f64), (first, last): (f64, f64)) {
    let shown_from = from.max(first);
    let shown_to = to.min(last);
    let subtitle = if shown_from <= shown_to {
        format!(
            "{} \u{2013} {} \u{b7} visible window",
            data::format_clock(shown_from),
            data::format_clock(shown_to)
        )
    } else {
        "nothing in view".to_owned()
    };
    let _ = Card::new("Statistics")
        .icon(icons::STATS)
        .subtitle(&subtitle)
        .show(ui, |ui| {
            let p = theme::ui_palette(ui);
            let mono = theme::builtin_font(ui, TextStyle::Monospace);
            let header = theme::font(ui, TextRole::Section);
            Grid::new("sprint-temp-graph-stats")
                .num_columns(8)
                .spacing(vec2(18.0, 8.0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("HEATER")
                            .font(header.clone())
                            .color(p.text_dim),
                    );
                    for title in ["NOW \u{b0}C", "TARGET", "MIN", "MAX", "MEAN", "STD DEV"] {
                        number_cell(ui, title, header.clone(), p.text_dim);
                    }
                    ui.label(
                        RichText::new("STATUS")
                            .font(header.clone())
                            .color(p.text_dim),
                    );
                    ui.end_row();
                    let number = |ui: &mut Ui, value: Option<f32>, decimals: usize| match value
                        .filter(|v| v.is_finite())
                    {
                        Some(v) => {
                            number_cell(ui, &format!("{v:.decimals$}"), mono.clone(), p.text)
                        }
                        None => number_cell(ui, "\u{2014}", mono.clone(), p.text_faint),
                    };
                    for s in series {
                        swatch_label(ui, s.color, &s.name);
                        let last = s.points.last();
                        number(ui, last.map(|pt| pt.current), 1);
                        number(ui, last.map(|pt| pt.target).filter(|t| *t > 0.0), 0);
                        let stats = data::window_stats(&s.points, from, to);
                        number(ui, stats.map(|st| st.min), 1);
                        number(ui, stats.map(|st| st.max), 1);
                        number(ui, stats.map(|st| st.mean), 1);
                        number(ui, stats.map(|st| st.std_dev), 2);
                        let (label, tone) = status(&s.points);
                        ui.add(Badge::new(label).tone(tone)).on_hover_text(format!(
                            "Stable: within \u{b1}{} \u{b0}C of the target for the last {} s",
                            data::STABLE_BAND,
                            data::STABLE_SECS
                        ));
                        ui.end_row();
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(values: &[(f64, f32, f32)]) -> Vec<Point> {
        values
            .iter()
            .map(|&(t, current, target)| Point {
                t,
                current,
                target,
                power: None,
            })
            .collect()
    }

    #[test]
    fn statuses() {
        assert_eq!(status(&[]).0, "No data");
        let steady: Vec<_> = (0..=40).map(|i| (f64::from(i), 215.1, 215.0)).collect();
        assert_eq!(status(&pts(&steady)), ("Stable", Tone::Success));
        assert_eq!(status(&pts(&[(0.0, 24.0, 0.0)])).0, "Off");
        assert_eq!(status(&pts(&[(0.0, 150.0, 215.0)])).0, "Heating");
        assert_eq!(status(&pts(&[(0.0, 214.0, 215.0)])).0, "Settling");
        assert_eq!(status(&pts(&[(0.0, 240.0, 215.0)])).0, "Cooling");
        assert_eq!(status(&pts(&[(0.0, f32::NAN, 215.0)])).0, "No reading");
    }

    #[test]
    fn heater_colors_and_labels() {
        let p = Palette::dark();
        assert_eq!(channel_color(&p, Channel::Hotend(0)), p.hotend);
        assert_eq!(channel_color(&p, Channel::Bed), p.bed);
        assert_eq!(channel_color(&p, Channel::Chamber), p.chamber);
        assert_ne!(channel_color(&p, Channel::Hotend(1)), p.hotend);
        // Many hotends never index out of range.
        let _ = channel_color(&p, Channel::Hotend(500));
        assert_eq!(degrees(50.0), "50°");
        assert_eq!(degrees(52.5), "52.5°");
        assert_eq!(
            Window::CHOICES
                .iter()
                .map(|(w, _)| w.secs())
                .collect::<Vec<_>>(),
            [Some(120.0), Some(300.0), Some(600.0), Some(1800.0), None]
        );
    }
}
