//! Calibration tab: PID autotune, E-steps and Z-offset wizards.

use crate::context::AppCtx;

/// Calibration tab: PID autotune, E-steps and Z-offset wizards.
#[derive(Default)]
pub struct CalibrationTab {}

impl CalibrationTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        let _ = ctx;
        ui.label("CalibrationTab is not implemented yet");
    }
}
