//! GPU toolpath viewer widget for egui, rendered with wgpu.
//! See `ARCHITECTURE.md` for the public contract.
//!
//! The crate is split into a GPU-agnostic core ([`camera`], [`color`],
//! [`instance`], [`pick`], [`scene`]) that is unit-tested without a GPU, and
//! a wgpu renderer driven by [`GcodeViewer`]. The scene is drawn into an
//! offscreen 4x MSAA target and shown as an egui image; it is re-rendered
//! only when the camera, options, data or size change.
//!
//! ```no_run
//! # fn demo(render_state: &egui_wgpu::RenderState, ui: &mut egui::Ui) {
//! use std::sync::Arc;
//! let mut viewer = sprint_viewer::GcodeViewer::new(render_state);
//! viewer.set_toolpath(Arc::new(sprint_viewer::demo::demo_toolpath()));
//! let response = viewer.ui(ui);
//! if let Some(line) = response.clicked_line {
//!     println!("clicked G-code line {}", line + 1);
//! }
//! # }
//! ```

pub mod camera;
pub mod color;
pub mod demo;
mod gpu;
pub mod instance;
mod options;
pub mod pick;
pub mod scene;
mod viewer;

pub use options::{BedSpec, ColorMode, RenderStyle, ViewMode, ViewPreset, ViewerOptions};
pub use viewer::{DEFAULT_LOD_PIXELS, GcodeViewer, ViewerResponse, headless_device};

/// Re-export of the wgpu crate used by the viewer (via egui-wgpu).
pub use egui_wgpu::wgpu;
