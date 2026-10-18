//! GPU toolpath viewer widget for egui, rendered with wgpu.
//! See `ARCHITECTURE.md` for the public contract.

pub mod color;
mod options;

pub use options::{BedSpec, ColorMode, RenderStyle, ViewMode, ViewPreset, ViewerOptions};
