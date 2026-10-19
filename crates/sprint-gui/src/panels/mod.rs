//! Panels and tabs of the main window.
//!
//! Every panel is a `Default` struct with
//! `pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>)`; the app
//! shell owns one of each and decides where it goes.

pub mod bed_mesh;
pub mod calibration;
pub mod controls;
pub mod printer_info;
pub mod sd_card;
pub mod temp_graph;
pub mod tuning;

pub use bed_mesh::BedMeshTab;
pub use calibration::CalibrationTab;
pub use controls::ControlsSidebar;
pub use printer_info::PrinterInfoTab;
pub use sd_card::SdCardTab;
pub use temp_graph::TemperatureGraphTab;
pub use tuning::TuningTab;
