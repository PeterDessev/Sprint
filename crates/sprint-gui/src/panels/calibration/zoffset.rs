//! Z offset / first layer wizard: babystep live while watching the first
//! layer (or a sheet of paper), then make the adjustment permanent in the
//! probe offset (M851 Z) or the home offset (M206 Z).
//!
//! The babystepping counted is the printer state's
//! ([`PrinterState::babystep_z`]): every acknowledged babystep, from here,
//! the controls, the console or the print, since the counter was last
//! reset or the adjustment applied. Homing Z, a reset, a halt or a new
//! connection make the printer forget its babysteps, and the count starts
//! over. Babystepping with Z not known to be homed asks first (once, until
//! the homing changes): most firmware ignores babysteps then.

use egui::{Align, Layout, RichText, Ui};

use crate::context::AppCtx;
use crate::homing::{self, Consent};
use crate::icons;
use crate::insight::calibrate::{
    ZOffsetSource, z_offset_after_babystep, z_offset_command, z_offset_source,
};
use crate::insight::{format, ui as kit};
use crate::theme::{self, TextRole, Tone};
use crate::widgets::{
    ActionButton, Card, ConfirmDialog, DialogResult, IconButton, SegmentedControl,
};
use sprint_host::{Axes, PrinterState};

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

/// The babystepping counted from `mark` when the printer reports
/// `babystep_z` in all (mm, rounded to the micron: the accumulated reports
/// carry float noise). A printer that forgot its babysteps (exactly zero)
/// counts none.
pub fn counted_babystep(babystep_z: f32, mark: f32) -> f32 {
    if babystep_z == 0.0 || !babystep_z.is_finite() || !mark.is_finite() {
        return 0.0;
    }
    ((babystep_z - mark) * 1000.0).round() / 1000.0
}

/// The Z offset wizard.
#[derive(Debug)]
pub struct ZOffsetWizard {
    /// Babystep size (mm).
    step: f32,
    /// The printer's babystepping ([`PrinterState::babystep_z`]) the count
    /// starts from (mm): what it was when the counter was reset or the
    /// adjustment applied.
    mark: f32,
    /// The offset the babystepping is relative to, captured on the first
    /// babystep.
    baseline: Option<(ZOffsetSource, f32)>,
    /// "Babystep anyway" with Z not known to be homed.
    homing: Consent,
    /// A babystep (mm) waiting for "Babystep anyway".
    pending: Option<f32>,
}

impl Default for ZOffsetWizard {
    fn default() -> Self {
        ZOffsetWizard {
            step: 0.025,
            mark: 0.0,
            baseline: None,
            homing: Consent::default(),
            pending: None,
        }
    }
}

impl ZOffsetWizard {
    /// Status for the wizard list.
    pub fn status(&self, state: &PrinterState) -> Option<(String, Tone)> {
        let total = self.total(state);
        (total.abs() > 1e-4).then(|| {
            (
                format!("{} mm", format::signed(f64::from(total), 3)),
                Tone::Accent,
            )
        })
    }

    /// Babystepping since the counter was reset or the adjustment applied
    /// (mm; negative = closer).
    pub fn total(&self, state: &PrinterState) -> f32 {
        counted_babystep(state.babystep_z, self.mark)
    }

    /// Follow the printer: babysteps it forgot (homing Z, a reset, a halt,
    /// a new connection) no longer count, and a "Babystep anyway" lasts
    /// until the homing changes.
    fn observe(&mut self, state: &PrinterState) {
        if state.babystep_z == 0.0 {
            self.mark = 0.0;
        }
        self.homing.observe(state);
        if !state.is_connected() {
            self.pending = None;
        }
    }

    /// Draw the wizard.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        self.observe(ctx.state);
        let state = ctx.state;
        let total = self.total(state);
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
                self.homing_notice(ui, ctx);
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
                let new_z = z_offset_after_babystep(src, baseline, total);
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
                                ctx.printer.babystep_z(-total);
                                APPLIED_HOME_MESSAGE
                            }
                        };
                        // Further babysteps count from the new offset: from
                        // the printer's babystepping now, or once the take
                        // back above is acknowledged, from what it leaves.
                        self.baseline = Some((src, new_z));
                        self.mark = match src {
                            ZOffsetSource::Probe => state.babystep_z,
                            ZOffsetSource::HomeOffset => state.babystep_z - total,
                        };
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
        self.dialog(ui, ctx);
    }

    /// A warning while Z is not known to be homed, with a way to home.
    fn homing_notice(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        if !ctx.connected() || homing::unhomed(ctx.state, Axes::Z).is_empty() {
            return;
        }
        let can_home = ctx.command_blocker().is_none() && !ctx.printer_busy();
        let why = format!(
            "{} Most firmware ignores babysteps until Z is homed, and homing Z drops them \
             again: home before babystepping.",
            homing::unhomed_reason(ctx.state, Axes::Z)
        );
        if kit::notice(
            ui,
            Tone::Warning,
            icons::HOME,
            "Z is not known to be homed",
            Some(&why),
            can_home.then_some("Home all axes"),
        ) && can_home
        {
            ctx.printer.home(Axes::ALL);
        }
        ui.add_space(10.0);
    }

    /// "Babystep without homing Z?"
    fn dialog(&mut self, ui: &Ui, ctx: &mut AppCtx<'_>) {
        let Some(mm) = self.pending else {
            return;
        };
        let message = format!(
            "{} Most firmware ignores babysteps until Z is homed, and homing Z drops them \
             again.",
            homing::why_unhomed(ctx.state, Axes::Z)
        );
        let answer = ConfirmDialog::new(
            "sprint-zoffset-babystep-unhomed",
            "Babystep without homing Z?",
        )
        .message(&message)
        .confirm_label("Babystep anyway")
        .show(ui.ctx());
        match answer {
            Some(DialogResult::Confirmed) => {
                self.pending = None;
                self.homing.give(ctx.state);
                if ctx.command_blocker().is_none() {
                    self.babystep(ctx, mm);
                }
            }
            Some(DialogResult::Cancelled) => self.pending = None,
            None => {}
        }
    }

    /// Send a babystep, capturing the offset it is relative to on the
    /// first one.
    fn babystep(&mut self, ctx: &AppCtx<'_>, mm: f32) {
        if self.baseline.is_none() || self.total(ctx.state).abs() <= 1e-4 {
            self.baseline = z_offset_source(&ctx.state.settings);
        }
        ctx.printer.babystep_z(mm);
    }

    fn babystep_panel(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let p = theme::ui_palette(ui);
        let ready = ctx.command_blocker().is_none();
        let total = self.total(ctx.state);
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
                            RichText::new(format!("{} mm", format::signed(f64::from(total), 3)))
                                .font(theme::font(ui, TextRole::Display))
                                .color(if total.abs() > 1e-4 { p.accent } else { p.text }),
                        );
                        let hint = if total < -1e-4 {
                            "nozzle closer to the bed"
                        } else if total > 1e-4 {
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
                let job = ctx.printer_busy();
                let mut buttons = |ui: &mut Ui| {
                    reset = ui
                        .add_enabled(
                            total.abs() > 1e-4,
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
                    self.mark = ctx.state.babystep_z;
                    self.baseline = None;
                }
                if home {
                    ctx.printer.home(Axes::ALL);
                }
            });
        if let Some(mm) = step_by {
            if self.homing.ask(ctx.state, Axes::Z).is_empty() {
                self.babystep(ctx, mm);
            } else {
                self.pending = Some(mm);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_shows_the_total() {
        let mut w = ZOffsetWizard::default();
        let mut state = PrinterState::default();
        assert_eq!(w.status(&state), None);
        state.babystep_z = -0.05;
        assert_eq!(
            w.status(&state).map(|s| s.0),
            Some("\u{2212}0.050 mm".to_owned())
        );
        // Counted from the reset counter.
        w.mark = -0.05;
        assert_eq!(w.status(&state), None);
        state.babystep_z = -0.075;
        assert_eq!(w.total(&state), -0.025);
    }

    #[test]
    fn babysteps_count_from_the_mark_until_the_printer_forgets_them() {
        // Float noise from the accumulated reports reads as the micron.
        let mut z = 0.0_f32;
        for _ in 0..4 {
            z += 0.025;
        }
        assert_eq!(counted_babystep(z, 0.0), 0.1);
        assert_eq!(counted_babystep(z, 0.05), 0.05);
        // Homing Z (or a reset) zeroes the printer's babystepping: nothing
        // counts, whatever the mark.
        assert_eq!(counted_babystep(0.0, 0.05), 0.0);
        assert_eq!(counted_babystep(f32::NAN, 0.0), 0.0);
        let mut w = ZOffsetWizard {
            mark: 0.05,
            ..ZOffsetWizard::default()
        };
        let state = PrinterState::default();
        w.observe(&state);
        assert_eq!(w.mark, 0.0);
    }
}
