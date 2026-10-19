//! Left sidebar with printer controls: temperatures, motion, extruder, live tuning and macros.

use crate::context::AppCtx;

/// Left sidebar with printer controls: temperatures, motion, extruder, live tuning and macros.
#[derive(Default)]
pub struct ControlsSidebar {}

impl ControlsSidebar {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        let _ = ctx;
        ui.label("ControlsSidebar is not implemented yet");
    }
}
