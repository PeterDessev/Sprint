//! Temperature graph tab: heater history, targets, power and statistics.

use crate::context::AppCtx;

/// Temperature graph tab: heater history, targets, power and statistics.
#[derive(Default)]
pub struct TemperatureGraphTab {}

impl TemperatureGraphTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        let _ = ctx;
        ui.label("TemperatureGraphTab is not implemented yet");
    }
}
