//! E-steps wizard: heat, mark the filament, extrude 100 mm slowly, measure
//! what is left and correct the extruder's steps/mm.

use egui::{Align2, CornerRadius, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};
use sprint_host::Heater;

use super::{preset_chips, reading, stepper};
use crate::context::AppCtx;
use crate::icons;
use crate::insight::calibrate::{
    self, ESTEPS_EXTRUDE_MM, ESTEPS_FEEDRATE_MM_MIN, ESTEPS_MARK_MM, ESTEPS_TEMP_TOLERANCE,
    current_esteps, esteps_command,
};
use crate::insight::{format, ui as kit};
use crate::theme::{self, TextRole, Tone, with_alpha};
use crate::widgets::{ActionButton, Card, LinearProgress, NumericField, StatusPill};

/// Step 1 guidance.
const HEAT_HELP: &str = concat!(
    "Heat the hotend to the temperature you print this filament at. The firmware ",
    "refuses to extrude cold filament.",
);

/// Step 2 guidance.
const MARK_HELP: &str = concat!(
    "Measure up from where the filament enters the extruder and mark the filament ",
    "with a fine pen. Measure carefully: the result is only as good as this mark.",
);

/// Step 4 guidance.
const MEASURE_HELP: &str = concat!(
    "Measure from the extruder inlet up to the mark. If the extruder were perfect, ",
    "it would be the mark distance minus 100 mm.",
);

/// Shown for corrections over 10%.
const LARGE_CHANGE_HELP: &str = concat!(
    "More than 10% off: measure again, and check the extruder's tension and gears ",
    "before applying.",
);

const STEPS: [&str; 4] = ["Heat", "Mark", "Extrude", "Measure"];

/// The E-steps wizard.
#[derive(Debug)]
pub struct EstepsWizard {
    step: usize,
    tool: u8,
    temperature: f32,
    mark: f32,
    remaining: f64,
    /// UI time when the test extrusion was sent.
    extrude_started: Option<f64>,
    /// The extruder steps/mm in effect for the test extrusion: the result
    /// corrects these, whatever the printer reports after applying it.
    tested_steps: Option<f32>,
    applied: Option<f32>,
}

impl Default for EstepsWizard {
    fn default() -> Self {
        EstepsWizard {
            step: 0,
            tool: 0,
            temperature: 205.0,
            mark: ESTEPS_MARK_MM,
            remaining: f64::from(ESTEPS_MARK_MM - ESTEPS_EXTRUDE_MM),
            extrude_started: None,
            tested_steps: None,
            applied: None,
        }
    }
}

impl EstepsWizard {
    /// Status for the wizard list.
    pub fn status(&self) -> Option<(String, Tone)> {
        if self.applied.is_some() {
            Some(("Applied".into(), Tone::Success))
        } else if self.step > 0 {
            Some((format!("Step {} of 4", self.step + 1), Tone::Accent))
        } else {
            None
        }
    }

    /// Expected duration of the test extrusion (seconds).
    pub fn extrusion_secs() -> f64 {
        f64::from(ESTEPS_EXTRUDE_MM / ESTEPS_FEEDRATE_MM_MIN * 60.0)
    }

    /// Draw the wizard.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        Card::new("Extruder E-steps")
            .icon(icons::EXTRUDE)
            .subtitle("M92 E")
            .show(ui, |ui| {
                stepper(ui, &STEPS, self.step);
                ui.add_space(6.0);
                match self.step {
                    0 => self.heat(ui, ctx),
                    1 => self.mark(ui),
                    2 => self.extrude(ui, ctx),
                    _ => self.measure(ui, ctx),
                }
            });
    }

    /// Back / next buttons; returns whether "next" was clicked.
    fn nav(&mut self, ui: &mut Ui, next: Option<(&str, bool)>) -> bool {
        let mut advanced = false;
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if self.step > 0
                && ui
                    .add(ActionButton::new("Back").icon(icons::CARET_LEFT).ghost())
                    .clicked()
            {
                self.step -= 1;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((label, enabled)) = next
                    && ui
                        .add_enabled(
                            enabled,
                            ActionButton::new(label).icon(icons::CARET_RIGHT).primary(),
                        )
                        .clicked()
                {
                    self.step += 1;
                    advanced = true;
                }
            });
        });
        advanced
    }

    fn heat(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let heater = Heater::Hotend(self.tool);
        let (current, target) = reading(ctx.state, heater);
        let ready = calibrate::within_target(current, target, ESTEPS_TEMP_TOLERANCE);
        kit::dim(ui, HEAT_HELP);
        ui.add_space(8.0);
        ui.add(
            NumericField::new(&mut self.temperature)
                .label("Temperature")
                .unit("°C")
                .decimals(0)
                .range(170.0..=300.0)
                .label_width(100.0)
                .field_width(110.0),
        );
        let presets: Vec<(String, f32)> = ctx
            .config
            .active()
            .temp_presets
            .iter()
            .map(|p| (p.name.clone(), p.hotend))
            .collect();
        ui.horizontal(|ui| {
            ui.add_space(112.0);
            if let Some(v) = preset_chips(ui, &presets, self.temperature) {
                self.temperature = v;
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let label = format!("Heat to {:.0} °C", self.temperature);
            if ui
                .add_enabled(
                    ctx.command_blocker().is_none(),
                    ActionButton::new(&label).icon(icons::HEATING),
                )
                .clicked()
            {
                ctx.printer.set_temperature(heater, self.temperature);
            }
            let (text, tone) = if ready {
                (
                    format!("Ready · {}", format::temperature(f64::from(current))),
                    Tone::Success,
                )
            } else if target > 0.0 {
                (
                    format!(
                        "Heating · {} / {:.0} °C",
                        format::fixed(f64::from(current), 1),
                        target
                    ),
                    Tone::Warning,
                )
            } else {
                (
                    format!("Off · {}", format::temperature(f64::from(current))),
                    Tone::Neutral,
                )
            };
            ui.add(StatusPill::new(&text, tone));
        });
        if !ready {
            ui.add_space(4.0);
            kit::caption(ui, "Continue once the hotend is within 2 °C of its target.");
        }
        self.nav(ui, Some(("Next", ready)));
    }

    fn mark(&mut self, ui: &mut Ui) {
        kit::dim(ui, MARK_HELP);
        ui.add_space(8.0);
        let max_mark = 300.0;
        ui.add(
            NumericField::new(&mut self.mark)
                .label("Mark at")
                .unit("mm")
                .decimals(0)
                .range((ESTEPS_EXTRUDE_MM + 5.0)..=max_mark)
                .label_width(100.0)
                .field_width(110.0)
                .hint("Distance between the mark and the extruder inlet"),
        );
        ui.add_space(6.0);
        diagram(ui, self.mark, None);
        let valid = self.mark.is_finite() && self.mark > ESTEPS_EXTRUDE_MM && self.mark <= max_mark;
        if self.nav(ui, Some(("Marked", valid))) {
            // A fresh mark: a new test extrusion.
            self.extrude_started = None;
            self.tested_steps = None;
        }
    }

    fn extrude(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let heater = Heater::Hotend(self.tool);
        let (current, target) = reading(ctx.state, heater);
        let hot = calibrate::within_target(current, target, ESTEPS_TEMP_TOLERANCE * 2.5);
        let secs = Self::extrusion_secs();
        kit::dim(
            ui,
            &format!(
                concat!(
                    "The printer extrudes {:.0} mm at {:.0} mm/min, which takes about {}. ",
                    "Slow extrusion keeps the hotend from limiting the flow."
                ),
                ESTEPS_EXTRUDE_MM,
                ESTEPS_FEEDRATE_MM_MIN,
                format::duration(secs)
            ),
        );
        ui.add_space(8.0);
        let now = ui.input(|i| i.time);
        // One extrusion per mark: a second one would push the mark past
        // what the measurement assumes (or into the extruder).
        let extruded = self.extrude_started.is_some();
        let blocker = ctx.motion_blocker();
        let can = blocker.is_none() && hot && !extruded;
        ui.horizontal(|ui| {
            let label = format!("Extrude {:.0} mm", ESTEPS_EXTRUDE_MM);
            let tip = if extruded {
                "Already extruded. To repeat the test, go back and mark the filament again."
            } else if let Some(reason) = blocker {
                reason
            } else if !hot {
                "The hotend is not at temperature"
            } else {
                "M83, G1 E100 F50"
            };
            if ui
                .add_enabled(
                    can,
                    ActionButton::new(&label)
                        .icon(icons::EXTRUDE)
                        .primary()
                        .tooltip(tip),
                )
                .clicked()
            {
                ctx.printer
                    .extrude(self.tool, ESTEPS_EXTRUDE_MM, ESTEPS_FEEDRATE_MM_MIN);
                self.extrude_started = Some(now);
                self.tested_steps = current_esteps(&ctx.state.settings, self.tool);
                // Until measured, what a perfect extruder would leave: no
                // correction, whatever the mark distance.
                self.remaining = f64::from(self.mark - ESTEPS_EXTRUDE_MM).max(0.0);
            }
            if !hot && !extruded {
                ui.add(StatusPill::new("Hotend not hot", Tone::Warning));
            }
        });
        if let Some(start) = self.extrude_started {
            let elapsed = (now - start).max(0.0);
            let fraction = (elapsed / secs).clamp(0.0, 1.0) as f32;
            ui.add_space(8.0);
            let label = if fraction >= 1.0 {
                "Done: wait until the extruder stops".to_owned()
            } else {
                format!("Extruding · {} left", format::clock(secs - elapsed))
            };
            ui.add(
                LinearProgress::new(fraction)
                    .label(&label)
                    .show_percentage(true),
            );
            if fraction < 1.0 {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
        self.nav(ui, Some(("Measure", extruded)));
    }

    fn measure(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let reported = current_esteps(&ctx.state.settings, self.tool);
        // The measurement corrects the steps/mm the test ran with, not
        // whatever the printer reports once the result was applied.
        let current = self.tested_steps.or(reported);
        kit::dim(ui, MEASURE_HELP);
        ui.add_space(8.0);
        ui.add(
            NumericField::new(&mut self.remaining)
                .label("Remaining")
                .unit("mm")
                .decimals(1)
                .range(0.0..=f64::from(self.mark))
                .label_width(100.0)
                .field_width(110.0)
                .hint("Distance from the extruder inlet to the mark"),
        );
        ui.add_space(6.0);
        diagram(ui, self.mark, Some(self.remaining as f32));
        ui.add_space(8.0);
        let p = theme::ui_palette(ui);
        let result = current
            .ok_or(calibrate::EstepsError::InvalidSteps)
            .and_then(|c| {
                calibrate::esteps(c, ESTEPS_EXTRUDE_MM, self.mark, self.remaining as f32)
            });
        match result {
            Ok(r) => {
                let tone_color = if r.needs_double_check() {
                    Some(p.warning)
                } else {
                    None
                };
                kit::metric_grid(
                    ui,
                    4,
                    &[
                        (
                            "Extruded",
                            format!("{} mm", format::fixed(f64::from(r.extruded), 1)),
                            None,
                        ),
                        (
                            "Tested with",
                            current.map_or_else(
                                || format::MISSING.to_owned(),
                                |c| format::fixed(f64::from(c), 2),
                            ),
                            None,
                        ),
                        (
                            "New steps/mm",
                            format::fixed(f64::from(r.new_steps), 2),
                            Some(p.accent),
                        ),
                        (
                            "Change",
                            format!("{}%", format::signed(f64::from(r.change) * 100.0, 1)),
                            tone_color,
                        ),
                    ],
                );
                if r.needs_double_check() {
                    kit::notice(
                        ui,
                        Tone::Warning,
                        icons::WARNING,
                        "Large correction",
                        Some(LARGE_CHANGE_HELP),
                        None,
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let ready = ctx.command_blocker().is_none();
                    // This result was applied and the printer reports it (a
                    // new measurement needs applying again).
                    let target = (r.new_steps * 100.0).round() / 100.0;
                    let same = |v: f32| (v - target).abs() < 0.006;
                    let applied = self.applied.is_some_and(same) && reported.is_some_and(same);
                    let label = format!(
                        "Apply {} steps/mm",
                        format::fixed(f64::from(r.new_steps), 2)
                    );
                    if !applied
                        && ui
                            .add_enabled(
                                ready,
                                ActionButton::new(&label).icon(icons::CHECK).primary(),
                            )
                            .clicked()
                    {
                        let cmd = esteps_command(&ctx.state.settings, self.tool, r.new_steps);
                        ctx.printer.apply_setting(&cmd);
                        self.tested_steps = current;
                        self.applied = Some(target);
                        ctx.toasts
                            .success("E-steps applied")
                            .message("Run the test again to confirm, then save to EEPROM.");
                    }
                    let save = ActionButton::new("Save to EEPROM")
                        .icon(icons::SAVE)
                        .tooltip("M500: keep the new steps/mm after a restart");
                    let save = if applied { save.primary() } else { save };
                    let no_eeprom = kit::lacks_eeprom(ctx.state);
                    if ui
                        .add_enabled(ready && applied && !no_eeprom, save)
                        .on_disabled_hover_text(if no_eeprom {
                            kit::NO_EEPROM
                        } else {
                            "Apply the new steps/mm first"
                        })
                        .clicked()
                    {
                        ctx.printer.save_settings();
                        ctx.toasts.success("E-steps saved to EEPROM");
                    }
                });
            }
            Err(err) => {
                kit::notice(
                    ui,
                    Tone::Danger,
                    icons::ERROR,
                    "Cannot compute E-steps",
                    Some(err.message()),
                    None,
                );
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add(ActionButton::new("Back").icon(icons::CARET_LEFT).ghost())
                .clicked()
            {
                self.step -= 1;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(ActionButton::new("Start over").icon(icons::RESTART).ghost())
                    .clicked()
                {
                    let temperature = self.temperature;
                    *self = EstepsWizard {
                        temperature,
                        ..EstepsWizard::default()
                    };
                }
            });
        });
    }
}

/// A side view of the extruder and the filament with the mark and, once
/// measured, the remaining length.
fn diagram(ui: &mut Ui, mark: f32, remaining: Option<f32>) {
    let p = theme::ui_palette(ui);
    let width = ui.available_width().min(520.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 86.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let font = theme::font(ui, TextRole::Caption);
    // Extruder on the left, filament running right; the mark at `mark` mm.
    let extruder = Rect::from_min_size(pos2(rect.min.x + 4.0, rect.min.y + 18.0), vec2(56.0, 40.0));
    painter.rect(
        extruder,
        CornerRadius::same(theme::RADIUS),
        p.raised,
        Stroke::new(1.0, p.border_strong),
        egui::StrokeKind::Inside,
    );
    painter.text(
        extruder.center(),
        Align2::CENTER_CENTER,
        "Extruder",
        FontId::proportional(10.5),
        p.text_dim,
    );
    let y = extruder.center().y;
    let x0 = extruder.max.x;
    let x1 = rect.max.x - 8.0;
    let scale_mm = mark.max(1.0) * 1.15;
    let x_of = |mm: f32| x0 + (x1 - x0) * (mm / scale_mm).clamp(0.0, 1.0);
    painter.line_segment(
        [pos2(x0, y), pos2(x1, y)],
        Stroke::new(4.0, with_alpha(p.plot_color(3), 0.85)),
    );
    // The mark.
    let xm = x_of(match remaining {
        Some(r) if r.is_finite() => r,
        _ => mark,
    });
    painter.line_segment(
        [pos2(xm, y - 9.0), pos2(xm, y + 9.0)],
        Stroke::new(2.5, p.accent),
    );
    painter.text(
        pos2(xm, y - 12.0),
        Align2::CENTER_BOTTOM,
        "mark",
        font.clone(),
        p.accent,
    );
    // Dimension line.
    let yd = y + 22.0;
    painter.line_segment([pos2(x0, yd), pos2(xm, yd)], Stroke::new(1.0, p.text_dim));
    for x in [x0, xm] {
        painter.line_segment(
            [pos2(x, yd - 4.0), pos2(x, yd + 4.0)],
            Stroke::new(1.0, p.text_dim),
        );
    }
    let text = match remaining {
        Some(r) if r.is_finite() => format!("{} mm left", format::trimmed(f64::from(r), 1)),
        _ => format!("{} mm", format::trimmed(f64::from(mark), 0)),
    };
    painter.text(
        pos2((x0 + xm) / 2.0, yd + 4.0),
        Align2::CENTER_TOP,
        text,
        font.clone(),
        p.text,
    );
    if remaining.is_some() {
        // Where the mark started.
        let xs = x_of(mark);
        painter.circle_stroke(pos2(xs, y), 4.0, Stroke::new(1.0, p.text_faint));
        painter.text(
            pos2(xs, y - 12.0),
            Align2::CENTER_BOTTOM,
            "was",
            font,
            p.text_faint,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extrusion_takes_two_minutes() {
        assert!((EstepsWizard::extrusion_secs() - 120.0).abs() < 1e-6);
    }

    #[test]
    fn status_reflects_progress() {
        let mut w = EstepsWizard::default();
        assert_eq!(w.status(), None);
        w.step = 2;
        assert_eq!(w.status().map(|s| s.0), Some("Step 3 of 4".to_owned()));
        w.applied = Some(95.0);
        assert_eq!(w.status().map(|s| s.1), Some(Tone::Success));
    }
}
