//! The GPU the tests of one test binary render on: created once and shared
//! by every harness (`tests/common` and `tests/app_support` include this
//! module). A wgpu device per test meant dozens of devices created at once
//! by the parallel tests, which some drivers do not survive
//! (`STATUS_ACCESS_VIOLATION` on Windows under load).

#![allow(dead_code)]

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, OnceLock};

use egui_kittest::wgpu::WgpuTestRenderer;
use egui_wgpu::wgpu;

/// The wgpu setup: prefer a real GPU over CPU rasterizers (the DX12 WARP
/// adapter crashes on some Windows machines), Vulkan/Metal over DX12.
/// OpenGL is left out: its context belongs to one thread, and the shared
/// device is used from every test thread, which drivers refuse (EGL
/// `BadAccess` on Linux, aborting the test binary). `WGPU_BACKEND` still
/// narrows the backends.
fn wgpu_setup() -> egui_wgpu::WgpuSetup {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup
        .instance_descriptor
        .backends
        .remove(wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL);
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

/// Run `f` with the panic message silenced: a missing adapter is a skip,
/// not a test failure.
fn quietly<T>(f: impl FnOnce() -> T) -> Option<T> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(AssertUnwindSafe(f)).ok();
    std::panic::set_hook(previous);
    result
}

/// `SPRINT_TEST_NO_GPU` (set, any value) makes the GPU tests run as on a
/// machine without an adapter: CI sets it, as software rasterizers on
/// runners vary and some crash.
pub fn gpu_disabled() -> bool {
    std::env::var_os("SPRINT_TEST_NO_GPU").is_some()
}

/// The shared GPU, created on first use (`None` when the machine has no
/// usable adapter, or with [`gpu_disabled`]).
fn shared_gpu() -> Option<egui_wgpu::WgpuSetupExisting> {
    if gpu_disabled() {
        return None;
    }
    static GPU: OnceLock<Option<(wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue)>> =
        OnceLock::new();
    let (instance, adapter, device, queue) = GPU
        .get_or_init(|| {
            quietly(|| {
                egui_kittest::wgpu::create_render_state(
                    wgpu_setup(),
                    egui_wgpu::RendererOptions::PREDICTABLE,
                )
            })
            .map(|s| (s.instance, s.adapter, s.device, s.queue))
        })
        .clone()?;
    Some(egui_wgpu::WgpuSetupExisting {
        instance,
        adapter,
        device,
        queue,
    })
}

/// Whether the shared GPU exists: without it the harnesses have no
/// renderer of ours, and must not render at all (egui_kittest would fall
/// back to its own wgpu setup, OpenGL included, which crashes on some
/// Linux machines).
pub fn available() -> bool {
    shared_gpu().is_some()
}

/// The renderer of a harness without the shared GPU: it drops the texture
/// updates and renders nothing. (egui_kittest's default would keep every
/// font atlas update until a render, then set up wgpu on its own, OpenGL
/// included.)
pub struct NoRenderer;

impl egui_kittest::TestRenderer for NoRenderer {
    fn handle_delta(&mut self, delta: &mut egui::TexturesDelta) {
        delta.clear();
    }

    fn render(
        &mut self,
        _ctx: &egui::Context,
        _output: &egui::FullOutput,
    ) -> Result<image::RgbaImage, String> {
        Err("no GPU adapter".to_owned())
    }
}

/// A wgpu renderer on the shared GPU (its own egui renderer, the device
/// shared), or `None` when the machine has no usable adapter.
pub fn renderer() -> Option<WgpuTestRenderer> {
    // Not `quietly`: swapping the global panic hook for every harness
    // would swallow the messages of tests failing meanwhile.
    let renderer = shared_gpu().and_then(|gpu| {
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            WgpuTestRenderer::from_setup(egui_wgpu::WgpuSetup::Existing(gpu))
        }))
        .ok()
    });
    if renderer.is_none() {
        eprintln!("no wgpu adapter: rendering is unavailable and snapshots are skipped");
    }
    renderer
}
