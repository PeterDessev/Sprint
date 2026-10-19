//! Sprint desktop GUI.
//! See `ARCHITECTURE.md` for the public contract.
//!
//! [`run`] opens the main window; [`app::SprintApp`] is the eframe app
//! (usable on its own, e.g. in tests with `egui_kittest`).

use std::path::PathBuf;
use std::sync::Arc;

pub mod app;
pub mod app_icon;
pub mod config_store;
pub mod connection;
pub mod context;
pub mod dialogs;
pub mod fonts;
pub mod format;
pub mod icons;
pub mod loader;
pub mod menu;
pub mod native_dialog;
pub mod notify;
pub mod panels;
pub mod prefs;
pub mod settings;
pub mod theme;
pub mod widgets;

/// Options for launching the GUI.
#[derive(Clone, Debug, Default)]
pub struct GuiOptions {
    /// G-code file to load at startup.
    pub file: Option<PathBuf>,
    /// Connect to the built-in virtual printer at startup.
    pub simulator: bool,
}

/// Initial window size in points.
pub const WINDOW_SIZE: [f32; 2] = [1440.0, 900.0];
/// Smallest window size in points.
pub const MIN_WINDOW_SIZE: [f32; 2] = [1024.0, 640.0];

/// The native window options: title, size, icon, drag and drop.
pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sprint")
            .with_app_id("sprint")
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE)
            .with_icon(Arc::new(app_icon::icon_data()))
            .with_drag_and_drop(true),
        persist_window: true,
        ..Default::default()
    }
}

/// Launch the GUI and block until the window closes.
///
/// The configuration is read from [`sprint_host::AppConfig::path`] and
/// saved back there. The app never opens a serial port by itself unless
/// the active profile enables `auto_connect`; `options.simulator` connects
/// to the built-in virtual printer instead.
pub fn run(options: GuiOptions) -> anyhow::Result<()> {
    let app_options = app::AppOptions::desktop(options);
    eframe::run_native(
        "Sprint",
        native_options(),
        Box::new(move |cc| Ok(Box::new(app::SprintApp::new(cc, app_options)))),
    )
    .map_err(|e| anyhow::anyhow!("could not open the Sprint window: {e}"))
}
