//! Helpers for the app shell tests (`tests/app_*.rs`): the real
//! [`SprintApp`] driven headless through `egui_kittest`, rendered with wgpu
//! when the machine has an adapter, against the built-in simulator.
//!
//! Snapshots are compared against `tests/snapshots/app_*.png`
//! (regenerate with `UPDATE_SNAPSHOTS=1`). They were made on Windows 10
//! with the reference fonts; on other machines, or without a GPU adapter,
//! the app still runs every frame but the image comparison is skipped.

#![allow(dead_code)]

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use egui::Vec2;
use egui_kittest::wgpu::WgpuTestRenderer;
use egui_kittest::{Harness, SnapshotError, SnapshotOptions};
use egui_wgpu::wgpu;
use sprint_gui::app::{AppOptions, SprintApp};
use sprint_gui::fonts;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{AppConfig, ConnectOptions, ConnectTarget, PrinterProfile};

/// Fonts the committed snapshots were rendered with (Windows 10).
const REFERENCE_FONTS: [&str; 3] = ["Segoe UI", "Segoe UI Semibold", "Cascadia Mono"];

/// The default window size of the snapshots.
pub const WINDOW: Vec2 = Vec2::new(1280.0, 800.0);

/// The wgpu setup: prefer a real GPU over CPU rasterizers (the DX12 WARP
/// adapter crashes on some Windows machines), Vulkan/Metal over DX12/GL.
fn wgpu_setup() -> egui_wgpu::WgpuSetup {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup
        .instance_descriptor
        .backends
        .remove(wgpu::Backends::BROWSER_WEBGPU);
    setup.native_adapter_selector = Some(Arc::new(|adapters, _surface| {
        let mut adapters: Vec<_> = adapters
            .iter()
            .filter(|a| {
                let info = a.get_info();
                !(info.backend == wgpu::Backend::Dx12 && info.device_type == wgpu::DeviceType::Cpu)
            })
            .collect();
        adapters.sort_by_key(|a| {
            let info = a.get_info();
            let backend = match info.backend {
                wgpu::Backend::Vulkan | wgpu::Backend::Metal => 0,
                wgpu::Backend::Dx12 => 1,
                wgpu::Backend::Gl => 2,
                _ => 3,
            };
            let device = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::Other | wgpu::DeviceType::VirtualGpu => 2,
                wgpu::DeviceType::Cpu => 3,
            };
            (backend, device)
        });
        adapters
            .first()
            .map(|a| (*a).clone())
            .ok_or_else(|| "no usable wgpu adapter".to_owned())
    }));
    egui_wgpu::WgpuSetup::CreateNew(setup)
}

/// The GPU every app of this test binary renders on, created once (`None`
/// when the machine has no usable adapter). A device per test meant dozens
/// of devices created at once by the parallel tests, which some drivers do
/// not survive.
fn shared_gpu() -> Option<egui_wgpu::WgpuSetupExisting> {
    static GPU: OnceLock<Option<(wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue)>> =
        OnceLock::new();
    let (instance, adapter, device, queue) = GPU
        .get_or_init(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let state = std::panic::catch_unwind(AssertUnwindSafe(|| {
                egui_kittest::wgpu::create_render_state(
                    wgpu_setup(),
                    egui_wgpu::RendererOptions::PREDICTABLE,
                )
            }))
            .ok();
            std::panic::set_hook(previous);
            state.map(|s| (s.instance, s.adapter, s.device, s.queue))
        })
        .clone()?;
    Some(egui_wgpu::WgpuSetupExisting {
        instance,
        adapter,
        device,
        queue,
    })
}

/// A wgpu renderer on the shared GPU, or `None` when the machine has no
/// usable adapter.
fn renderer() -> Option<WgpuTestRenderer> {
    let renderer = shared_gpu().and_then(|gpu| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let renderer = std::panic::catch_unwind(AssertUnwindSafe(|| {
            WgpuTestRenderer::from_setup(egui_wgpu::WgpuSetup::Existing(gpu))
        }))
        .ok();
        std::panic::set_hook(previous);
        renderer
    });
    if renderer.is_none() {
        eprintln!("no wgpu adapter: the app runs without the 3D view and snapshots are skipped");
    }
    renderer
}

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

/// The sample files of the repository.
pub fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

/// The app in a harness of `size`, with a wgpu render state when possible.
pub fn app(options: AppOptions, size: Vec2) -> Harness<'static, SprintApp> {
    // Frames of 1/60 s, so toasts and animations last as long (in frames)
    // as on a real display.
    let mut builder = Harness::builder()
        .with_size(size)
        .with_max_steps(400)
        .with_step_dt(1.0 / 60.0);
    if let Some(renderer) = renderer() {
        builder = builder.renderer(renderer);
    }
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
