//! Bed mesh tab: heatmap, statistics, probing and bed screw adjustment.

use crate::context::AppCtx;

/// Bed mesh tab: heatmap, statistics, probing and bed screw adjustment.
#[derive(Default)]
pub struct BedMeshTab {}

impl BedMeshTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        let _ = ctx;
        ui.label("BedMeshTab is not implemented yet");
    }
}
