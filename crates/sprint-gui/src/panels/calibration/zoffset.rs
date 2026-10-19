//! Z offset / first layer wizard: babystep live while watching the first
//! layer (or a sheet of paper), then make the adjustment permanent in the
//! probe offset (M851 Z) or the home offset (M206 Z).

use egui::{Align, Layout, RichText, Ui};

use crate::context::AppCtx;
use crate::icons;
use crate::insight::calibrate::{
    ZOffsetSource, z_offset_after_babystep, z_offset_command, z_offset_source,
};
use crate::insight::{format, ui as kit};
use crate::theme::{self, TextRole, Tone};
use crate::widgets::{ActionButton, Card, IconButton, SegmentedControl};
use sprint_host::Axes;

/// How to use the wizard.
const GUIDANCE: &str = concat!(
    "Heat up, home and start a first-layer test print (or park the nozzle at Z0 over ",
    "a sheet of paper). Babystep until the lines are slightly squished and touch ",
    "each other, then store the adjustment.",
);

/// What the probe offset does.
const PROBE_NOTE: &str = "The probe's Z offset sets where the nozzle is when the probe triggers.";

/// Why the home offset is used.
const HOME_OFFSET_NOTE: &str = "No probe offset is reported, so the home offset shifts Z instead.";

/// Shown when neither offset is reported.
const NO_OFFSET_HELP: &str = concat!(
    "The firmware reported neither a probe offset (M851) nor a home offset (M206). ",
    "Babystepping still works; load the settings to store the result.",
);

/// Toast after applying the probe offset.
const APPLIED_MESSAGE: &str = "It takes effect at the next homing. Save to EEPROM to keep it.";

/// Toast after applying the home offset.
const APPLIED_HOME_MESSAGE: &str =
    "The babystep was taken back: the home offset moves the nozzle now. Save to EEPROM to keep it.";

/// When a stored probe offset applies.
const TAKES_EFFECT: &str = concat!(
    "The stored offset takes effect at the next homing; the babystepping stays ",
    "active until then.",
);

/// When a stored home offset applies.
const TAKES_EFFECT_HOME: &str = concat!(
    "The home offset applies to the next moves at once, so applying it takes the ",
    "babystepping back.",
);

/// Tooltip of the counter reset.
const RESET_TIP: &str = "Forget the babystepping counted so far (does not move the nozzle)";

/// Babystep sizes offered (mm).
pub const BABYSTEP_SIZES: [f32; 4] = [0.01, 0.025, 0.05, 0.1];

/// The Z offset wizard.
#[derive(Debug)]
pub struct ZOffsetWizard {
    /// Babystep size (mm).
    step: f32,
    /// Babystepping sent since the baseline (mm; negative = closer).
    total: f32,
    /// The offset the babystepping is relative to, captured on the first
    /// babystep.
    baseline: Option<(ZOffsetSource, f32)>,
}

impl Default for ZOffsetWizard {
    fn default() -> Self {
        ZOffsetWizard {
            step: 0.025,
            total: 0.0,
            baseline: None,
        }
    }
}

impl ZOffsetWizard {
    /// Status for the wizard list.
    pub fn status(&self) -> Option<(String, Tone)> {
        (self.total.abs() > 1e-4).then(|| {
            (
                format!("{} mm", format::signed(f64::from(self.total), 3)),
                Tone::Accent,
            )
        })
    }

    /// Babystepping accumulated since the last apply (mm).
    pub fn total(&self) -> f32 {
        self.total
    }

    /// Draw the wizard.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let state = ctx.state;
        let source = z_offset_source(&state.settings);
        let p = theme::ui_palette(ui);
        Card::new("Z offset and first layer")
            .icon(icons::BABYSTEP)
            .subtitle("M290 babystepping")
            .show(ui, |ui| {
                kit::dim(ui, GUIDANCE);
                ui.add_space(8.0);
                match source {
                    Some((src, z)) => {
                        let what = match src {
                            ZOffsetSource::Probe => PROBE_NOTE,
                            ZOffsetSource::HomeOffset => HOME_OFFSET_NOTE,
                        };
                        ui.horizontal(|ui| {
                            kit::metric(
                                ui,
                                src.name(),
                                &format!("{} mm", format::fixed(f64::from(z), 3)),
                                None,
                            );
                            ui.add_space(16.0);
                            ui.vertical(|ui| {
                                ui.add_space(6.0);
                                kit::caption(ui, what);
                            });
                        });
                    }
                    None => {
                        kit::notice(
                            ui,
                            Tone::Info,
                            icons::INFO,
                            "No Z offset reported",
                            Some(NO_OFFSET_HELP),
                            None,
                        );
                    }
                }
                ui.add_space(10.0);
                self.babystep_panel(ui, ctx);
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                // Make it permanent.
                let Some((src, z)) = source else {
                    return;
                };
                let baseline = match self.baseline {
                    Some((s, b)) if s == src => b,
                    _ => z,
                };
                let new_z = z_offset_after_babystep(src, baseline, self.total);
                let changed = (new_z - z).abs() > 1e-4;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{}: {} → {} mm",
                            src.name(),
                            format::fixed(f64::from(z), 3),
                            format::fixed(f64::from(new_z), 3)
                        ))
                        .font(theme::font(ui, TextRole::Title))
                        .color(if changed { p.text } else { p.text_dim }),
                    );
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ready = ctx.command_blocker().is_none();
                    let label = match src {
                        ZOffsetSource::Probe => "Apply to probe offset",
                        ZOffsetSource::HomeOffset => "Apply to home offset",
                    };
                    if ui
                        .add_enabled(
                            ready && changed,
                            ActionButton::new(label)
                                .icon(icons::CHECK)
                                .primary()
                                .tooltip(format!(
                                    "{} Z{}",
                                    src.code(),
                                    format::trimmed(f64::from(new_z), 3)
                                )),
                        )
                        .clicked()
                    {
                        ctx.printer
                            .apply_setting(&z_offset_command(&state.settings, src, new_z));
                        let message = match src {
                            // The probe offset counts from the next homing,
                            // which also ends the babystep.
                            ZOffsetSource::Probe => APPLIED_MESSAGE,
                            // M206 shifts the next moves right away: with
                            // the babystep still in place the adjustment
                            // would count twice until the next homing.
                            ZOffsetSource::HomeOffset => {
                                ctx.printer.babystep_z(-self.total);
                                APPLIED_HOME_MESSAGE
                            }
                        };
                        // Further babysteps count from the new offset.
                        self.baseline = Some((src, new_z));
                        self.total = 0.0;
                        ctx.toasts.success("Z offset applied").message(message);
                    }
                    if ui
                        .add_enabled(
                            ready && !kit::lacks_eeprom(state),
                            ActionButton::new("Save to EEPROM")
                                .icon(icons::SAVE)
                                .tooltip("M500: keep the offset after a restart"),
                        )
                        .on_disabled_hover_text(kit::NO_EEPROM)
                        .clicked()
                    {
                        ctx.printer.save_settings();
                        ctx.toasts.success("Z offset saved to EEPROM");
                    }
                });
                ui.add_space(4.0);
                kit::caption(
                    ui,
                    match src {
                        ZOffsetSource::Probe => TAKES_EFFECT,
                        ZOffsetSource::HomeOffset => TAKES_EFFECT_HOME,
                    },
                );
            });
    }

    fn babystep_panel(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let p = theme::ui_palette(ui);
        let ready = ctx.command_blocker().is_none();
        let source = z_offset_source(&ctx.state.settings);
        let mut step_by = None;
        egui::Frame::new()
            .fill(p.raised)
            .corner_radius(egui::CornerRadius::same(theme::RADIUS_LG))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        kit::caption(ui, "BABYSTEPPED");
                        ui.label(
                            RichText::new(format!(
                                "{} mm",
                                format::signed(f64::from(self.total), 3)
                            ))
                            .font(theme::font(ui, TextRole::Display))
                            .color(if self.total.abs() > 1e-4 {
                                p.accent
                            } else {
                                p.text
                            }),
                        );
                        let hint = if self.total < -1e-4 {
                            "nozzle closer to the bed"
                        } else if self.total > 1e-4 {
                            "nozzle further from the bed"
                        } else {
                            "no adjustment yet"
                        };
                        kit::caption(ui, hint);
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.add_enabled_ui(ready, |ui| {
                            let size = format::trimmed(f64::from(self.step), 3);
                            if ui
                                .add(
                                    IconButton::new(icons::ARROW_DOWN)
                                        .large()
                                        .tooltip(format!("Lower the nozzle {size} mm")),
                                )
                                .clicked()
                            {
                                step_by = Some(-self.step);
                            }
                            if ui
                                .add(
                                    IconButton::new(icons::ARROW_UP)
                                        .large()
                                        .tooltip(format!("Raise the nozzle {size} mm")),
                                )
                                .clicked()
                            {
                                step_by = Some(self.step);
                            }
                        });
                    });
                });
                ui.add_space(8.0);
                // The sizes with their unit when that fits, else the unit
                // goes into the caption.
                let sized = |unit: &str| -> Vec<(f32, String)> {
                    BABYSTEP_SIZES
                        .iter()
                        .map(|s| (*s, format!("{}{unit}", format::trimmed(f64::from(*s), 3))))
                        .collect()
                };
                let width_of = |ui: &Ui, options: &[(f32, String)]| {
                    let labels: Vec<&str> = options.iter().map(|(_, l)| l.as_str()).collect();
                    kit::segmented_width(ui, &labels)
                };
                let avail = ui.available_width();
                let (caption, options) = {
                    let with_unit = sized(" mm");
                    if 40.0 + width_of(ui, &with_unit) <= avail {
                        ("Step", with_unit)
                    } else {
                        ("Step (mm)", sized(""))
                    }
                };
                // Home and the reset sit right of the sizes when they fit,
                // else on a row of their own (instead of over the sizes).
                let one_row = avail >= 70.0 + width_of(ui, &options) + 220.0;
                let mut home = false;
                let mut reset = false;
                let job = ctx.job_active();
                let mut buttons = |ui: &mut Ui| {
                    reset = ui
                        .add_enabled(
                            self.total.abs() > 1e-6,
                            ActionButton::new("Reset counter")
                                .ghost()
                                .small()
                                .tooltip(RESET_TIP),
                        )
                        .clicked();
                    home = ui
                        .add_enabled(
                            ready && !job,
                            ActionButton::new("Home")
                                .icon(icons::HOME)
                                .small()
                                .tooltip("G28"),
                        )
                        .clicked();
                };
                ui.horizontal(|ui| {
                    kit::caption(ui, caption);
                    ui.add(
                        SegmentedControl::new(&mut self.step, &options).id_salt("babystep-size"),
                    );
                    if one_row {
                        ui.with_layout(Layout::right_to_left(Align::Center), &mut buttons);
                    }
                });
                if !one_row {
                    ui.add_space(4.0);
                    let width = ui.available_width();
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, 28.0),
                        Layout::right_to_left(Align::Center),
                        buttons,
                    );
                }
                if reset {
                    self.total = 0.0;
                    self.baseline = None;
                }
                if home {
                    ctx.printer.home(Axes::ALL);
                }
            });
        if let Some(mm) = step_by {
            if self.baseline.is_none() {
                self.baseline = source;
            }
            ctx.printer.babystep_z(mm);
            self.total = ((self.total + mm) * 1000.0).round() / 1000.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_shows_the_total() {
        let mut w = ZOffsetWizard::default();
        assert_eq!(w.status(), None);
        w.total = -0.05;
        assert_eq!(w.status().map(|s| s.0), Some("\u{2212}0.050 mm".to_owned()));
    }
}
