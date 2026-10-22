//! Helpers for the app shell tests (`tests/app_*.rs`): the real
//! [`SprintApp`] driven headless through `egui_kittest`, rendered with wgpu
//! when the machine has an adapter, against the built-in simulator.
//!
//! Snapshots are compared against `tests/snapshots/app_*.png`
//! (regenerate with `UPDATE_SNAPSHOTS=1`). They were made on Windows 10
//! with the reference fonts; on other machines, or without a GPU adapter,
//! the app still runs every frame but the image comparison is skipped.
//! Without the reference fonts the app uses egui's bundled fonts, so the
//! layout tests measure the same text everywhere, and `SPRINT_TEST_NO_GPU`
//! runs it as without an adapter (CI).

#![allow(dead_code)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::Vec2;
use egui_kittest::{Harness, SnapshotError, SnapshotOptions};
use sprint_gui::app::{AppOptions, SprintApp};
use sprint_gui::fonts;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{AppConfig, ConnectOptions, ConnectTarget, PrinterProfile};

#[path = "../gpu_support/mod.rs"]
mod gpu_support;

/// Fonts the committed snapshots were rendered with (Windows 10).
const REFERENCE_FONTS: [&str; 3] = ["Segoe UI", "Segoe UI Semibold", "Cascadia Mono"];

/// The default window size of the snapshots.
pub const WINDOW: Vec2 = Vec2::new(1280.0, 800.0);

/// A fast virtual printer: instant moves, quick heating, short boot.
pub fn fast_sim() -> SimConfig {
    SimConfig {
        instant_moves: true,
        time_scale: 200.0,
        boot_delay_ms: 10,
        ..SimConfig::default()
    }
}

/// Headless options whose active profile is the fast virtual printer.
pub fn sim_options() -> AppOptions {
    let profile = PrinterProfile {
        name: "Test printer".into(),
        connection: ConnectTarget::Simulator(fast_sim()),
        options: ConnectOptions {
            max_in_flight: 4,
            ..ConnectOptions::default()
        },
        ..PrinterProfile::default()
    };
    AppOptions {
        config: AppConfig {
            profiles: vec![profile],
            active_profile: 0,
            ..AppConfig::default()
        },
        ..AppOptions::headless()
    }
}

/// The sample files of the repository (full paths, as the app keeps
/// them).
pub fn sample(name: &str) -> PathBuf {
    sprint_gui::loader::absolute_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples")
            .join(name),
    )
}

/// Whether the tests render on a GPU (the 3D view needs one: without it
/// the viewer tab shows a notice instead of the scene and its cards).
pub fn gpu_available() -> bool {
    gpu_support::available()
}

/// Whether this machine has the fonts the snapshots were made with. Where
/// it has not, the tests use egui's bundled fonts, so that text measures
/// the same on every machine and the layout tests hold everywhere.
pub fn system_has_reference_fonts() -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| {
        // Tests the bundled fonts on a machine that has the reference ones.
        if std::env::var_os("SPRINT_TEST_BUNDLED_FONTS").is_some() {
            return false;
        }
        let (_, report) = fonts::system_definitions();
        [report.proportional, report.strong, report.monospace]
            .iter()
            .map(|f| f.as_deref())
            .eq(REFERENCE_FONTS.iter().map(|f| Some(*f)))
    })
}

/// The app in a harness of `size`, with a wgpu render state when possible
/// (and egui's bundled fonts without the reference fonts).
pub fn app(mut options: AppOptions, size: Vec2) -> Harness<'static, SprintApp> {
    options.bundled_fonts |= !system_has_reference_fonts();
    // Frames of 1/60 s, so toasts and animations last as long (in frames)
    // as on a real display.
    let mut builder = Harness::builder()
        .with_size(size)
        .with_max_steps(400)
        .with_step_dt(1.0 / 60.0);
    builder = match gpu_support::renderer() {
        Some(renderer) => builder.renderer(renderer),
        None => builder.renderer(gpu_support::NoRenderer),
    };
    let mut h = builder.build_eframe(move |cc| SprintApp::new(cc, options));
    // Fonts become active on the second pass.
    h.run_steps(3);
    h
}

/// Run frames until `done` holds (sleeping a little between frames so the
/// printer's threads make progress); false on timeout.
pub fn wait_until(
    h: &mut Harness<'_, SprintApp>,
    timeout: Duration,
    done: impl Fn(&SprintApp) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        h.step();
        if done(h.state()) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// [`wait_until`] that fails the test with `what` on timeout.
#[track_caller]
pub fn expect(h: &mut Harness<'_, SprintApp>, what: &str, done: impl Fn(&SprintApp) -> bool) {
    assert!(
        wait_until(h, Duration::from_secs(60), done),
        "timed out waiting for: {what}"
    );
}

/// Run a few frames (animations, layout settling).
pub fn settle(h: &mut Harness<'_, SprintApp>) {
    for _ in 0..20 {
        h.step();
    }
}

/// Whether this machine has the fonts the snapshots were made with.
pub fn has_reference_fonts(h: &Harness<'_, SprintApp>) -> bool {
    let Some(report) = fonts::report(&h.ctx) else {
        return false;
    };
    report.proportional.as_deref() == Some(REFERENCE_FONTS[0])
        && report.strong.as_deref() == Some(REFERENCE_FONTS[1])
        && report.monospace.as_deref() == Some(REFERENCE_FONTS[2])
}

/// Compare the window against `tests/snapshots/{name}.png` (see the
/// module docs). A full window has a little more anti-aliasing noise than
/// a single widget, hence the slightly wider tolerance.
pub fn snapshot(h: &mut Harness<'_, SprintApp>, name: &str) {
    if !gpu_support::available() {
        eprintln!("snapshot {name}: no GPU adapter, rendering and comparison skipped");
        return;
    }
    if !has_reference_fonts(h) {
        eprintln!("snapshot {name}: system fonts differ from the reference, comparison skipped");
        if let Err(err) = h.render() {
            eprintln!("snapshot {name}: rendering unavailable: {err}");
        }
        return;
    }
    let options = SnapshotOptions::new().threshold(1.0).max_failed_pixels(300);
    match h.try_snapshot_options(name, &options) {
        Ok(()) => {}
        Err(SnapshotError::RenderError { err }) => {
            eprintln!("snapshot {name}: rendering unavailable ({err}), comparison skipped");
        }
        Err(err) => panic!("{err}"),
    }
}
