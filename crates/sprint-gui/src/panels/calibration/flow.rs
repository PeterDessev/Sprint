//! Flow calibration helper (wall-thickness method): print a single-wall
//! cube, measure its walls, get the corrected flow.

use egui::Ui;

use crate::context::AppCtx;
use crate::icons;
use crate::insight::calibrate::{FlowError, flow_from_walls};
use crate::insight::{format, ui as kit};
use crate::theme::{self, Tone};
use crate::widgets::{ActionButton, Card, NumericField};

/// Names of the measured walls.
const WALLS: [&str; 4] = ["Front", "Right", "Back", "Left"];
/// Label and field widths of the wall fields.
const WALL_LABEL: f32 = 52.0;
const WALL_FIELD: f32 = 90.0;

/// The flow wizard.
#[derive(Debug)]
pub struct FlowWizard {
    /// Flow the test was printed with (%); `None` = take the printer's.
    current: Option<f32>,
    /// Expected wall width (mm); `None` = from the nozzle diameter.
    expected: Option<f32>,
    /// Measured walls (mm, 0 = not measured).
    walls: [f32; 4],
}

impl Default for FlowWizard {
    fn default() -> Self {
        FlowWizard {
            current: None,
            expected: None,
            walls: [0.0; 4],
        }
    }
}

/// Tooltip of the flow field.
const FLOW_HINT: &str = "The flow (extrusion multiplier) the test cube was printed with";
/// Tooltip of the expected wall field.
const EXPECTED_HINT: &str = "The extrusion width set in the slicer for the perimeter";

/// How to print and measure the test cube.
fn instructions(wall: f32) -> String {
    format!(
        "Print a 20 mm cube in vase mode (one perimeter, no top layers) with an extrusion \
         width of {} mm. Measure each wall with calipers, away from the corners and the seam.",
        format::trimmed(f64::from(wall), 2)
    )
}

/// Where to make a new flow permanent.
fn permanent_fix(flow: f32) -> String {
    format!(
        "For a permanent fix set the slicer's extrusion multiplier to {}. M221 below changes \
         the flow on the printer until the next restart.",
        format::fixed(f64::from(flow) / 100.0, 3)
    )
}

/// The usual single-wall extrusion width for a nozzle (112.5%).
pub fn default_wall_width(nozzle: f32) -> f32 {
    let nozzle = if nozzle.is_finite() && nozzle > 0.05 {
        nozzle
    } else {
        0.4
    };
    (nozzle * 1.125 * 100.0).round() / 100.0
}

impl FlowWizard {
    /// Draw the wizard.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let tool = ctx.state.active_tool;
        let printer_flow = ctx
            .state
            .flow_pct
            .get(usize::from(tool))
            .copied()
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(100.0);
        let nozzle = ctx.config.active().nozzle_diameter;
        let mut current = self.current.unwrap_or(printer_flow);
        let mut expected = self.expected.unwrap_or_else(|| default_wall_width(nozzle));
        Card::new("Flow")
            .icon(icons::FLOW)
            .subtitle("wall-thickness method")
            .show(ui, |ui| {
                kit::dim(ui, &instructions(expected));
                ui.add_space(8.0);
                let label_w = 150.0;
                if ui
                    .add(
                        NumericField::new(&mut current)
                            .label("Flow used")
                            .unit("%")
                            .decimals(1)
                            .range(10.0..=300.0)
                            .label_width(label_w)
                            .field_width(110.0)
                            .hint(FLOW_HINT),
                    )
                    .changed()
                {
                    self.current = Some(current);
                }
                if ui
                    .add(
                        NumericField::new(&mut expected)
                            .label("Expected wall")
                            .unit("mm")
                            .decimals(2)
                            .range(0.1..=3.0)
                            .label_width(label_w)
                            .field_width(110.0)
                            .hint(EXPECTED_HINT),
                    )
                    .changed()
                {
                    self.expected = Some(expected);
                }
                ui.add_space(6.0);
                kit::caption(ui, "MEASURED WALLS (leave 0 to skip)");
                // Four fields per row when they fit, else two (a 2x2 grid).
                let cell = WALL_LABEL + 6.0 + WALL_FIELD;
                let gap = 16.0;
                let per_row = if ui.available_width() >= 4.0 * cell + 3.0 * gap {
                    4
                } else if ui.available_width() >= 2.0 * cell + gap {
                    2
                } else {
                    1
                };
                for (r, row) in self.walls.chunks_mut(per_row).enumerate() {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for (c, wall) in row.iter_mut().enumerate() {
                            ui.add(
                                NumericField::new(wall)
                                    .label(WALLS[r * per_row + c])
                                    .unit("mm")
                                    .decimals(2)
                                    .range(0.0..=5.0)
                                    .label_width(WALL_LABEL)
                                    .field_width(WALL_FIELD),
                            );
                        }
                    });
                }
                ui.add_space(8.0);
                let p = theme::ui_palette(ui);
                match flow_from_walls(current, expected, &self.walls) {
                    Ok(r) => {
                        kit::metric_grid(
                            ui,
                            3,
                            &[
                                (
                                    "Average wall",
                                    format!("{} mm", format::fixed(f64::from(r.measured), 3)),
                                    None,
                                ),
                                (
                                    "New flow",
                                    format!("{} %", format::fixed(f64::from(r.new_flow), 1)),
                                    Some(p.accent),
                                ),
                                (
                                    "Change",
                                    format!("{}%", format::signed(f64::from(r.change) * 100.0, 1)),
                                    None,
                                ),
                            ],
                        );
                        ui.add_space(4.0);
                        kit::caption(ui, &permanent_fix(r.new_flow));
                        ui.add_space(6.0);
                        // Marlin takes M221 S as a whole percentage.
                        let percent = r.new_flow.round();
                        let label = format!(
                            "Set flow to {} % (M221)",
                            format::fixed(f64::from(percent), 0)
                        );
                        if ui
                            .add_enabled(
                                ctx.connected(),
                                ActionButton::new(&label).icon(icons::FLOW),
                            )
                            .clicked()
                        {
                            ctx.printer.set_flow(tool, percent);
                            // The walls were printed with the old flow: keep
                            // computing from it, not from the new one the
                            // printer reports from now on.
                            self.current = Some(current);
                            ctx.toasts.success(format!("Flow set to {percent:.0}%"));
                        }
                    }
                    Err(err) => {
                        let tone = if matches!(err, FlowError::NoMeasurement) {
                            Tone::Info
                        } else {
                            Tone::Warning
                        };
                        kit::notice(ui, tone, icons::RULER, err.message(), None, None);
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_width_from_nozzle() {
        assert_eq!(default_wall_width(0.4), 0.45);
        assert_eq!(default_wall_width(0.8), 0.9);
        assert_eq!(default_wall_width(f32::NAN), 0.45);
    }
}
