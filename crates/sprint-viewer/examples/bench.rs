//! Headless performance check for big toolpaths.
//!
//! ```text
//! cargo run -p sprint-viewer --release --example bench [-- <segments>]
//! ```
//!
//! Reports how long `set_toolpath` blocks, the worst per-frame CPU cost while
//! the scene streams in, steady-state frame times at 1920x1080 for several
//! views and styles, and picking latency. No window is opened.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sprint_viewer::{GcodeViewer, RenderStyle, ViewMode, ViewPreset, demo, wgpu};

const W: u32 = 1920;
const H: u32 = 1080;

fn frame_time(viewer: &mut GcodeViewer, device: &wgpu::Device, frames: u32) -> Duration {
    for _ in 0..3 {
        viewer.render_offscreen(W, H);
    }
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let start = Instant::now();
    for _ in 0..frames {
        viewer.render_offscreen(W, H);
    }
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    start.elapsed() / frames
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let segments: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(5_000_000);
    let Some((device, queue)) = sprint_viewer::headless_device() else {
        eprintln!("No GPU adapter available.");
        return;
    };
    let t = Instant::now();
    let toolpath = Arc::new(demo::stress_toolpath(segments));
    println!(
        "generated {} segments / {} layers in {:.0} ms",
        toolpath.segments.len(),
        toolpath.layers.len(),
        ms(t.elapsed())
    );

    let mut viewer = GcodeViewer::new_headless(&device, &queue);
    // Like a running UI: targets exist before the file arrives.
    viewer.render_offscreen(W, H);
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let t = Instant::now();
    viewer.set_toolpath(Arc::clone(&toolpath));
    println!("set_toolpath returned after {:.2} ms", ms(t.elapsed()));

    // Simulated frames while loading: poll + draw what is uploaded so far.
    let start = Instant::now();
    let mut frames = 0;
    let mut worst = Duration::ZERO;
    let mut worst_poll = Duration::ZERO;
    let mut slow = 0;
    loop {
        let t = Instant::now();
        let loading = viewer.poll();
        worst_poll = worst_poll.max(t.elapsed());
        viewer.render_offscreen(W, H);
        worst = worst.max(t.elapsed());
        if t.elapsed() > Duration::from_micros(16_700) {
            slow += 1;
        }
        frames += 1;
        if !loading {
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    println!(
        "loaded in {:.0} ms over {frames} frames ({slow} over 16.7 ms); worst frame CPU time {:.1} ms (loading work {:.1} ms)",
        ms(start.elapsed()),
        ms(worst),
        ms(worst_poll)
    );

    let report = |name: &str, d: Duration| {
        println!(
            "{name:<34} {:>7.2} ms/frame ({:.0} fps)",
            ms(d),
            1.0 / d.as_secs_f64().max(1e-9)
        );
    };
    report(
        "tubes, iso, all layers",
        frame_time(&mut viewer, &device, 30),
    );
    let lod = viewer.lod_pixels();
    viewer.set_lod_pixels(0.0);
    report(
        "  prisms only (no LOD)",
        frame_time(&mut viewer, &device, 30),
    );
    viewer.set_lod_pixels(f32::MAX);
    report("  ribbons only", frame_time(&mut viewer, &device, 30));
    viewer.set_lod_pixels(lod);
    viewer.set_view(ViewPreset::Top);
    report(
        "tubes, top, all layers",
        frame_time(&mut viewer, &device, 30),
    );
    viewer.set_view(ViewPreset::Iso);
    viewer.options_mut().style = RenderStyle::Lines;
    report(
        "lines, iso, all layers",
        frame_time(&mut viewer, &device, 30),
    );
    viewer.options_mut().style = RenderStyle::Tubes;
    viewer.options_mut().show_travel = true;
    report("tubes + travel, iso", frame_time(&mut viewer, &device, 30));
    viewer.options_mut().show_travel = false;
    let layers = viewer.layer_count();
    viewer.options_mut().layer_range = (0, layers / 2);
    report(
        "tubes, iso, half the layers",
        frame_time(&mut viewer, &device, 30),
    );
    viewer.options_mut().view_mode = ViewMode::LayerTop;
    report(
        "layer view (ortho, ghosted)",
        frame_time(&mut viewer, &device, 30),
    );
    viewer.options_mut().view_mode = ViewMode::Perspective;
    viewer.options_mut().layer_range = (0, usize::MAX);

    // Picking latency over a grid of screen positions, beads only and with
    // travels shown (which also searches the travel index).
    for travels in [false, true] {
        viewer.options_mut().show_travel = travels;
        let mut worst = Duration::ZERO;
        let mut total = Duration::ZERO;
        let mut hits = 0;
        let n = 40;
        for i in 0..n {
            for j in 0..n {
                let pos = [
                    (i as f32 + 0.5) / n as f32 * W as f32,
                    (j as f32 + 0.5) / n as f32 * H as f32,
                ];
                let t = Instant::now();
                if viewer.pick_screen(pos, [W as f32, H as f32]).is_some() {
                    hits += 1;
                }
                let d = t.elapsed();
                worst = worst.max(d);
                total += d;
            }
        }
        println!(
            "pick{}: {} rays, {hits} hits, avg {:.3} ms, worst {:.3} ms",
            if travels { " (+ travels)" } else { "" },
            n * n,
            ms(total / (n * n)),
            ms(worst)
        );
    }
}
