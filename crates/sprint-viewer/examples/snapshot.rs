//! Render the procedural demo toolpath headlessly and write PNG snapshots
//! for several color modes and views.
//!
//! ```text
//! cargo run -p sprint-viewer --example snapshot [-- <output dir>]
//! ```
//!
//! Output goes to `<cargo target dir>/viewer-snapshots/` unless a directory
//! is given (or `SPRINT_SNAPSHOT_DIR` is set). No window is opened.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use sprint_viewer::{
    BedSpec, ColorMode, GcodeViewer, RenderStyle, ViewMode, ViewPreset, ViewerOptions, demo,
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;

/// The cargo target directory this example was built into.
fn target_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        // <target>/<profile>/examples/snapshot(.exe)
        if let Some(dir) = exe.ancestors().nth(3)
            && dir.join("CACHEDIR.TAG").exists()
        {
            return dir.to_path_buf();
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}

fn output_dir() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    if let Ok(dir) = std::env::var("SPRINT_SNAPSHOT_DIR") {
        return PathBuf::from(dir);
    }
    target_dir().join("viewer-snapshots")
}

fn save(
    viewer: &mut GcodeViewer,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let Some((w, h, pixels)) = viewer.render_to_image(WIDTH, HEIGHT) else {
        return Err(format!("rendering {name} failed").into());
    };
    let path = dir.join(name);
    image::save_buffer(&path, &pixels, w, h, image::ColorType::Rgba8)?;
    println!(
        "{:<28} {w}x{h}  {:>6.1} ms  {}",
        name,
        start.elapsed().as_secs_f64() * 1000.0,
        path.display()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = output_dir();
    std::fs::create_dir_all(&dir)?;
    let Some((device, queue)) = sprint_viewer::headless_device() else {
        eprintln!("No GPU adapter available; nothing rendered.");
        return Ok(());
    };
    println!("Writing snapshots to {}", dir.display());

    let toolpath = Arc::new(demo::demo_toolpath());
    let last_layer = toolpath.layers.len().saturating_sub(1);
    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    let t = Instant::now();
    viewer.set_toolpath(Arc::clone(&toolpath));
    viewer.wait_until_loaded();
    println!(
        "Loaded {} segments, {} layers in {:.1} ms",
        toolpath.segments.len(),
        toolpath.layers.len(),
        t.elapsed().as_secs_f64() * 1000.0
    );

    save(&mut viewer, &dir, "01_feature_iso.png")?;

    for (name, mode) in [
        ("02_speed.png", ColorMode::Speed),
        ("03_width.png", ColorMode::Width),
        ("04_layer_height.png", ColorMode::LayerHeight),
        ("05_tool.png", ColorMode::Tool),
        ("06_fan.png", ColorMode::Fan),
        ("07_layer.png", ColorMode::Layer),
        ("08_time.png", ColorMode::Time),
    ] {
        viewer.options_mut().color_mode = mode;
        save(&mut viewer, &dir, name)?;
    }
    viewer.options_mut().color_mode = ColorMode::Feature;

    // Half-printed: layer range cut in the middle of the tower, scrubbed
    // inside the top layer, with the tool head.
    {
        let layer = 24.min(last_layer);
        let o = viewer.options_mut();
        o.layer_range = (0, layer);
        if let Some(l) = toolpath.layers.get(layer) {
            o.line_limit = Some(l.lines.start + (l.lines.end - l.lines.start) / 2);
        }
        save(&mut viewer, &dir, "09_layer_range_scrub.png")?;
    }

    // Print progress ghosting.
    {
        let o = viewer.options_mut();
        o.layer_range = (0, usize::MAX);
        o.line_limit = None;
        let mid = toolpath.layers.get(60).map(|l| l.lines.start);
        o.progress_line = mid;
        save(&mut viewer, &dir, "10_progress.png")?;
        viewer.options_mut().progress_line = None;
    }

    // Layer view of layer 20 with lower layers ghosted, travels and
    // retractions shown.
    {
        let o = viewer.options_mut();
        o.view_mode = ViewMode::LayerTop;
        o.layer_range = (0, 20.min(last_layer));
        o.show_travel = true;
        o.show_retractions = true;
        save(&mut viewer, &dir, "11_layer_top.png")?;
        let o = viewer.options_mut();
        o.view_mode = ViewMode::Perspective;
        o.layer_range = (0, usize::MAX);
    }

    // Travels and retractions in 3D, up to the top of the tower.
    {
        viewer.options_mut().layer_range = (0, 39.min(last_layer));
        save(&mut viewer, &dir, "12_travel_retractions.png")?;
        let o = viewer.options_mut();
        o.show_travel = false;
        o.show_retractions = false;
        o.layer_range = (0, usize::MAX);
    }

    // Lines style.
    viewer.options_mut().style = RenderStyle::Lines;
    save(&mut viewer, &dir, "13_lines.png")?;
    viewer.options_mut().style = RenderStyle::Tubes;

    // Presets.
    for (name, preset) in [
        ("14_front.png", ViewPreset::Front),
        ("15_top.png", ViewPreset::Top),
        ("16_right.png", ViewPreset::Right),
    ] {
        viewer.set_view(preset);
        save(&mut viewer, &dir, name)?;
    }
    viewer.set_view(ViewPreset::Iso);

    // Highlight a line on the tower.
    {
        let line = toolpath
            .segments
            .iter()
            .find(|s| s.layer == 30 && s.feature == sprint_gcode::Feature::ExternalPerimeter)
            .map(|s| s.line);
        viewer.options_mut().highlight_line = line;
        save(&mut viewer, &dir, "17_highlight.png")?;
        viewer.options_mut().highlight_line = None;
    }

    // Close-up of the tower corner to judge bead shading.
    {
        let cam = viewer.camera_mut();
        cam.target = Vec3::new(70.0, 96.0, 8.0);
        cam.distance = 32.0;
        cam.yaw = -0.6;
        cam.pitch = 0.45;
        save(&mut viewer, &dir, "18_closeup.png")?;
        // Same view with every bead drawn as a ribbon impostor (LOD path).
        let lod = viewer.lod_pixels();
        viewer.set_lod_pixels(f32::MAX);
        save(&mut viewer, &dir, "18b_closeup_ribbons.png")?;
        viewer.set_lod_pixels(lod);
        let cam = viewer.camera_mut();
        cam.target = Vec3::new(138.0, 112.0, 26.0);
        cam.distance = 45.0;
        cam.yaw = 0.3;
        cam.pitch = 0.6;
        save(&mut viewer, &dir, "19_vase_rim.png")?;
    }

    // Hidden features: only perimeters.
    {
        viewer.fit_to_view();
        let o = viewer.options_mut();
        for f in sprint_gcode::Feature::ALL {
            o.set_feature_hidden(
                f,
                !matches!(
                    f,
                    sprint_gcode::Feature::ExternalPerimeter | sprint_gcode::Feature::Perimeter
                ),
            );
        }
        save(&mut viewer, &dir, "20_perimeters_only.png")?;
        *viewer.options_mut() = ViewerOptions::default();
    }

    // Empty bed with build volume, centre origin.
    {
        let mut empty = GcodeViewer::new_headless(&device, &queue);
        empty.set_bed(BedSpec {
            size: [250.0, 210.0],
            max_z: 220.0,
            origin_center: false,
        });
        empty.options_mut().show_build_volume = true;
        save(&mut empty, &dir, "21_empty_bed.png")?;
    }

    // Lightweight stress run to report load and frame time.
    {
        let stress = Arc::new(demo::stress_toolpath(1_000_000));
        let mut v = GcodeViewer::new_headless(&device, &queue);
        let t = Instant::now();
        v.set_toolpath(Arc::clone(&stress));
        v.wait_until_loaded();
        println!(
            "Stress: {} segments loaded in {:.0} ms",
            stress.segments.len(),
            t.elapsed().as_secs_f64() * 1000.0
        );
        save(&mut v, &dir, "22_stress_1m.png")?;
    }
    Ok(())
}
