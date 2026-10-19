//! GPU toolpath viewer widget for egui, rendered with wgpu.
//! See `ARCHITECTURE.md` for the public contract.
//!
//! The crate is split into a GPU-agnostic core ([`camera`], [`color`],
//! [`instance`], [`pick`], [`scene`]) that is unit-tested without a GPU, and
//! a wgpu renderer driven by [`GcodeViewer`]. The scene is drawn into an
//! offscreen 4x MSAA target and shown as an egui image; it is re-rendered
//! only when the camera, options, data or size change.
//!
//! Beads are lit by a camera-relative studio rig (key, fill, rim, sky and
//! ground) in linear space with a hue-preserving highlight shoulder and
//! gentle depth cueing. Each bead also has a macro normal, the mean normal
//! of what its neighbours leave visible (sides of walls, tops of solid
//! infill), and shading blends from it to the bead's rounded profile as the
//! bead grows on screen: walls shade by the way they face at any distance,
//! layers thinner than a pixel do not alias into moire, and the prism and
//! ribbon levels of detail match. Ghosted geometry (not yet printed, or
//! below the layer in the layer view) is drawn solid and dim by default or
//! translucent ([`GhostStyle`]); the tool head is a small nozzle with a soft
//! shadow, and the layer being printed is highlighted.
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

#![warn(missing_docs)]

pub mod camera;
pub mod color;
pub mod demo;
mod gpu;
pub mod instance;
mod options;
pub mod pick;
pub mod scene;
mod viewer;

pub use options::{
    BedSpec, ColorMode, GhostStyle, RenderStyle, ViewMode, ViewPreset, ViewerOptions,
};
pub use viewer::{DEFAULT_LOD_PIXELS, GcodeViewer, ViewerResponse, headless_device};

/// Re-export of the wgpu crate used by the viewer (via egui-wgpu).
pub use egui_wgpu::wgpu;
