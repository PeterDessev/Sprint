//! Tuning tab: catalog-driven EEPROM settings editor.

use crate::context::AppCtx;

/// Tuning tab: catalog-driven EEPROM settings editor.
#[derive(Default)]
pub struct TuningTab {}

impl TuningTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        let _ = ctx;
        ui.label("TuningTab is not implemented yet");
    }
}
