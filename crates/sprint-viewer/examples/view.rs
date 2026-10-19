//! Interactive test bench for the toolpath viewer (eframe + wgpu).
//!
//! ```text
//! cargo run -p sprint-viewer --release --example view [-- [--color <mode>] [--stress <segments>]]
//! ```
//!
//! `--color` picks the initial color mode (feature, speed, width, height,
//! tool, fan, layer, time); `--stress` starts with a synthetic toolpath of
//! that many segments instead of the demo print.
//!
//! Left panel: data set, color mode, style, view mode and presets, layer
//! range, scrubbing, progress (with solid or translucent ghosts), highlight,
//! overlays, level of detail and the legend. Drag with the left button to
//! orbit, right/middle (or Shift+left) to pan, wheel to zoom towards the
//! cursor, double-click to focus, keys 0-6 for presets and Z to fit.
//!
//! Set `SPRINT_VIEW_FRAMES=<n>` to close the window automatically after `n`
//! frames (smoke test); with `SPRINT_VIEW_SCREENSHOT=<file.png>` a
//! screenshot of the whole window is saved shortly before closing.

use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use sprint_gcode::Toolpath;
use sprint_viewer::{
    BedSpec, ColorMode, GcodeViewer, GhostStyle, RenderStyle, ViewMode, ViewPreset, demo,
};

struct App {
    viewer: GcodeViewer,
    toolpath: Arc<Toolpath>,
    scrub: bool,
    scrub_fraction: f32,
    progress: bool,
    progress_fraction: f32,
    last_click: Option<u32>,
    last_hover: Option<u32>,
    frames: u64,
    exit_after: Option<u64>,
    screenshot: Option<std::path::PathBuf>,
    last_frame: Instant,
    frame_ms: f32,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Result<App, String> {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .ok_or("this example needs the wgpu renderer")?;
        let mut viewer = GcodeViewer::new(render_state);
        viewer.set_bed(BedSpec::default());
        let args: Vec<String> = std::env::args().collect();
        let arg = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let toolpath = Arc::new(match arg("--stress").and_then(|n| n.parse().ok()) {
            Some(n) => demo::stress_toolpath(n),
            None => demo::demo_toolpath(),
        });
        viewer.set_toolpath(Arc::clone(&toolpath));
        if let Some(name) = arg("--color") {
            let wanted = name.to_lowercase();
            match ColorMode::ALL.into_iter().find(|m| {
                let n = m.name().to_lowercase();
                n.starts_with(&wanted) || n.replace(' ', "").contains(&wanted)
            }) {
                Some(mode) => viewer.options_mut().color_mode = mode,
                None => eprintln!("unknown color mode {name:?}"),
            }
        }
        Ok(App {
            viewer,
            toolpath,
            scrub: false,
            scrub_fraction: 1.0,
            progress: false,
            progress_fraction: 0.5,
            last_click: None,
            last_hover: None,
            frames: 0,
            exit_after: std::env::var("SPRINT_VIEW_FRAMES")
                .ok()
                .and_then(|v| v.parse().ok()),
            screenshot: std::env::var_os("SPRINT_VIEW_SCREENSHOT").map(Into::into),
            last_frame: Instant::now(),
            frame_ms: 0.0,
        })
    }

    fn load(&mut self, toolpath: Toolpath) {
        self.toolpath = Arc::new(toolpath);
        self.viewer.set_toolpath(Arc::clone(&self.toolpath));
        self.last_click = None;
        self.viewer.options_mut().highlight_line = None;
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sprint viewer");
        ui.horizontal(|ui| {
            if ui.button("Demo").clicked() {
                self.load(demo::demo_toolpath());
            }
            if ui.button("Stress 1M").clicked() {
                self.load(demo::stress_toolpath(1_000_000));
            }
            if ui.button("Stress 5M").clicked() {
                self.load(demo::stress_toolpath(5_000_000));
            }
            if ui.button("Clear").clicked() {
                self.viewer.clear();
            }
        });
        ui.label(format!(
            "{} segments, {} layers{}",
            self.toolpath.segments.len(),
            self.viewer.layer_count(),
            if self.viewer.is_loading() {
                " (loading)"
            } else {
                ""
            }
        ));
        ui.separator();

        let options = self.viewer.options_mut();
        egui::ComboBox::from_label("Color")
            .selected_text(options.color_mode.name())
            .show_ui(ui, |ui| {
                for mode in ColorMode::ALL {
                    ui.selectable_value(&mut options.color_mode, mode, mode.name());
                }
            });
        ui.horizontal(|ui| {
            for style in RenderStyle::ALL {
                ui.radio_value(&mut options.style, style, style.name());
            }
            ui.separator();
            for mode in ViewMode::ALL {
                ui.radio_value(&mut options.view_mode, mode, mode.name());
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut options.show_travel, "Travel");
            ui.checkbox(&mut options.show_retractions, "Retractions");
            ui.checkbox(&mut options.show_bed, "Bed");
            ui.checkbox(&mut options.show_build_volume, "Volume");
            ui.checkbox(&mut options.show_axes, "Axes");
            ui.checkbox(&mut options.show_tool_head, "Tool head");
        });
        ui.horizontal(|ui| {
            ui.label("Ghosts");
            for style in GhostStyle::ALL {
                ui.radio_value(&mut options.ghost_style, style, style.name());
            }
        });
        ui.horizontal_wrapped(|ui| {
            for preset in ViewPreset::ALL {
                if ui.button(preset.name()).clicked() {
                    self.viewer.set_view(preset);
                }
            }
            if ui.button("Fit").clicked() {
                self.viewer.fit_to_view();
            }
        });
        ui.separator();

        let layers = self.viewer.layer_count();
        if layers > 0 {
            let options = self.viewer.options_mut();
            let last = layers - 1;
            let (mut lo, mut hi) = options.clamped_layer_range(layers).unwrap_or((0, last));
            ui.label("Layers");
            ui.add(egui::Slider::new(&mut hi, 0..=last).text("top"));
            ui.add(egui::Slider::new(&mut lo, 0..=last).text("bottom"));
            options.layer_range = (lo.min(hi), hi);
            let top = self.toolpath.layers.get(hi).cloned();
            ui.checkbox(&mut self.scrub, "Scrub inside the top layer");
            if self.scrub {
                ui.add(egui::Slider::new(&mut self.scrub_fraction, 0.0..=1.0).text("moves"));
            }
            let options = self.viewer.options_mut();
            options.line_limit = match (&top, self.scrub) {
                (Some(l), true) => {
                    let span = l.lines.end.saturating_sub(l.lines.start) as f32;
                    Some(l.lines.start + (span * self.scrub_fraction) as u32)
                }
                _ => None,
            };
            ui.checkbox(&mut self.progress, "Print progress");
            if self.progress {
                ui.add(egui::Slider::new(&mut self.progress_fraction, 0.0..=1.0).text("done"));
            }
            let lines = self.toolpath.line_count;
            self.viewer.options_mut().progress_line = self
                .progress
                .then_some((lines as f32 * self.progress_fraction) as u32);
        }
        let mut lod = self.viewer.lod_pixels();
        if ui
            .add(egui::Slider::new(&mut lod, 0.0..=20.0).text("LOD px"))
            .on_hover_text("Beads narrower than this on screen are drawn as ribbons")
            .changed()
        {
            self.viewer.set_lod_pixels(lod);
        }
        ui.separator();
        self.viewer.legend_ui(ui);
        ui.separator();
        ui.label(format!(
            "hover: {}   selected: {}",
            self.last_hover.map_or("-".into(), |l| (l + 1).to_string()),
            self.last_click.map_or("-".into(), |l| (l + 1).to_string())
        ));
        ui.label(format!("frame: {:.1} ms", self.frame_ms));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.frame_ms = self.frame_ms * 0.9 + dt * 0.1;
        self.last_frame = now;

        egui::Panel::left("controls")
            .resizable(true)
            .default_size(330.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui));
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let response = self.viewer.ui(ui);
            self.last_hover = response.hovered_line;
            if let Some(line) = response.clicked_line {
                self.last_click = Some(line);
                self.viewer.options_mut().highlight_line = Some(line);
            }
        });

        self.frames += 1;
        if let Some(limit) = self.exit_after {
            ui.ctx().request_repaint();
            if self.screenshot.is_some() && self.frames + 20 == limit {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            let shot = ui.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                    _ => None,
                })
            });
            if let Some(image) = shot
                && let Some(path) = self.screenshot.take()
            {
                let [w, h] = image.size;
                match image::save_buffer(
                    &path,
                    image.as_raw(),
                    w as u32,
                    h as u32,
                    image::ColorType::Rgba8,
                ) {
                    Ok(()) => println!("screenshot saved to {}", path.display()),
                    Err(err) => eprintln!("screenshot failed: {err}"),
                }
            }
            if self.frames >= limit {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_title("Sprint toolpath viewer"),
        ..Default::default()
    };
    eframe::run_native(
        "sprint-viewer",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)?))),
    )
}
