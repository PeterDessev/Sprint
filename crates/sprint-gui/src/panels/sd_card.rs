//! SD card tab: file list, printing, upload and deletion.

use crate::context::AppCtx;

/// SD card tab: file list, printing, upload and deletion.
#[derive(Default)]
pub struct SdCardTab {}

impl SdCardTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        let _ = ctx;
        ui.label("SdCardTab is not implemented yet");
    }
}
