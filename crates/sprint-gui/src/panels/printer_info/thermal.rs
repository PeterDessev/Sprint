//! The thermal health card: per heater stability, last heat-up, holding
//! power and findings (with a shortcut to PID calibration).

use egui::{Align, Layout, RichText, Ui};
use sprint_host::Heater;

use crate::context::{AppAction, AppCtx, Tab};
use crate::icons;
use crate::insight::thermal::{self, Finding, HeaterHealth, ThermalState};
use crate::insight::{format, ui as kit};
use crate::theme::{self, HeaterKind, Palette, TextRole, Tone};
use crate::widgets::{Card, Sparkline, StatusPill};

/// The color of a heater.
pub(crate) fn heater_color(p: &Palette, heater: Heater) -> egui::Color32 {
    p.heater(match heater {
        Heater::Hotend(_) => HeaterKind::Hotend,
        Heater::Bed => HeaterKind::Bed,
        Heater::Chamber => HeaterKind::Chamber,
    })
}

/// "Hotend" for a single-hotend printer, else "Hotend 1", "Bed", ...
pub(crate) fn heater_name(heater: Heater, hotends: usize) -> String {
    match heater {
        Heater::Hotend(_) if hotends <= 1 => "Hotend".to_owned(),
        other => other.name(),
    }
}

fn state_pill(state: &ThermalState) -> (String, Tone) {
    match state {
        ThermalState::Off => ("Off".to_owned(), Tone::Neutral),
        ThermalState::Heating { remaining } => {
            (format!("Heating · {remaining:.0} °C to go"), Tone::Warning)
        }
        ThermalState::Cooling { excess } => (format!("Cooling · {excess:.0} °C above"), Tone::Info),
        ThermalState::Settling => ("Settling".to_owned(), Tone::Info),
        ThermalState::Stable => ("Stable".to_owned(), Tone::Success),
        ThermalState::Unstable => ("Oscillating".to_owned(), Tone::Danger),
        ThermalState::Unknown => ("No data".to_owned(), Tone::Neutral),
    }
}

pub(super) fn thermal_health(ui: &mut Ui, ctx: &mut AppCtx<'_>) {
    let history = &ctx.state.temp_history;
    let heaters = thermal::heaters_in(history);
    let hotends = heaters
        .iter()
        .filter(|h| matches!(h, Heater::Hotend(_)))
        .count();
    let healths: Vec<HeaterHealth> = heaters
        .iter()
        .map(|&h| thermal::analyze(history, h))
        .collect();
    let issues = healths.iter().map(|h| h.findings.len()).sum::<usize>();
    let mut calibrate = None;
    Card::new("Thermal health")
        .icon(icons::THERMOMETER)
        .subtitle("last 60 s at target")
        .show_with_header(
            ui,
            |ui| {
                if !healths.is_empty() {
                    let (label, tone) = if issues == 0 {
                        ("All good".to_owned(), Tone::Success)
                    } else {
                        (
                            format!("{issues} finding{}", if issues == 1 { "" } else { "s" }),
                            Tone::Warning,
                        )
                    };
                    ui.add(StatusPill::new(&label, tone));
                }
            },
            |ui| {
                if healths.is_empty() {
                    kit::dim(ui, "No temperature history yet.");
                    return;
                }
                for (i, health) in healths.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(2.0);
                    }
                    if heater_section(ui, health, hotends) {
                        calibrate = Some(health.heater);
                    }
                }
            },
        );
    if let Some(heater) = calibrate {
        // The Calibration tab opens its PID wizard for this heater.
        kit::request_pid_calibration(ui.ctx(), heater);
        ctx.actions.push(AppAction::ShowTab(Tab::Calibration));
    }
}

/// One heater; returns whether "Calibrate PID" was clicked.
fn heater_section(ui: &mut Ui, h: &HeaterHealth, hotends: usize) -> bool {
    let p = theme::ui_palette(ui);
    let color = heater_color(&p, h.heater);
    let name = heater_name(h.heater, hotends);
    let mut clicked = false;
    ui.push_id(("heater", format!("{:?}", h.heater)), |ui| {
        ui.horizontal(|ui| {
            kit::dot_label(ui, color, &name, true);
            ui.add_space(4.0);
            let reading = if h.target > 0.0 {
                format!(
                    "{} / {} °C",
                    format::fixed(f64::from(h.current), 1),
                    format::fixed(f64::from(h.target), 0)
                )
            } else {
                format!("{} °C", format::fixed(f64::from(h.current), 1))
            };
            ui.label(
                RichText::new(reading)
                    .font(egui::FontId::new(
                        13.0,
                        theme::font(ui, TextRole::Title).family,
                    ))
                    .color(p.text_dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (label, tone) = state_pill(&h.state);
                ui.add(StatusPill::new(&label, tone));
            });
        });
        if h.recent.len() >= 2 {
            ui.add(
                Sparkline::new(&h.recent)
                    .height(34.0)
                    .color(color)
                    .min_span(2.0)
                    .reference((h.target > 0.0).then_some(h.target)),
            );
        }
        let dash = || format::MISSING.to_owned();
        let deg = |v: f32, d: usize| format!("{} °C", format::fixed(f64::from(v), d));
        let s = h.stability.as_ref();
        let osc_color = s.and_then(|s| s.is_oscillating().then_some(p.danger));
        kit::metric_grid(
            ui,
            3,
            &[
                ("Mean", s.map_or_else(dash, |s| deg(s.mean, 1)), None),
                (
                    "Std dev",
                    s.map_or_else(dash, |s| deg(s.std_dev, 2)),
                    osc_color,
                ),
                (
                    "Peak-to-peak",
                    s.map_or_else(dash, |s| deg(s.peak_to_peak, 1)),
                    osc_color,
                ),
            ],
        );
        let up = h.heatup.as_ref();
        let overshoot_color =
            up.and_then(|u| (u.overshoot > thermal::OVERSHOOT_LIMIT).then_some(p.warning));
        kit::metric_grid(
            ui,
            3,
            &[
                (
                    "Time to target",
                    up.and_then(|u| u.time_to_target)
                        .map_or_else(dash, format::seconds),
                    None,
                ),
                (
                    "Overshoot",
                    up.filter(|u| u.time_to_target.is_some())
                        .map_or_else(dash, |u| deg(u.overshoot, 1)),
                    overshoot_color,
                ),
                (
                    "Settling time",
                    up.and_then(|u| u.settling_time)
                        .map_or_else(dash, format::seconds),
                    None,
                ),
            ],
        );
        if let Some(power) = s.and_then(|s| s.mean_power) {
            ui.horizontal(|ui| {
                kit::caption(ui, "Holding power");
                let width = (ui.available_width() - 44.0).max(40.0);
                let bar_color = if power > thermal::POWER_LIMIT {
                    p.warning
                } else {
                    color
                };
                kit::bar(ui, power, bar_color, width, 6.0);
                ui.label(
                    RichText::new(format::percent(f64::from(power), 0))
                        .size(12.0)
                        .color(p.text_dim),
                );
            });
        }
        if s.is_none() && h.target > 0.0 && matches!(h.state, ThermalState::Settling) {
            kit::caption(ui, "Statistics appear after 10 s at the target.");
        }
        if up.is_some_and(|u| u.partial) {
            kit::caption(
                ui,
                "The history starts during this heat-up: times are lower bounds.",
            );
        }
        for finding in &h.findings {
            ui.add_space(4.0);
            let tone = match finding {
                Finding::Oscillating { .. } => Tone::Danger,
                _ => Tone::Warning,
            };
            let action = finding.suggests_pid_tune().then_some("Calibrate PID");
            if kit::notice(
                ui,
                tone,
                icons::WARNING,
                &finding.title(),
                Some(finding.advice()),
                action,
            ) {
                clicked = true;
            }
        }
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heater_names() {
        assert_eq!(heater_name(Heater::Hotend(0), 1), "Hotend");
        assert_eq!(heater_name(Heater::Hotend(1), 2), "Hotend 1");
        assert_eq!(heater_name(Heater::Bed, 1), "Bed");
    }

    #[test]
    fn pills_for_states() {
        assert_eq!(state_pill(&ThermalState::Stable).1, Tone::Success);
        assert_eq!(state_pill(&ThermalState::Unstable).1, Tone::Danger);
        assert!(
            state_pill(&ThermalState::Heating { remaining: 12.4 })
                .0
                .contains("12")
        );
    }
}
