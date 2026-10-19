//! Printer tab: firmware, capabilities, statistics, endstops, link quality and thermal health.

use crate::context::AppCtx;

/// Printer tab: firmware, capabilities, statistics, endstops, link quality and thermal health.
#[derive(Default)]
pub struct PrinterInfoTab {}

impl PrinterInfoTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        let _ = ctx;
        ui.label("PrinterInfoTab is not implemented yet");
    }
}
