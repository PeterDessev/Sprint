//! Sprint desktop GUI.
//! See `ARCHITECTURE.md` for the public contract.
//!
//! [`run`] opens the main window; [`app::SprintApp`] is the eframe app
//! (usable on its own, e.g. in tests with `egui_kittest`).

use std::path::PathBuf;
use std::sync::Arc;

use sprint_host::ConnectTarget;

pub mod app;
pub mod app_icon;
pub mod config_store;
pub mod connection;
pub mod context;
pub mod dialogs;
pub mod fonts;
pub mod format;
pub mod fsutil;
pub mod icons;
pub mod insight;
pub mod job_text;
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
    /// Connect to this printer at startup (`sprint gui --connect T`); it
    /// wins over `simulator` and the active profile's `auto_connect`.
    pub connect: Option<ConnectTarget>,
}

/// Initial window size in points.
pub const WINDOW_SIZE: [f32; 2] = [1440.0, 900.0];
/// Smallest window size in points.
pub const MIN_WINDOW_SIZE: [f32; 2] = [1024.0, 640.0];

/// The native window options: title, size, icon, drag and drop. `app_id`
/// also names the folder where eframe keeps the window and UI state.
pub fn native_options(app_id: &str) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sprint")
            .with_app_id(app_id)
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
/// The configuration is read from [`sprint_host::AppConfig::path`] (the
/// file `SPRINT_CONFIG` names, when set) and saved back there. At startup
/// the app connects only to `options.connect`, else to the built-in
/// virtual printer with `options.simulator`, else to the active profile's
/// printer when the profile enables `auto_connect`; it never opens a port
/// otherwise.
pub fn run(options: GuiOptions) -> anyhow::Result<()> {
    run_app(app::AppOptions::desktop(options))
}

/// Open the window with explicit [`app::AppOptions`] (another
/// configuration, an in-memory one for demos, ...) and block until it
/// closes. Options that do not restore the UI preferences also keep the
/// window state apart from the desktop app's ("sprint-demo").
pub fn run_app(options: app::AppOptions) -> anyhow::Result<()> {
    let app_id = if options.restore_prefs {
        "sprint"
    } else {
        "sprint-demo"
    };
    eframe::run_native(
        "Sprint",
        native_options(app_id),
        Box::new(move |cc| Ok(Box::new(app::SprintApp::new(cc, options)))),
    )
    .map_err(|e| anyhow::anyhow!("could not open the Sprint window: {e}"))
}
