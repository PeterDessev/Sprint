//! GPU toolpath viewer widget for egui, rendered with wgpu.
//! See `ARCHITECTURE.md` for the public contract.

pub mod camera;
pub mod color;
pub mod instance;
mod options;
pub mod pick;

pub use options::{BedSpec, ColorMode, RenderStyle, ViewMode, ViewPreset, ViewerOptions};
