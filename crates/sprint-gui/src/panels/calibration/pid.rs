//! PID autotune wizard: pick a heater, target and cycles, run M303, watch
//! the oscillation, compare the result with the current gains, apply and
//! save.

use egui::{RichText, Ui};
use sprint_host::Heater;

use super::{heaters, preset_chips, printer_time, reading};
use crate::context::AppCtx;
use crate::icons;
use crate::insight::calibrate::{self, PidGains, current_pid, pid_command};
use crate::insight::thermal::heater_series;
use crate::insight::{format, ui as kit};
use crate::panels::printer_info::heater_color;
use crate::theme::{self, Tone};
use crate::widgets::{
    ActionButton, Badge, Card, ChartSeries, KeyValueGrid, LinearProgress, MiniChart, NumericField,
    SegmentedControl, StatusPill,
};

/// What an autotune does.
const INTRO: &str = concat!(
    "The firmware heats and cools the heater around the target several times and ",
    "derives PID gains from the swings. It takes 5 to 10 minutes.",
);

/// The autotune error shown when the firmware halted during a run.
const HALTED_DURING_AUTOTUNE: &str = "the printer halted";

/// Advice for hotend autotunes.
const FAN_NOTE: &str = concat!(
    "Set the part-cooling fan to the speed you usually print with: it changes how ",
    "much power the hotend needs.",
);

/// Tooltip of the abort button.
const ABORT_TIP: &str = "Interrupt the autotune (M108) and switch the heater off";

/// Shown when the new gains are in use.
const ACTIVE_NOTE: &str = concat!(
    "The firmware is using these gains now (M303 U1). Save them to EEPROM to keep ",
    "them after a restart.",
);

/// Advice after a runaway.
const TOO_HIGH_TIP: &str = concat!(
    "The temperature ran away above the target. Lower the target, check the ",
    "thermistor's mounting and try again.",
);

/// Advice after a timeout.
const TIMEOUT_TIP: &str = concat!(
    "The heater did not reach or cross the target in time. Check the heater ",
    "cartridge, its wiring and the supply voltage.",
);

/// Default autotune cycles (Marlin's recommendation).
pub const DEFAULT_CYCLES: u32 = 8;

/// Width of the field labels.
const LABEL_WIDTH: f32 = 72.0;

/// The PID autotune wizard.
#[derive(Debug)]
pub struct PidWizard {
    heater: Heater,
    hotend_target: f32,
    bed_target: f32,
    chamber_target: f32,
    cycles: u32,
    /// Printer time and gains when this wizard started an autotune.
    started: Option<(f64, Heater, Option<PidGains>)>,
}

impl Default for PidWizard {
    fn default() -> Self {
        PidWizard {
            heater: Heater::Hotend(0),
            hotend_target: 210.0,
            bed_target: 60.0,
            chamber_target: 45.0,
            cycles: DEFAULT_CYCLES,
            started: None,
        }
    }
}

impl PidWizard {
    /// Tune `heater` next.
    pub fn select(&mut self, heater: Heater) {
        self.heater = heater;
    }

    /// The heater the wizard tunes.
    pub fn heater(&self) -> Heater {
        self.heater
    }

    fn target_mut(&mut self) -> &mut f32 {
        match self.heater {
            Heater::Hotend(_) => &mut self.hotend_target,
            Heater::Bed => &mut self.bed_target,
            Heater::Chamber => &mut self.chamber_target,
        }
    }

    fn range(&self) -> std::ops::RangeInclusive<f32> {
        match self.heater {
            Heater::Hotend(_) => 150.0..=300.0,
            Heater::Bed => 40.0..=120.0,
            Heater::Chamber => 30.0..=80.0,
        }
    }

    /// Draw the wizard.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let state = ctx.state;
        let autotune = state.autotune.clone();
        let running = autotune.as_ref().is_some_and(|a| a.running);
        let options: Vec<Heater> = heaters(state, ctx.config.active().heated_bed);
        if !options.contains(&self.heater) {
            self.heater = options.first().copied().unwrap_or(Heater::Hotend(0));
        }
        let hotends = options
            .iter()
            .filter(|h| matches!(h, Heater::Hotend(_)))
            .count();
        Card::new("Autotune")
            .icon(icons::HEATING)
            .subtitle("M303")
            .show(ui, |ui| {
                kit::dim(ui, INTRO);
                ui.add_space(8.0);
                ui.add_enabled_ui(!running, |ui| {
                    let labels: Vec<(Heater, String)> = options
                        .iter()
                        .map(|h| (*h, crate::panels::printer_info::heater_name(*h, hotends)))
                        .collect();
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        kit::field_label(ui, "Heater", LABEL_WIDTH);
                        ui.add(
                            SegmentedControl::new(&mut self.heater, &labels).id_salt("pid-heater"),
                        );
                    });
                    ui.add_space(6.0);
                    let range = self.range();
                    let target = self.target_mut();
                    ui.add(
                        NumericField::new(target)
                            .label("Target")
                            .unit("°C")
                            .decimals(0)
                            .range(range)
                            .label_width(LABEL_WIDTH)
                            .field_width(110.0),
                    );
                    let presets: Vec<(String, f32)> = ctx
                        .config
                        .active()
                        .temp_presets
                        .iter()
                        .map(|p| {
                            let v = match self.heater {
                                Heater::Hotend(_) => p.hotend,
                                Heater::Bed => p.bed,
                                Heater::Chamber => 0.0,
                            };
                            (p.name.clone(), v)
                        })
                        .collect();
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(LABEL_WIDTH + 6.0);
                        let current = *self.target_mut();
                        if let Some(v) = preset_chips(ui, &presets, current) {
                            *self.target_mut() = v;
                        }
                    });
                    ui.add_space(4.0);
                    ui.add(
                        NumericField::new(&mut self.cycles)
                            .label("Cycles")
                            .range(3..=20)
                            .label_width(LABEL_WIDTH)
                            .field_width(110.0)
                            .hint("More cycles average out noise; 8 is a good default."),
                    );
                });
                if matches!(self.heater, Heater::Hotend(_)) {
                    ui.add_space(4.0);
                    kit::caption(ui, FAN_NOTE);
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let blocker = ctx
                        .command_blocker()
                        .or_else(|| ctx.job_active().then_some("Not while a job runs"));
                    let can_start = blocker.is_none() && !running;
                    let target = *self.target_mut();
                    let label = format!("Start autotune at {target:.0} °C");
                    if ui
                        .add_enabled(
                            can_start,
                            ActionButton::new(&label).icon(icons::PLAY).primary(),
                        )
                        .on_disabled_hover_text(blocker.unwrap_or("An autotune is running"))
                        .clicked()
                    {
                        let baseline = current_pid(&state.settings, self.heater);
                        self.started = Some((printer_time(state), self.heater, baseline));
                        ctx.printer
                            .start_pid_autotune(self.heater, target, self.cycles);
                    }
                    if running
                        && ui
                            .add(
                                ActionButton::new("Abort")
                                    .icon(icons::STOP)
                                    .danger()
                                    .tooltip(ABORT_TIP),
                            )
                            .clicked()
                    {
                        let heater = autotune.as_ref().map_or(self.heater, |a| a.heater);
                        ctx.printer.send("M108");
                        ctx.printer.set_temperature(heater, 0.0);
                    }
                });
            });
        if let Some(a) = &autotune {
            ui.add_space(theme::GAP_LG);
            if a.running && state.halted {
                // A halted firmware ends the autotune without a report: the
                // run must not read as going on.
                failure(ui, HALTED_DURING_AUTOTUNE);
            } else if a.running {
                self.progress(ui, ctx, a.heater, a.target, a.cycles, hotends);
            } else if let Some(error) = &a.error {
                failure(ui, error);
            } else if let Some(result) = a.result {
                self.result(ui, ctx, a.heater, PidGains::from_tuple(result), hotends);
            }
        }
    }

    fn progress(
        &self,
        ui: &mut Ui,
        ctx: &AppCtx<'_>,
        heater: Heater,
        target: f32,
        cycles: u32,
        hotends: usize,
    ) {
        let state = ctx.state;
        let now = printer_time(state);
        let series = heater_series(&state.temp_history, heater);
        // When this wizard started the run, from then on; otherwise (or when
        // that was an earlier run) from the climb that began the run.
        let estimated = calibrate::autotune_start(&series, target);
        let mine = match self.started {
            Some((t, h, _)) if h == heater && t <= now => Some(t),
            _ => None,
        };
        let since = match (mine, estimated) {
            (Some(a), Some(b)) => a.max(b),
            (a, b) => a.or(b).unwrap_or(now),
        };
        let seen = calibrate::autotune_cycles(&series, target, since).min(cycles as usize);
        let name = crate::panels::printer_info::heater_name(heater, hotends);
        let p = theme::ui_palette(ui);
        let color = heater_color(&p, heater);
        let (current, _) = reading(state, heater);
        Card::new(&format!("Tuning {name} at {target:.0} °C"))
            .icon(icons::HOURGLASS)
            .show_with_header(
                ui,
                |ui| {
                    ui.add(StatusPill::new("Running", Tone::Accent).pulse(true));
                },
                |ui| {
                    kit::metric_grid(
                        ui,
                        3,
                        &[
                            (
                                "Temperature",
                                format::temperature(f64::from(current)),
                                Some(color),
                            ),
                            ("Elapsed", format::clock(now - since), None),
                            ("Cycles (est.)", format!("{seen} of {cycles}"), None),
                        ],
                    );
                    ui.add_space(4.0);
                    ui.add(
                        LinearProgress::new(seen as f32 / cycles.max(1) as f32)
                            .label("Oscillation cycles")
                            .show_percentage(true),
                    );
                    ui.add_space(8.0);
                    let temps: Vec<[f64; 2]> = series
                        .iter()
                        .filter(|pt| pt.t >= since)
                        .map(|pt| [pt.t - since, f64::from(pt.current)])
                        .collect();
                    let target_line: Vec<[f64; 2]> = match (temps.first(), temps.last()) {
                        (Some(a), Some(b)) => {
                            vec![[a[0], f64::from(target)], [b[0], f64::from(target)]]
                        }
                        _ => Vec::new(),
                    };
                    if temps.len() >= 2 {
                        MiniChart::new()
                            .series(ChartSeries::new(&name, &temps, color).fill(true))
                            .series(ChartSeries::new("Target", &target_line, color).dashed(true))
                            .unit("°C")
                            .height(150.0)
                            .show(ui);
                    } else {
                        kit::caption(ui, "Waiting for temperature reports…");
                    }
                },
            );
    }

    fn result(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        heater: Heater,
        gains: PidGains,
        hotends: usize,
    ) {
        let state = ctx.state;
        let current = current_pid(&state.settings, heater);
        let baseline = match self.started {
            Some((_, h, b)) if h == heater => b.or(current),
            _ => current,
        };
        let applied = current.is_some_and(|c| close(c, gains));
        let name = crate::panels::printer_info::heater_name(heater, hotends);
        let p = theme::ui_palette(ui);
        let mut apply = false;
        let mut save = false;
        Card::new(&format!("{name}: new PID gains"))
            .icon(icons::SUCCESS)
            .show_with_header(
                ui,
                |ui| {
                    if applied {
                        ui.add(Badge::new("ACTIVE").tone(Tone::Success));
                    }
                },
                |ui| {
                    KeyValueGrid::new("pid-result")
                        .key_width(40.0)
                        .show(ui, |kv| {
                            let rows = [
                                ("Kp", gains.kp, baseline.map(|b| b.kp)),
                                ("Ki", gains.ki, baseline.map(|b| b.ki)),
                                ("Kd", gains.kd, baseline.map(|b| b.kd)),
                            ];
                            for (label, new, old) in rows {
                                kv.row_ui(label, |ui| {
                                    ui.label(
                                        RichText::new(format::fixed(f64::from(new), 2))
                                            .font(egui::FontId::new(
                                                15.0,
                                                theme::font(ui, theme::TextRole::Title).family,
                                            ))
                                            .color(p.text),
                                    );
                                    if let Some(old) = old {
                                        ui.label(
                                            RichText::new(format!(
                                                "was {}",
                                                format::fixed(f64::from(old), 2)
                                            ))
                                            .size(12.0)
                                            .color(p.text_dim),
                                        );
                                        if let Some(change) =
                                            format::change(f64::from(old), f64::from(new))
                                        {
                                            ui.add(Badge::new(&change).tone(Tone::Neutral));
                                        }
                                    }
                                });
                            }
                        });
                    ui.add_space(6.0);
                    let note = if applied {
                        ACTIVE_NOTE
                    } else {
                        "Apply the gains, then save them to EEPROM to keep them after a restart."
                    };
                    kit::dim(ui, note);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let ready = ctx.command_blocker().is_none();
                        let code = match heater {
                            Heater::Hotend(_) => "M301",
                            Heater::Bed => "M304",
                            Heater::Chamber => "M309",
                        };
                        if !applied
                            && ui
                                .add_enabled(
                                    ready,
                                    ActionButton::new("Apply gains")
                                        .icon(icons::CHECK)
                                        .primary()
                                        .tooltip(format!("Send {code} with the new gains")),
                                )
                                .clicked()
                        {
                            apply = true;
                        }
                        let save_button = ActionButton::new("Save to EEPROM")
                            .icon(icons::SAVE)
                            .tooltip("M500: keep the gains after a restart");
                        let save_button = if applied {
                            save_button.primary()
                        } else {
                            save_button
                        };
                        let no_eeprom = kit::lacks_eeprom(state);
                        if ui
                            .add_enabled(ready && applied && !no_eeprom, save_button)
                            .on_disabled_hover_text(if no_eeprom {
                                kit::NO_EEPROM
                            } else {
                                "Apply the gains first"
                            })
                            .clicked()
                        {
                            save = true;
                        }
                    });
                },
            );
        if apply {
            ctx.printer
                .apply_setting(&pid_command(&state.settings, heater, gains));
            ctx.toasts.success("PID gains applied");
        }
        if save {
            ctx.printer.save_settings();
            ctx.toasts.success("PID gains saved to EEPROM");
        }
    }
}

/// Gains equal to two decimals.
fn close(a: PidGains, b: PidGains) -> bool {
    a.as_array()
        .iter()
        .zip(b.as_array())
        .all(|(x, y)| (x - y).abs() < 0.006 + 0.0005 * y.abs())
}

fn failure(ui: &mut Ui, error: &str) {
    let lower = error.to_lowercase();
    let tip = if lower.contains("halted") {
        "Restart the firmware (M999) or reconnect, then start the autotune again."
    } else if lower.contains("temperature too high") || lower.contains("too high") {
        TOO_HIGH_TIP
    } else if lower.contains("timeout") {
        TIMEOUT_TIP
    } else if lower.contains("bad extruder") || lower.contains("unknown") {
        "The firmware does not have this heater, or PID control is not enabled for it."
    } else {
        "Check the heater and thermistor, then try again. The previous gains are still in use."
    };
    kit::notice(
        ui,
        Tone::Danger,
        icons::ERROR,
        &format!("Autotune failed: {error}"),
        Some(tip),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gains_compare_to_two_decimals() {
        let a = PidGains {
            kp: 21.73,
            ki: 1.5,
            kd: 78.68,
        };
        let b = PidGains {
            kp: 21.734,
            ki: 1.498,
            kd: 78.677,
        };
        assert!(close(a, b));
        assert!(!close(
            a,
            PidGains {
                kp: 22.2,
                ki: 1.08,
                kd: 114.0
            }
        ));
    }

    #[test]
    fn targets_follow_the_heater() {
        let mut w = PidWizard::default();
        assert_eq!(*w.target_mut(), 210.0);
        w.heater = Heater::Bed;
        assert_eq!(*w.target_mut(), 60.0);
        assert_eq!(w.range(), 40.0..=120.0);
    }
}
