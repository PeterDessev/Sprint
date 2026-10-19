//! The real widget inside egui, rendered through egui-wgpu by egui_kittest:
//! the viewer's offscreen image is shown in the window, hovering a travel
//! move shows its tooltip and clicking reports its line. A PNG of the whole
//! window is written next to the example snapshots
//! (`<target>/viewer-snapshots/33_kittest_travel_tooltip.png`) so it can be
//! inspected. Skips when no wgpu adapter is available.

use std::path::PathBuf;
use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state};
use glam::{Vec2, Vec3};
use sprint_gcode::{MoveKind, Toolpath};
use sprint_viewer::{GcodeViewer, ViewPreset, demo, headless_device, wgpu};

const SIZE: egui::Vec2 = egui::vec2(960.0, 640.0);

struct State {
    viewer: GcodeViewer,
    rect: egui::Rect,
    hovered: Option<u32>,
    clicked: Option<u32>,
}

/// Like kittest's default setup, but preferring a hardware GPU: the scene
/// renders much faster, and some software adapters (WARP on older Windows
/// 10 builds) crash while creating the device.
fn hardware_first_setup() -> egui_wgpu::WgpuSetup {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup
        .instance_descriptor
        .backends
        .remove(wgpu::Backends::BROWSER_WEBGPU);
    setup.native_adapter_selector = Some(Arc::new(|adapters, _surface| {
        let mut adapters: Vec<_> = adapters.iter().collect();
        adapters.sort_by_key(|a| match a.get_info().device_type {
            wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            wgpu::DeviceType::Other | wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Cpu => 3,
        });
        adapters
            .first()
            .map(|a| (*a).clone())
            .ok_or_else(|| "no adapter found".to_owned())
    }));
    egui_wgpu::WgpuSetup::CreateNew(setup)
}

fn snapshot_dir() -> PathBuf {
    // <target>/tmp -> <target>/viewer-snapshots
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    tmp.parent()
        .map_or(tmp.clone(), |t| t.to_path_buf())
        .join("viewer-snapshots")
}

/// Travels of a layer, longest first.
fn travels_of(tp: &Toolpath, layer: usize) -> Vec<sprint_gcode::Segment> {
    let mut travels: Vec<_> = tp
        .layers
        .get(layer)
        .and_then(|l| tp.segments.get(l.segments.clone()))
        .unwrap_or_default()
        .iter()
        .filter(|s| s.kind == MoveKind::Travel)
        .copied()
        .collect();
    travels.sort_by(|a, b| b.length().total_cmp(&a.length()));
    travels
}

#[test]
fn hovering_a_travel_in_the_real_ui_shows_its_tooltip() {
    if headless_device().is_none() {
        eprintln!("skipping UI test: no wgpu adapter available");
        return;
    }
    let render_state = create_render_state(
        hardware_first_setup(),
        egui_wgpu::RendererOptions::PREDICTABLE,
    );
    let toolpath = Arc::new(demo::demo_toolpath());
    let top = 39;
    let mut viewer = GcodeViewer::new(&render_state);
    viewer.set_toolpath(Arc::clone(&toolpath));
    viewer.wait_until_loaded();
    let o = viewer.options_mut();
    o.show_travel = true;
    o.layer_range = (0, top);
    viewer.set_view(ViewPreset::Top);

    let state = State {
        viewer,
        rect: egui::Rect::NOTHING,
        hovered: None,
        clicked: None,
    };
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_max_steps(120)
        .renderer(WgpuTestRenderer::from_render_state(render_state))
        .build_ui_state(
            |ui, s: &mut State| {
                let r = s.viewer.ui(ui);
                s.rect = r.response.rect;
                s.hovered = r.hovered_line;
                if r.clicked_line.is_some() {
                    s.clicked = r.clicked_line;
                }
            },
            state,
        );
    // Let the preset animation finish.
    harness.run_steps(40);

    // A travel of the top layer crossing empty bed, and a point of it where
    // nothing but the travel is under the cursor.
    let s = harness.state_mut();
    let size = Vec2::new(s.rect.width(), s.rect.height());
    s.viewer.options_mut().show_travel = false;
    let probe = |viewer: &GcodeViewer, travel: &sprint_gcode::Segment, t: f32| {
        let p = Vec3::from(travel.start).lerp(Vec3::from(travel.end), t);
        let q = viewer.camera().project_to_screen(p, size)?;
        let local = [q.x, q.y + 1.5];
        let inside =
            local[0] > 4.0 && local[1] > 4.0 && local[0] < size.x - 4.0 && local[1] < size.y - 4.0;
        (inside && viewer.pick_screen(local, size.to_array()).is_none()).then_some(local)
    };
    let (travel, local) = travels_of(&toolpath, top)
        .into_iter()
        .find_map(|travel| {
            let local = [0.5, 0.4, 0.6, 0.3, 0.7]
                .into_iter()
                .find_map(|t| probe(&s.viewer, &travel, t))?;
            Some((travel, local))
        })
        .expect("a travel crosses empty bed");
    s.viewer.options_mut().show_travel = true;
    let pos = s.rect.min + egui::vec2(local[0], local[1]);

    harness.hover_at(pos);
    harness.run_steps(3);
    // That travel, or one stacked above it on a lower layer's hop.
    let hovered = harness.state().hovered.expect("a travel under the cursor");
    let segment = toolpath
        .segments
        .iter()
        .find(|s| s.line == hovered)
        .expect("a move of the file");
    assert_eq!(segment.kind, MoveKind::Travel, "line {hovered}");
    assert!(segment.layer as usize <= top && travel.layer as usize <= top);
    // The travel tooltip, not a bead's.
    harness.get_by_label("Travel");
    harness.get_by_label("Length");
    assert!(harness.query_by_label("Width × height").is_none());

    let image = harness.render().expect("rendered");
    let dir = snapshot_dir();
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = image.save(dir.join("33_kittest_travel_tooltip.png"));
    }
    // The viewer's image is on screen: the orange parts and the bed.
    let orange = image
        .pixels()
        .filter(|p| p[0] > 150 && p[0] as i32 > p[2] as i32 + 70)
        .count();
    assert!(orange > 500, "only {orange} part pixels in the window");

    harness.drag_at(pos);
    harness.step();
    harness.drop_at(pos);
    harness.run_steps(2);
    assert_eq!(harness.state().clicked, Some(hovered));

    // Hiding travels: the spot is empty bed, no tooltip.
    harness.state_mut().viewer.options_mut().show_travel = false;
    harness.hover_at(pos + egui::vec2(0.5, 0.0));
    harness.run_steps(3);
    assert_eq!(harness.state().hovered, None);
    assert!(harness.query_by_label("Travel").is_none());
}
