//! The Sprint window on the built-in virtual printer, with a sample file
//! loaded: a safe playground that never touches a serial port, the user's
//! configuration or the desktop app's window state.
//!
//! ```text
//! cargo run -p sprint-gui --example gui_demo [FILE] [--fast]
//! ```
//!
//! `--fast` runs the virtual printer 20x faster than real time with
//! instant moves, so a whole print takes seconds.

use std::path::PathBuf;

use sprint_gui::app::AppOptions;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{AppConfig, ConnectTarget, PrinterProfile};

fn main() -> anyhow::Result<()> {
    let mut file = None;
    let mut fast = false;
    for arg in std::env::args_os().skip(1) {
        if arg == "--fast" {
            fast = true;
        } else {
            file = Some(PathBuf::from(arg));
        }
    }
    let file = file.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/calibration-cube.gcode")
    });
    let sim = if fast {
        SimConfig {
            time_scale: 20.0,
            instant_moves: true,
            ..SimConfig::default()
        }
    } else {
        SimConfig::default()
    };
    let profile = PrinterProfile {
        name: "Demo printer".into(),
        connection: ConnectTarget::Simulator(sim.clone()),
        ..PrinterProfile::default()
    };
    sprint_gui::run_app(AppOptions {
        file: Some(file),
        simulator: Some(sim),
        config: AppConfig {
            profiles: vec![profile],
            active_profile: 0,
            ..AppConfig::default()
        },
        native_dialogs: true,
        ..AppOptions::headless()
    })
}
