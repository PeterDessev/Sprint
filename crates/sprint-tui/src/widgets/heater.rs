//! Heater identities and states, and the one-line heater readout.

use std::borrow::Cow;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::format;
use crate::theme::{Theme, default_theme};
use crate::widgets::util::{self, sanitize};

/// Which heater a reading belongs to.
///
/// Mirrors the host's heater identities so applications can map them one
/// to one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HeaterKind {
    /// Hotend of the given tool (0-based).
    Hotend(u8),
    /// Heated bed.
    Bed,
    /// Heated chamber.
    Chamber,
}

impl HeaterKind {
    /// Short label: `"T0"`, `"Bed"`, `"Chamber"`.
    pub fn label(self) -> String {
        match self {
            HeaterKind::Hotend(tool) => format!("T{tool}"),
            HeaterKind::Bed => "Bed".to_string(),
            HeaterKind::Chamber => "Chamber".to_string(),
        }
    }
}

/// What a heater is doing relative to its target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HeaterState {
    /// No usable reading (missing or not a number).
    #[default]
    Unknown,
    /// No target set (heater off).
    Off,
    /// Below the target by more than [`HeaterState::TOLERANCE`].
    Heating,
    /// Above the target by more than [`HeaterState::TOLERANCE`].
    Cooling,
    /// Within [`HeaterState::TOLERANCE`] of the target.
    AtTarget,
}

impl HeaterState {
    /// Distance from the target (°C) still counted as "at target".
    pub const TOLERANCE: f32 = 2.0;

    /// Classify a reading. A missing, zero, negative or non-finite target
    /// means the heater is off.
    pub fn classify(current: f32, target: Option<f32>) -> HeaterState {
        if !current.is_finite() {
            return HeaterState::Unknown;
        }
        match target {
            Some(t) if t.is_finite() && t > 0.0 => {
                if (current - t).abs() <= Self::TOLERANCE {
                    HeaterState::AtTarget
                } else if current < t {
                    HeaterState::Heating
                } else {
                    HeaterState::Cooling
                }
            }
            _ => HeaterState::Off,
        }
    }

    /// Lowercase name: `"heating"`, `"at target"`, ...
    pub fn name(self) -> &'static str {
        match self {
            HeaterState::Unknown => "unknown",
            HeaterState::Off => "off",
            HeaterState::Heating => "heating",
            HeaterState::Cooling => "cooling",
            HeaterState::AtTarget => "at target",
        }
    }
}

/// Narrowest power bar worth drawing, in columns.
const MIN_BAR: usize = 4;
/// Longest custom label, in columns (longer ones are cut with `…`).
const MAX_LABEL: usize = 12;
/// Columns the temperatures are padded to when a bar follows
/// (`"205.3/210.5°C"`).
const TEMPS_COLS: usize = 13;

/// A compact one-line heater readout:
///
/// ```text
/// T0  205.3/210°C ████████▌        64%
/// Bed  22.1°C off ░░░░░░░░░░░░░░░   0%
/// ```
///
/// The label is drawn in the heater's color, the temperatures in the color
/// of the [`HeaterState`] (heating, cooling, at target, off), followed by a
/// bar of the heater power and the power in percent. When the area is too
/// narrow the bar, then the power, are left out. Only the first row of the
/// area is used.
#[derive(Clone, Debug)]
pub struct HeaterGauge<'a> {
    kind: HeaterKind,
    label: Option<Cow<'a, str>>,
    current: f32,
    target: Option<f32>,
    power: Option<f32>,
    label_width: usize,
    bar_width: Option<usize>,
    theme: &'a Theme,
}

impl<'a> HeaterGauge<'a> {
    /// A readout of `current` °C against `target` °C (`None` or 0 = off).
    pub fn new(kind: HeaterKind, current: f32, target: Option<f32>) -> HeaterGauge<'a> {
        HeaterGauge {
            kind,
            label: None,
            current,
            target,
            power: None,
            label_width: 0,
            bar_width: None,
            theme: default_theme(),
        }
    }

    /// Heater power in `0..=1`; enables the power bar.
    #[must_use]
    pub fn power(mut self, power: Option<f32>) -> Self {
        self.power = power;
        self
    }

    /// Replace the default label (`"T0"`, `"Bed"`, ...).
    #[must_use]
    pub fn label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Pad the label to this many columns so several gauges line up.
    #[must_use]
    pub fn label_width(mut self, width: usize) -> Self {
        self.label_width = width;
        self
    }

    /// Limit the power bar to this many columns (default: fill the row).
    #[must_use]
    pub fn bar_width(mut self, width: usize) -> Self {
        self.bar_width = Some(width);
        self
    }

    /// Draw with this theme.
    #[must_use]
    pub fn theme(mut self, theme: &'a Theme) -> Self {
        self.theme = theme;
        self
    }

    /// The state the readout is colored by.
    pub fn state(&self) -> HeaterState {
        HeaterState::classify(self.current, self.target)
    }

    /// The temperature part of the readout: `"205.3/210°C"`, `" 22.1°C off"`.
    fn temperatures(&self) -> String {
        let current = format!("{:>5}", format::temperature(self.current));
        match self.state() {
            HeaterState::Off => format!("{current}°C off"),
            HeaterState::Unknown if self.target.is_none_or(|t| !t.is_finite() || t <= 0.0) => {
                format!("{current}°C")
            }
            _ => {
                let target = self
                    .target
                    .map_or(String::new(), |t| format::trimmed(f64::from(t), 1));
                format!("{current}/{target}°C")
            }
        }
    }
}

impl Widget for HeaterGauge<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        let total = usize::from(area.width);
        let state = self.state();
        let label_text = match &self.label {
            Some(l) => util::truncate(&sanitize(l), MAX_LABEL).into_owned(),
            None => self.kind.label(),
        };
        let label_cols = util::width(&label_text).max(self.label_width);
        let label = format!(
            "{label_text}{} ",
            " ".repeat(label_cols - util::width(&label_text))
        );
        let temps = self.temperatures();
        let label_span = Span::styled(
            label.clone(),
            theme
                .fg(theme.heater(self.kind))
                .add_modifier(Modifier::BOLD),
        );
        let temp_style = theme.heater_state(state);
        let label_cols = util::width(&label);
        let temps_cols = util::width(&temps);
        let mut spans = Vec::with_capacity(6);
        spans.push(label_span);
        match self.power.filter(|p| p.is_finite()) {
            Some(power) => {
                let power = util::clamp01(power);
                let power_text = format!(" {:>4}", format!("{:.0}%", power * 100.0));
                let power_cols = util::width(&power_text);
                // With a bar, the temperatures are padded so the bars of
                // stacked gauges line up.
                let padded_cols = temps_cols.max(TEMPS_COLS);
                let bar_room = total.saturating_sub(label_cols + padded_cols);
                if bar_room >= power_cols + 1 + MIN_BAR {
                    let mut cells = bar_room - power_cols - 1;
                    if let Some(max) = self.bar_width {
                        cells = cells.min(max.max(MIN_BAR));
                    }
                    let (filled, empty) = util::bar(power, cells, '░');
                    let pad = " ".repeat(padded_cols - temps_cols);
                    spans.push(Span::styled(temps, temp_style));
                    spans.push(Span::raw(format!("{pad} ")));
                    spans.push(Span::styled(filled, theme.fg(theme.heater(self.kind))));
                    spans.push(Span::styled(empty, theme.dim()));
                    spans.push(Span::styled(power_text, theme.dim()));
                } else {
                    let fits = total.saturating_sub(label_cols + temps_cols) >= power_cols;
                    spans.push(Span::styled(temps, temp_style));
                    if fits {
                        spans.push(Span::styled(power_text, theme.dim()));
                    }
                }
            }
            None => spans.push(Span::styled(temps, temp_style)),
        }
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testutil::{lines, render};

    #[test]
    fn classify_states() {
        assert_eq!(HeaterState::classify(20.0, None), HeaterState::Off);
        assert_eq!(HeaterState::classify(20.0, Some(0.0)), HeaterState::Off);
        assert_eq!(HeaterState::classify(20.0, Some(-5.0)), HeaterState::Off);
        assert_eq!(
            HeaterState::classify(20.0, Some(f32::NAN)),
            HeaterState::Off
        );
        assert_eq!(
            HeaterState::classify(f32::NAN, Some(200.0)),
            HeaterState::Unknown
        );
        assert_eq!(
            HeaterState::classify(150.0, Some(200.0)),
            HeaterState::Heating
        );
        assert_eq!(
            HeaterState::classify(198.5, Some(200.0)),
            HeaterState::AtTarget
        );
        assert_eq!(
            HeaterState::classify(202.0, Some(200.0)),
            HeaterState::AtTarget
        );
        assert_eq!(
            HeaterState::classify(230.0, Some(200.0)),
            HeaterState::Cooling
        );
    }

    #[test]
    fn labels() {
        assert_eq!(HeaterKind::Hotend(1).label(), "T1");
        assert_eq!(HeaterKind::Bed.label(), "Bed");
        assert_eq!(HeaterKind::Chamber.label(), "Chamber");
        assert_eq!(HeaterState::AtTarget.name(), "at target");
    }

    #[test]
    fn full_readout_with_power_bar() {
        let gauge = HeaterGauge::new(HeaterKind::Hotend(0), 205.26, Some(210.0)).power(Some(0.5));
        let buf = render(gauge, 30, 1);
        // "T0 " + "205.3/210°C" padded to 13 columns, a space, the bar, "  50%".
        assert_eq!(lines(&buf), vec!["T0 205.3/210°C   ████░░░░  50%"]);
    }

    #[test]
    fn off_heater_and_alignment() {
        let gauge = HeaterGauge::new(HeaterKind::Bed, 22.14, None).label_width(4);
        let buf = render(gauge, 24, 1);
        assert_eq!(lines(&buf), vec!["Bed   22.1°C off        "]);
    }

    #[test]
    fn narrow_area_drops_bar_then_power() {
        let make = || HeaterGauge::new(HeaterKind::Hotend(1), 60.0, Some(60.0)).power(Some(0.25));
        // Room for the power but not the bar.
        let buf = render(make(), 18, 1);
        assert_eq!(lines(&buf), vec!["T1  60.0/60°C  25%"]);
        // Not even the power: the text is cut.
        let buf = render(make(), 8, 1);
        assert_eq!(lines(&buf), vec!["T1  60.0"]);
    }

    #[test]
    fn bar_width_limit_and_bad_values() {
        let gauge = HeaterGauge::new(HeaterKind::Chamber, f32::NAN, Some(f32::INFINITY))
            .power(Some(f32::NAN))
            .label("C")
            .bar_width(4);
        let buf = render(gauge, 30, 1);
        // NaN power is ignored; NaN temperature shows the placeholder.
        assert_eq!(lines(&buf)[0].trim_end(), "C    --°C");

        let gauge = HeaterGauge::new(HeaterKind::Hotend(0), 100.0, Some(200.0))
            .power(Some(7.0))
            .bar_width(4);
        let buf = render(gauge, 40, 1);
        assert_eq!(lines(&buf)[0].trim_end(), "T0 100.0/200°C   ████ 100%");
    }

    #[test]
    fn state_colors_follow_theme() {
        let theme = Theme::dark();
        let gauge = HeaterGauge::new(HeaterKind::Hotend(0), 150.0, Some(200.0)).theme(&theme);
        assert_eq!(gauge.state(), HeaterState::Heating);
        let buf = render(gauge, 20, 1);
        // The temperature text uses the heating (accent) color.
        assert_eq!(buf[(3, 0)].fg, theme.accent);
        // The label uses the hotend color.
        assert_eq!(buf[(0, 0)].fg, theme.heater(HeaterKind::Hotend(0)));
    }

    #[test]
    fn empty_area_is_fine() {
        let gauge = HeaterGauge::new(HeaterKind::Bed, 60.0, Some(60.0));
        let mut buf = Buffer::empty(Rect::new(0, 0, 0, 0));
        gauge.render(Rect::new(0, 0, 0, 0), &mut buf);
    }

    #[test]
    fn long_and_hostile_labels_are_cut() {
        let gauge = HeaterGauge::new(HeaterKind::Hotend(0), 20.0, None)
            .label("Extruder\u{1b}[31m number one with a long name");
        let buf = render(gauge, 30, 1);
        assert_eq!(lines(&buf)[0].trim_end(), "Extruder·[3…  20.0°C off");
    }
}
