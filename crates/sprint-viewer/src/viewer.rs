//! [`GcodeViewer`]: the egui widget tying camera, picking, legend and the
//! wgpu renderer together.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use egui_wgpu::wgpu;
use glam::{Mat4, Vec2, Vec3};
use sprint_gcode::{Feature, Toolpath};

use crate::camera::{Camera, Frustum, Projection, Ray};
use crate::color::{
    self, Legend, LegendData, ScalarRanges, format_duration, format_value, ramp_color,
    srgb_to_linear, srgb_to_linear_rgb,
};
use crate::gpu::{self, DrawParams, Globals, Renderer, Slot, flags};
use crate::instance::{self, CLUSTER_SIZE, Cluster, LayerRanges, MAX_COORD, SceneData};
use crate::options::{BedSpec, ColorMode, RenderStyle, ViewMode, ViewPreset, ViewerOptions};
use crate::pick::{PickFilter, PickHit, PickIndex, PickKind};

/// Bytes of instance data uploaded per frame while a toolpath streams in
/// (a 5M-segment file needs about 15 frames).
const UPLOAD_BUDGET: usize = 16 << 20;
/// Duration of animated camera transitions (seconds).
const ANIM_SECONDS: f32 = 0.35;
/// Orbit speed in radians per point dragged.
const ORBIT_SPEED: f32 = 0.0085;
/// Retraction marker radius in pixels.
const MARKER_PX: f32 = 3.5;
/// Default bead width (pixels) below which beads are drawn as ribbons.
pub const DEFAULT_LOD_PIXELS: f32 = 3.0;
/// Pick tolerance in pixels for thin beads.
const PICK_TOLERANCE_PX: f32 = 2.5;
/// Pick tolerance in pixels around travel lines.
const TRAVEL_PICK_PX: f32 = 4.0;
/// Darkening of the grooves between resolved beads (0..1).
const CAVITY: f32 = 0.5;
/// Projected bead thickness (pixels) over which the grooves between beads
/// fade in. The bead's own rounded normal takes over from its macro normal
/// later, between half and two and a half times the upper bound (see
/// `bead_light` in the shader).
const DETAIL_PIXELS: (f32, f32) = (1.2, 4.5);
/// Strength of the depth cueing towards the background (0..1).
const DEPTH_CUE: f32 = 0.35;
/// Exposure applied to lit surfaces before the highlight shoulder.
const EXPOSURE: f32 = 1.18;

/// Camera-relative studio lights: direction towards the light (xyz) and
/// intensity (w).
#[derive(Clone, Copy, Debug, PartialEq)]
struct LightRig {
    key: [f32; 4],
    fill: [f32; 4],
    rim: [f32; 4],
}

impl LightRig {
    /// Key from the upper left front, fill from the right, rim from behind
    /// and above. Lights follow the camera (so every view is lit the same
    /// way) but lean towards world up so top surfaces read as tops.
    fn for_camera(cam: &Camera) -> LightRig {
        let (r, u, f) = (cam.right(), cam.up(), cam.forward());
        let dir = |v: Vec3, fallback: Vec3| v.normalize_or(fallback);
        let key = dir(r * -0.5 + u * 0.42 - f * 0.68 + Vec3::Z * 0.2, Vec3::Z);
        let fill = dir(r * 0.7 + u * 0.05 - f * 0.7, -f);
        let rim = dir(f * 0.8 + u * 0.6, Vec3::Z);
        let pack = |v: Vec3, w: f32| [v.x, v.y, v.z, w];
        LightRig {
            key: pack(key, 0.8),
            fill: pack(fill, 0.26),
            rim: pack(rim, 0.16),
        }
    }
}

/// What [`GcodeViewer::ui`] reports back.
#[derive(Clone, Debug)]
pub struct ViewerResponse {
    /// The egui response of the viewer area.
    pub response: egui::Response,
    /// Source line of the bead (or, while travels are shown, the travel
    /// move) clicked this frame.
    pub clicked_line: Option<u32>,
    /// Source line of the bead or shown travel move under the pointer.
    pub hovered_line: Option<u32>,
}

enum Loaded {
    Scene(Box<SceneData>),
    /// A pick index over extrusions or travels (see [`PickIndex::kind`]).
    Pick(Box<PickIndex>),
}

struct Loader {
    rx: mpsc::Receiver<Loaded>,
    cancel: Arc<AtomicBool>,
}

/// CPU-side information about the loaded scene.
struct SceneInfo {
    layers: Vec<LayerRanges>,
    clusters: Vec<Cluster>,
    legend: LegendData,
    bounds: Option<([f32; 3], [f32; 3])>,
    pick: Option<PickIndex>,
    travel_pick: Option<PickIndex>,
}

struct Anim {
    from: Camera,
    to: Camera,
    t: f32,
    top: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct HoverQuery {
    pos: [f32; 2],
    view: [f32; 2],
    camera: Camera,
    filter: PickFilter,
    has_index: bool,
    /// Travels are shown and indexed.
    travels: bool,
}

#[derive(Clone, Copy, Debug)]
struct Hover {
    segment: usize,
    line: u32,
}

#[derive(Clone, Debug, PartialEq)]
struct FrameKey {
    params: DrawParams,
    uploaded: (usize, usize, usize),
}

/// GPU toolpath viewer widget (3D orbit view and top-down layer view).
pub struct GcodeViewer {
    renderer: Renderer,
    egui_renderer: Option<Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>>,
    texture_id: Option<egui::TextureId>,
    repaint_ctx: Option<egui::Context>,
    options: ViewerOptions,
    bed: BedSpec,
    camera: Camera,
    camera_top: Camera,
    top_fitted: bool,
    anim: Option<Anim>,
    auto_fit: bool,
    fit_aspect: f32,
    toolpath: Option<Arc<Toolpath>>,
    loader: Option<Loader>,
    info: Option<SceneInfo>,
    last_frame: Option<FrameKey>,
    hover: Option<Hover>,
    hover_query: Option<HoverQuery>,
    lod_pixels: f32,
}

impl GcodeViewer {
    /// Create a viewer that draws through eframe's wgpu render state.
    pub fn new(render_state: &egui_wgpu::RenderState) -> GcodeViewer {
        let mut viewer = GcodeViewer::new_headless(&render_state.device, &render_state.queue);
        viewer.egui_renderer = Some(Arc::clone(&render_state.renderer));
        viewer
    }

    /// Create a viewer from a raw wgpu device and queue, without egui
    /// integration (tests, thumbnails, servers): [`GcodeViewer::render_to_image`]
    /// works; [`GcodeViewer::ui`] reserves space and handles input but shows
    /// no image.
    pub fn new_headless(device: &wgpu::Device, queue: &wgpu::Queue) -> GcodeViewer {
        let renderer = Renderer::new(device, queue);
        let mut camera_top = Camera {
            projection: Projection::Orthographic,
            ..Camera::default()
        };
        camera_top.set_preset(ViewPreset::Top);
        let mut viewer = GcodeViewer {
            renderer,
            egui_renderer: None,
            texture_id: None,
            repaint_ctx: None,
            options: ViewerOptions::default(),
            bed: BedSpec::default(),
            camera: Camera::default(),
            camera_top,
            top_fitted: false,
            anim: None,
            auto_fit: true,
            fit_aspect: 16.0 / 10.0,
            toolpath: None,
            loader: None,
            info: None,
            last_frame: None,
            hover: None,
            hover_query: None,
            lod_pixels: DEFAULT_LOD_PIXELS,
        };
        viewer.fit_now(viewer.fit_aspect);
        viewer
    }

    /// Show a toolpath. Instance data is built on a background thread and
    /// streamed to the GPU over the next frames; the layer range is reset to
    /// all layers and the camera is fitted.
    pub fn set_toolpath(&mut self, toolpath: Arc<Toolpath>) {
        self.cancel_loader();
        self.renderer.clear_scene();
        self.info = None;
        self.hover = None;
        self.hover_query = None;
        self.last_frame = None;
        let layers = instance::layer_count(&toolpath);
        self.options.layer_range = (0, layers.saturating_sub(1));
        self.toolpath = Some(Arc::clone(&toolpath));
        self.auto_fit = true;
        self.top_fitted = false;
        self.anim = None;
        self.fit_now(self.fit_aspect);

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let job_cancel = Arc::clone(&cancel);
        let ctx = self.repaint_ctx.clone();
        let tp = Arc::clone(&toolpath);
        let spawned = std::thread::Builder::new()
            .name("sprint-viewer-build".to_owned())
            .spawn(move || {
                let Some(scene) = instance::build_scene(&tp, &job_cancel) else {
                    return;
                };
                if tx.send(Loaded::Scene(Box::new(scene))).is_err() {
                    return;
                }
                if let Some(ctx) = &ctx {
                    ctx.request_repaint();
                }
                for kind in [PickKind::Extrusions, PickKind::Travels] {
                    let Some(pick) = PickIndex::build_kind(&tp, kind, &job_cancel) else {
                        return;
                    };
                    if tx.send(Loaded::Pick(Box::new(pick))).is_err() {
                        return;
                    }
                    if let Some(ctx) = &ctx {
                        ctx.request_repaint();
                    }
                }
            });
        match spawned {
            Ok(_) => self.loader = Some(Loader { rx, cancel }),
            Err(err) => {
                log::warn!("sprint-viewer: cannot spawn the build thread ({err}); building inline");
                let never = AtomicBool::new(false);
                if let Some(scene) = instance::build_scene(&toolpath, &never) {
                    self.accept_scene(scene);
                }
                for kind in [PickKind::Extrusions, PickKind::Travels] {
                    if let Some(pick) = PickIndex::build_kind(&toolpath, kind, &never) {
                        self.handle_loaded(Loaded::Pick(Box::new(pick)));
                    }
                }
            }
        }
    }

    /// The toolpath being shown.
    pub fn toolpath(&self) -> Option<&Arc<Toolpath>> {
        self.toolpath.as_ref()
    }

    /// Remove the toolpath (the bed stays visible).
    pub fn clear(&mut self) {
        self.cancel_loader();
        self.renderer.clear_scene();
        self.toolpath = None;
        self.info = None;
        self.hover = None;
        self.hover_query = None;
        self.last_frame = None;
        self.options.layer_range = (0, usize::MAX);
        self.auto_fit = true;
        // Keep the orientation a running animation was heading for, but
        // stop it: it would otherwise end framing the removed toolpath.
        if let Some(anim) = self.anim.take() {
            if anim.top {
                self.camera_top = anim.to;
            } else {
                self.camera = anim.to;
            }
        }
        self.fit_now(self.fit_aspect);
    }

    /// Set the printer bed / build volume.
    pub fn set_bed(&mut self, bed: BedSpec) {
        self.bed = bed.sanitized();
        self.renderer.set_bed(&self.bed);
        self.last_frame = None;
        self.refit_if_auto();
    }

    /// The printer bed.
    pub fn bed(&self) -> BedSpec {
        self.bed
    }

    /// Current options.
    pub fn options(&self) -> &ViewerOptions {
        &self.options
    }

    /// Mutable options; changes apply on the next frame without re-uploading
    /// geometry.
    pub fn options_mut(&mut self) -> &mut ViewerOptions {
        &mut self.options
    }

    /// The 3D (perspective) camera.
    pub fn camera(&self) -> &Camera {
        &self.camera
    }

    /// Mutable 3D camera (disables automatic re-fitting).
    pub fn camera_mut(&mut self) -> &mut Camera {
        self.auto_fit = false;
        self.anim = None;
        &mut self.camera
    }

    /// Orient the 3D camera to a preset (animated in the widget). Switches
    /// from the layer view back to the 3D view.
    pub fn set_view(&mut self, preset: ViewPreset) {
        self.options.view_mode = ViewMode::Perspective;
        let mut to = self.camera;
        to.set_preset(preset);
        if self.auto_fit {
            let (min, max) = self.fit_box();
            to.fit_bounds(min, max, self.fit_aspect);
        }
        self.start_anim(to, false);
    }

    /// Fit the active camera to the toolpath (or the bed when empty).
    pub fn fit_to_view(&mut self) {
        self.auto_fit = true;
        let (min, max) = self.fit_box();
        let top = self.options.view_mode == ViewMode::LayerTop;
        let mut to = if top { self.camera_top } else { self.camera };
        to.fit_bounds(min, max, self.fit_aspect);
        self.start_anim(to, top);
    }

    /// Level of detail: beads narrower than `pixels` on screen are drawn as
    /// shaded ribbon impostors (2 triangles) instead of prisms (24). 0 forces
    /// prisms everywhere; a huge value forces ribbons. Default
    /// [`DEFAULT_LOD_PIXELS`].
    pub fn set_lod_pixels(&mut self, pixels: f32) {
        self.lod_pixels = if pixels.is_finite() {
            pixels.max(0.0)
        } else {
            DEFAULT_LOD_PIXELS
        };
    }

    /// Current level-of-detail threshold (see [`GcodeViewer::set_lod_pixels`]).
    pub fn lod_pixels(&self) -> f32 {
        self.lod_pixels
    }

    /// True while the toolpath is still being prepared or uploaded.
    pub fn is_loading(&self) -> bool {
        self.loader.is_some() || self.renderer.is_uploading()
    }

    /// Advance background loading by one frame's worth of work without
    /// drawing: accept finished build results and upload the next slice of
    /// instance data. [`GcodeViewer::ui`] does this automatically; call it
    /// when driving the viewer headlessly. Returns true while still loading.
    pub fn poll(&mut self) -> bool {
        self.poll_loader();
        let uploading = !self.renderer.upload_step(UPLOAD_BUDGET);
        self.loader.is_some() || uploading
    }

    /// Block until the scene is built, indexed and fully on the GPU.
    pub fn wait_until_loaded(&mut self) {
        while let Some(loader) = &self.loader {
            match loader.rx.recv() {
                Ok(msg) => self.handle_loaded(msg),
                Err(_) => self.loader = None,
            }
        }
        while !self.renderer.upload_step(usize::MAX / 2) {}
    }

    /// Number of layers in the loaded scene.
    pub fn layer_count(&self) -> usize {
        match (&self.info, &self.toolpath) {
            (Some(info), _) => info.layers.len(),
            (None, Some(tp)) => instance::layer_count(tp),
            (None, None) => 0,
        }
    }

    /// Value ranges of the loaded scene (gradient modes).
    pub fn scalar_ranges(&self) -> Option<&ScalarRanges> {
        self.info.as_ref().map(|i| &i.legend.ranges)
    }

    /// Legend model for the current color mode.
    pub fn legend(&self) -> Legend {
        match &self.info {
            Some(info) => color::build_legend(&info.legend, self.options.color_mode),
            None => Legend::Empty,
        }
    }

    /// Pick the nearest visible bead along a world-space ray (respecting
    /// layer range, hidden features and line limit). `None` until the pick
    /// index has been built.
    pub fn pick(&self, ray: &Ray, min_radius: f32) -> Option<PickHit> {
        let tp = self.toolpath.as_ref()?;
        let index = self.info.as_ref()?.pick.as_ref()?;
        index.pick(tp, ray, &self.pick_filter(), min_radius)
    }

    /// Pick the nearest shown travel move passing within `radius` mm of a
    /// world-space ray (respecting layer range and line limit; hidden
    /// features do not hide travels). `None` while travels are hidden or
    /// until the travel index has been built.
    pub fn pick_travel(&self, ray: &Ray, radius: f32) -> Option<PickHit> {
        self.pick_travel_within(ray, radius, f32::INFINITY)
    }

    /// [`GcodeViewer::pick_travel`] limited to hits nearer than `max_t`.
    fn pick_travel_within(&self, ray: &Ray, radius: f32, max_t: f32) -> Option<PickHit> {
        if !self.options.show_travel {
            return None;
        }
        let tp = self.toolpath.as_ref()?;
        let index = self.info.as_ref()?.travel_pick.as_ref()?;
        index.pick_within(tp, ray, &self.pick_filter(), radius, max_t)
    }

    /// Pick at a position inside a view of `size` (same units, origin
    /// top-left) using the active camera: the bead under the cursor, or a
    /// shown travel move within a few pixels when it is not hidden behind
    /// that bead. Use the segment's `kind` to tell them apart.
    pub fn pick_screen(&self, pos: [f32; 2], size: [f32; 2]) -> Option<PickHit> {
        let cam = self.active_camera();
        let view = Vec2::from(size).max(Vec2::ONE);
        let ray = cam.ray_from_screen(Vec2::from(pos), view);
        let world_px = cam.world_per_pixel(cam.distance, view.y);
        let tolerance = PICK_TOLERANCE_PX * world_px;
        let bead = self.pick(&ray, tolerance);
        // Travel lines are drawn over the beads they pass in front of (or
        // lie on); the bead's hit is up to its pick tolerance early. Travels
        // hidden behind the bead are not searched.
        let max_t = bead.map_or(f32::INFINITY, |b| b.t + tolerance + 0.05);
        self.pick_travel_within(&ray, TRAVEL_PICK_PX * world_px, max_t)
            .or(bead)
    }

    /// Allocate the remaining space and draw; handles orbit (LMB drag), pan
    /// (RMB/MMB drag or Shift+LMB), zoom (wheel, towards cursor),
    /// double-click to focus, click to pick, and keys 0-6 / Z for presets
    /// and fit while hovered.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> ViewerResponse {
        self.repaint_ctx = Some(ui.ctx().clone());
        self.poll_loader();
        let uploading = !self.renderer.upload_step(UPLOAD_BUDGET);

        let mut size = ui.available_size();
        if !size.x.is_finite() || size.x < 1.0 {
            size.x = 320.0;
        }
        if !size.y.is_finite() || size.y < 1.0 {
            size.y = 240.0;
        }
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point().max(0.1);
        let rect = egui::Rect::from_min_max(
            ((rect.min.to_vec2() * ppp).round() / ppp).to_pos2(),
            ((rect.max.to_vec2() * ppp).round() / ppp).to_pos2(),
        );
        let w_px = ((rect.width() * ppp).round() as u32).max(1);
        let h_px = ((rect.height() * ppp).round() as u32).max(1);
        let aspect = w_px as f32 / h_px as f32;

        let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1);
        self.step_anim(dt);
        self.prepare_cameras(aspect);

        let mut clicked_line = None;
        self.handle_input(ui, &response, rect, &mut clicked_line);
        let hovered_line = self.update_hover(&response, rect);

        if self.egui_renderer.is_some() && rect.width() >= 1.0 && rect.height() >= 1.0 {
            if self.renderer.ensure_targets(Slot::Ui, w_px, h_px) {
                self.sync_egui_texture();
                self.last_frame = None;
            }
            let size_px = self
                .renderer
                .targets(Slot::Ui)
                .map_or((w_px, h_px), |t| t.size);
            let key = FrameKey {
                params: self.build_params(size_px),
                uploaded: self.renderer.uploaded(),
            };
            if self.last_frame.as_ref() != Some(&key) {
                self.renderer.render(Slot::Ui, &key.params);
                self.last_frame = Some(key);
            }
        }

        let painter = ui.painter_at(rect);
        match self.texture_id {
            Some(id) => {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                painter.image(id, rect, uv, egui::Color32::WHITE);
            }
            None => {
                let [r, g, b] = self.options.background;
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(r, g, b));
                if self.egui_renderer.is_none() {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "3D view requires the wgpu renderer",
                        egui::FontId::proportional(14.0),
                        egui::Color32::from_gray(160),
                    );
                }
            }
        }
        let loading = self.loader.is_some() || uploading;
        if loading {
            self.paint_loading(&painter, rect);
        }
        if let Some(hover) = self.hover
            && !response.dragged()
            && let Some(pos) = response.hover_pos()
        {
            self.show_tooltip(ui.ctx(), response.id.with("hover"), hover, pos, rect);
        }
        if loading || self.anim.is_some() {
            ui.ctx().request_repaint();
        }
        ViewerResponse {
            response,
            clicked_line,
            hovered_line,
        }
    }

    /// Compact color legend for the current color mode (features with
    /// visibility checkboxes and time share, tools, or a gradient bar with
    /// min/max values).
    pub fn legend_ui(&mut self, ui: &mut egui::Ui) {
        let mode = self.options.color_mode;
        let title = if mode.is_gradient() && !mode.unit().is_empty() {
            format!("{} ({})", mode.name(), mode.unit())
        } else {
            mode.name().to_owned()
        };
        ui.label(egui::RichText::new(title).strong());
        match self.legend() {
            Legend::Features(rows) => {
                let id = ui.id().with("sprint-viewer-feature-legend");
                egui::Grid::new(id)
                    .num_columns(2)
                    .spacing([12.0, 3.0])
                    .show(ui, |ui| {
                        for row in rows {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                let mut visible = !self.options.is_feature_hidden(row.feature);
                                if ui
                                    .checkbox(&mut visible, "")
                                    .on_hover_text("Show or hide this feature")
                                    .changed()
                                {
                                    self.options.set_feature_hidden(row.feature, !visible);
                                }
                                swatch(ui, row.color);
                                ui.label(row.feature.name());
                            });
                            match row.time_share {
                                Some(share) => ui.label(
                                    egui::RichText::new(format!(
                                        "{:>5.1}%  {}",
                                        share * 100.0,
                                        format_duration(row.time)
                                    ))
                                    .weak(),
                                ),
                                None => ui.label(
                                    egui::RichText::new(format!("{} moves", row.segments)).weak(),
                                ),
                            };
                            ui.end_row();
                        }
                    });
                ui.horizontal(|ui| {
                    if ui.small_button("Show all").clicked() {
                        self.options.hidden_features = [false; Feature::COUNT];
                    }
                    if ui.small_button("Hide all").clicked() {
                        self.options.hidden_features = [true; Feature::COUNT];
                    }
                });
            }
            Legend::Tools(rows) => {
                for row in rows {
                    ui.horizontal(|ui| {
                        swatch(ui, row.color);
                        ui.label(format!("Tool {}", row.tool));
                        ui.label(egui::RichText::new(format!("{} moves", row.segments)).weak());
                    });
                }
            }
            Legend::Gradient(g) => gradient_bar(ui, &g),
            Legend::Empty => {
                ui.label(egui::RichText::new("No data").weak());
            }
        }
        if self.options.show_travel {
            ui.horizontal(|ui| {
                let [r, g, b, _] = color::TRAVEL_COLOR;
                swatch(ui, [r, g, b]);
                ui.label("Travel");
            });
        }
        if self.options.show_retractions {
            ui.horizontal(|ui| {
                swatch(ui, color::RETRACT_COLOR);
                ui.label("Retraction");
                swatch(ui, color::UNRETRACT_COLOR);
                ui.label("Prime");
            });
        }
    }

    /// Render one frame offscreen and return RGBA8 pixels (tests,
    /// thumbnails). Waits for a pending toolpath to finish loading. The
    /// returned size may be smaller than requested when it exceeds the GPU's
    /// texture or buffer limits (very large requests are scaled down keeping
    /// the aspect ratio). `None` for a zero size or when the GPU cannot
    /// allocate the image.
    pub fn render_to_image(&mut self, width: u32, height: u32) -> Option<(u32, u32, Vec<u8>)> {
        self.wait_until_loaded();
        if width == 0 || height == 0 {
            return None;
        }
        let (width, height) = self.renderer.readback_size(width, height);
        if !self.render_offscreen(width, height) {
            return None;
        }
        self.renderer.read_pixels(Slot::Offscreen)
    }

    /// Submit one frame to the offscreen target used by
    /// [`GcodeViewer::render_to_image`] without reading it back (warm-up and
    /// benchmarks). Uses whatever data is uploaded so far; finishes camera
    /// animations. Returns false for a zero size.
    pub fn render_offscreen(&mut self, width: u32, height: u32) -> bool {
        if width == 0 || height == 0 {
            return false;
        }
        if let Some(anim) = self.anim.take() {
            if anim.top {
                self.camera_top = anim.to;
            } else {
                self.camera = anim.to;
            }
        }
        self.renderer.ensure_targets(Slot::Offscreen, width, height);
        let Some(size) = self.renderer.targets(Slot::Offscreen).map(|t| t.size) else {
            return false;
        };
        self.prepare_cameras(size.0 as f32 / size.1 as f32);
        let params = self.build_params(size);
        self.renderer.render(Slot::Offscreen, &params);
        true
    }

    // ------------------------------------------------------------ internals

    fn cancel_loader(&mut self) {
        if let Some(loader) = self.loader.take() {
            loader.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn poll_loader(&mut self) {
        let mut messages = Vec::new();
        if let Some(loader) = &self.loader {
            loop {
                match loader.rx.try_recv() {
                    Ok(msg) => messages.push(msg),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.loader = None;
                        break;
                    }
                }
            }
        }
        for msg in messages {
            self.handle_loaded(msg);
        }
    }

    fn handle_loaded(&mut self, msg: Loaded) {
        match msg {
            Loaded::Scene(scene) => self.accept_scene(*scene),
            Loaded::Pick(pick) => {
                if let Some(info) = self.info.as_mut() {
                    match pick.kind() {
                        PickKind::Extrusions => info.pick = Some(*pick),
                        PickKind::Travels => info.travel_pick = Some(*pick),
                    }
                }
                self.hover_query = None;
            }
        }
    }

    fn accept_scene(&mut self, scene: SceneData) {
        let SceneData {
            extrusions,
            travels,
            retractions,
            layers,
            clusters,
            legend,
            bounds,
        } = scene;
        self.renderer.set_scene(extrusions, travels, retractions);
        self.info = Some(SceneInfo {
            layers,
            clusters,
            legend,
            bounds,
            pick: None,
            travel_pick: None,
        });
        self.last_frame = None;
        // The scene's bounds ignore start/end G-code and so can differ from
        // the toolpath's.
        self.refit_if_auto();
    }

    /// Refit the cameras after the fit box changed, when auto-fitting. A
    /// running animation (e.g. a fit_to_view or set_view issued right after
    /// set_toolpath) is retargeted instead of being left to undo the fit.
    fn refit_if_auto(&mut self) {
        if !self.auto_fit {
            return;
        }
        let aspect = self.fit_aspect;
        let (min, max) = self.fit_box();
        let radius = self.scene_radius();
        match self.anim.as_mut() {
            None => self.fit_now(aspect),
            Some(anim) => {
                anim.to.scene_radius = radius;
                anim.to.fit_bounds(min, max, aspect);
                let other = if anim.top {
                    &mut self.camera
                } else {
                    &mut self.camera_top
                };
                other.scene_radius = radius;
                other.fit_bounds(min, max, aspect);
                self.top_fitted = true;
            }
        }
    }

    /// The box the camera fits: rendered extrusion bounds, the toolpath's
    /// own bounds, or a slab of the bed.
    fn fit_box(&self) -> (Vec3, Vec3) {
        if let Some((min, max)) = self.info.as_ref().and_then(|i| i.bounds) {
            return (Vec3::from(min), Vec3::from(max));
        }
        if let Some(tp) = &self.toolpath {
            let b = tp.bounds;
            let ok = !b.is_empty()
                && b.min
                    .iter()
                    .chain(b.max.iter())
                    .all(|v| v.is_finite() && v.abs() < MAX_COORD);
            if ok {
                return (Vec3::from(b.min), Vec3::from(b.max));
            }
        }
        let v = self.bed.volume();
        (Vec3::from(v.min), Vec3::new(v.max[0], v.max[1], 10.0))
    }

    /// Generous radius of everything that may be drawn; sizes the
    /// orthographic depth range (Depth32Float keeps plenty of precision).
    fn scene_radius(&self) -> f32 {
        let v = self.bed.volume();
        let (min, max) = self.fit_box();
        let lo = Vec3::from(v.min).min(min);
        let hi = Vec3::from(v.max).max(max);
        ((hi - lo).length() + 50.0).clamp(10.0, MAX_COORD)
    }

    fn fit_now(&mut self, aspect: f32) {
        let (min, max) = self.fit_box();
        let radius = self.scene_radius();
        self.camera.scene_radius = radius;
        self.camera_top.scene_radius = radius;
        self.camera.fit_bounds(min, max, aspect);
        self.camera_top.fit_bounds(min, max, aspect);
        self.top_fitted = true;
        self.fit_aspect = aspect;
    }

    /// Keep scene radius current and re-fit when auto-fitting and the
    /// aspect ratio changed.
    fn prepare_cameras(&mut self, aspect: f32) {
        let radius = self.scene_radius();
        self.camera.scene_radius = radius;
        self.camera_top.scene_radius = radius;
        if !self.top_fitted {
            let (min, max) = self.fit_box();
            self.camera_top.fit_bounds(min, max, aspect);
            self.top_fitted = true;
        }
        let resized = (aspect / self.fit_aspect - 1.0).abs() > 0.01;
        self.fit_aspect = aspect;
        if resized {
            // Retargets a running animation rather than skipping the refit.
            self.refit_if_auto();
        }
    }

    fn active_camera(&self) -> &Camera {
        match self.options.view_mode {
            ViewMode::Perspective => &self.camera,
            ViewMode::LayerTop => &self.camera_top,
        }
    }

    fn active_camera_mut(&mut self) -> &mut Camera {
        match self.options.view_mode {
            ViewMode::Perspective => &mut self.camera,
            ViewMode::LayerTop => &mut self.camera_top,
        }
    }

    /// The user moved the camera: stop animations and auto-fitting.
    fn interacted(&mut self) {
        // Keep the in-between camera of a running animation where the user
        // grabbed it.
        self.anim = None;
        self.auto_fit = false;
    }

    fn start_anim(&mut self, to: Camera, top: bool) {
        let from = if top { self.camera_top } else { self.camera };
        self.anim = Some(Anim {
            from,
            to,
            t: 0.0,
            top,
        });
    }

    fn step_anim(&mut self, dt: f32) {
        let Some(anim) = self.anim.as_mut() else {
            return;
        };
        anim.t = (anim.t + dt / ANIM_SECONDS).min(1.0);
        let eased = 1.0 - (1.0 - anim.t).powi(3);
        let cam = anim.from.lerp(&anim.to, eased);
        let (top, done) = (anim.top, anim.t >= 1.0);
        if top {
            self.camera_top = cam;
        } else {
            self.camera = cam;
        }
        if done {
            self.anim = None;
        }
    }

    fn pick_filter(&self) -> PickFilter {
        let n = self.layer_count();
        let (lo, hi) = self.options.clamped_layer_range(n).unwrap_or((0, 0));
        let lo = if self.options.view_mode == ViewMode::LayerTop {
            hi
        } else {
            lo
        };
        PickFilter {
            layers: (lo, hi),
            hidden_mask: self.options.hidden_mask(),
            line_limit: self.options.line_limit.map(|l| (l, hi)),
        }
    }

    /// World point to zoom towards / focus on: the bead under the cursor,
    /// else the bed plane, else the target plane.
    fn anchor_at(&self, local: Vec2, view: Vec2) -> Option<Vec3> {
        if let Some(hit) = self.pick_screen(local.to_array(), view.to_array()) {
            return Some(hit.point);
        }
        let cam = self.active_camera();
        let ray = cam.ray_from_screen(local, view);
        if let Some(t) = ray.intersect_plane_z(0.0) {
            let p = ray.at(t);
            if (p - cam.target).length() < cam.scene_radius * 4.0 {
                return Some(p);
            }
        }
        ray.intersect_plane(cam.target, cam.forward())
            .map(|t| ray.at(t))
    }

    fn handle_input(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: egui::Rect,
        clicked_line: &mut Option<u32>,
    ) {
        let view = Vec2::new(rect.width().max(1.0), rect.height().max(1.0));
        let local = |p: egui::Pos2| Vec2::new(p.x - rect.min.x, p.y - rect.min.y);
        let top = self.options.view_mode == ViewMode::LayerTop;
        let shift = ui.input(|i| i.modifiers.shift);

        let delta = response.drag_delta();
        if delta != egui::Vec2::ZERO {
            let d = Vec2::new(delta.x, delta.y);
            let primary = response.dragged_by(egui::PointerButton::Primary);
            let pan = response.dragged_by(egui::PointerButton::Secondary)
                || response.dragged_by(egui::PointerButton::Middle)
                || (primary && (shift || top));
            if pan {
                self.interacted();
                self.active_camera_mut().pan(d, view.y);
            } else if primary {
                self.interacted();
                self.camera.orbit(-d.x * ORBIT_SPEED, d.y * ORBIT_SPEED);
            }
        }

        if response.contains_pointer() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            if scroll != 0.0 || (zoom - 1.0).abs() > 1e-4 {
                let factor = (-scroll * 0.0022).exp() / zoom.max(1e-3);
                let anchor = response
                    .hover_pos()
                    .and_then(|p| self.anchor_at(local(p), view));
                self.interacted();
                self.active_camera_mut().zoom(factor, anchor);
                ui.ctx()
                    .input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            }
            if !ui.ctx().egui_wants_keyboard_input() {
                self.handle_keys(ui);
            }
        }

        if response.double_clicked() {
            if let Some(p) = response.interact_pointer_pos()
                && let Some(anchor) = self.anchor_at(local(p), view)
            {
                let top = self.options.view_mode == ViewMode::LayerTop;
                let mut to = *self.active_camera();
                to.focus_on(if top {
                    Vec3::new(anchor.x, anchor.y, to.target.z)
                } else {
                    anchor
                });
                self.auto_fit = false;
                self.start_anim(to, top);
            }
        } else if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
            && let Some(hit) = self.pick_screen(local(p).to_array(), view.to_array())
        {
            *clicked_line = self
                .toolpath
                .as_ref()
                .and_then(|tp| tp.segments.get(hit.segment))
                .map(|s| s.line);
        }
    }

    fn handle_keys(&mut self, ui: &egui::Ui) {
        use egui::Key;
        let keys = [
            (Key::Num0, ViewPreset::Iso),
            (Key::Num1, ViewPreset::Top),
            (Key::Num2, ViewPreset::Bottom),
            (Key::Num3, ViewPreset::Front),
            (Key::Num4, ViewPreset::Back),
            (Key::Num5, ViewPreset::Left),
            (Key::Num6, ViewPreset::Right),
        ];
        for (key, preset) in keys {
            if ui.input(|i| i.key_pressed(key) && i.modifiers.is_none()) {
                self.auto_fit = false;
                self.set_view(preset);
            }
        }
        if ui.input(|i| i.key_pressed(Key::Z) && i.modifiers.is_none()) {
            self.fit_to_view();
        }
    }

    fn update_hover(&mut self, response: &egui::Response, rect: egui::Rect) -> Option<u32> {
        let pos = response
            .hover_pos()
            .filter(|_| !response.dragged() && rect.width() >= 1.0 && rect.height() >= 1.0);
        let Some(pos) = pos else {
            self.hover = None;
            self.hover_query = None;
            return None;
        };
        let query = HoverQuery {
            pos: [pos.x - rect.min.x, pos.y - rect.min.y],
            view: [rect.width(), rect.height()],
            camera: *self.active_camera(),
            filter: self.pick_filter(),
            has_index: self.info.as_ref().is_some_and(|i| i.pick.is_some()),
            travels: self.options.show_travel
                && self.info.as_ref().is_some_and(|i| i.travel_pick.is_some()),
        };
        if self.hover_query.as_ref() != Some(&query) {
            let hit = self.pick_screen(query.pos, query.view);
            self.hover = hit.and_then(|h| {
                let tp = self.toolpath.as_ref()?;
                let seg = tp.segments.get(h.segment)?;
                Some(Hover {
                    segment: h.segment,
                    line: seg.line,
                })
            });
            self.hover_query = Some(query);
        }
        self.hover.map(|h| h.line)
    }

    fn sync_egui_texture(&mut self) {
        let (Some(egui_renderer), Some(targets)) =
            (self.egui_renderer.as_ref(), self.renderer.targets(Slot::Ui))
        else {
            return;
        };
        let device = self.renderer.device();
        let mut r = egui_renderer.write();
        match self.texture_id {
            None => {
                self.texture_id = Some(r.register_native_texture(
                    device,
                    &targets.sample_view,
                    wgpu::FilterMode::Linear,
                ));
            }
            Some(id) => {
                r.update_egui_texture_from_wgpu_texture(
                    device,
                    &targets.sample_view,
                    wgpu::FilterMode::Linear,
                    id,
                );
            }
        }
    }

    fn tool_head_position(&self) -> Option<Vec3> {
        if !self.options.show_tool_head {
            return None;
        }
        let line = self.options.line_limit.or(self.options.progress_line)?;
        let tp = self.toolpath.as_ref()?;
        let seg = tp.segments.get(tp.segment_at_line(line)?)?;
        let p = Vec3::from(seg.end);
        (p.is_finite() && p.abs().max_element() < MAX_COORD).then_some(p)
    }

    /// Depth-cue start distance and `1 / (end - start)` for a camera: haze
    /// starts a little in front of the scene centre and is complete at the
    /// far side of the bed or print, whichever reaches further.
    fn depth_cue_range(&self, cam: &Camera) -> (f32, f32) {
        let (min, max) = self.fit_box();
        let bed = self.bed.volume();
        let lo = min.min(Vec3::new(bed.min[0], bed.min[1], 0.0));
        let hi = max.max(Vec3::new(bed.max[0], bed.max[1], 0.0));
        let centre = (lo + hi) * 0.5;
        let radius = ((hi - lo).length() * 0.5).max(1.0);
        let dist = (cam.eye() - centre).length();
        let start = (dist - radius * 0.25).max(0.0);
        let end = dist + radius;
        let span = (end - start).max(1e-3);
        if start.is_finite() && span.is_finite() {
            (start, 1.0 / span)
        } else {
            (0.0, 0.0)
        }
    }

    fn build_params(&self, size: (u32, u32)) -> DrawParams {
        let o = &self.options;
        let cam = self.active_camera();
        let aspect = size.0.max(1) as f32 / size.1.max(1) as f32;
        let f = cam.forward();
        let persp = cam.projection == Projection::Perspective;
        let pixel = match cam.projection {
            Projection::Perspective => 2.0 * (cam.fov_y * 0.5).tan() / size.1.max(1) as f32,
            Projection::Orthographic => 2.0 * cam.half_height() / size.1.max(1) as f32,
        };
        let rig = LightRig::for_camera(cam);
        let eye = cam.eye();
        let (fog_start, fog_inv_span) = self.depth_cue_range(cam);

        let lin4 = |c: [u8; 3], a: f32| {
            let l = srgb_to_linear_rgb(c);
            [l[0], l[1], l[2], a]
        };
        let mut feature_colors = [[0.0f32; 4]; 16];
        for (slot, feature) in feature_colors.iter_mut().zip(Feature::ALL) {
            *slot = lin4(color::feature_color(feature), 1.0);
        }
        let mut tool_colors = [[0.0f32; 4]; 8];
        for (slot, c) in tool_colors.iter_mut().zip(color::TOOL_COLORS) {
            *slot = lin4(c, 1.0);
        }
        let mut ramp = [[0.0f32; 4]; 12];
        for (slot, c) in ramp.iter_mut().zip(color::RAMP) {
            *slot = [
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
                1.0,
            ];
        }
        let [tr, tg, tb, ta] = color::TRAVEL_COLOR;

        let n = self.info.as_ref().map_or(0, |i| i.layers.len());
        let range = o.clamped_layer_range(n);
        let (lo, hi) = range.unwrap_or((0, 0));
        let top_mode = o.view_mode == ViewMode::LayerTop;

        let mut flag_bits = 0u32;
        if o.progress_line.is_some() {
            flag_bits |= flags::PROGRESS;
        }
        if o.highlight_line.is_some() {
            flag_bits |= flags::HIGHLIGHT;
        }
        if o.line_limit.is_some() {
            flag_bits |= flags::LINE_LIMIT;
        }
        if self.hover.is_some() {
            flag_bits |= flags::HOVER;
        }
        if top_mode {
            flag_bits |= flags::LAYER_TOP;
        }
        if self.renderer.encodes_gamma() {
            flag_bits |= flags::ENCODE_GAMMA;
        }

        let scalar = self
            .info
            .as_ref()
            .map(|i| i.legend.ranges.shader_range(o.color_mode))
            .unwrap_or_default()
            .or_zero();
        let bed_min = self.bed.min_xy();
        let bed_max = self.bed.max_xy();
        let globals = Globals {
            view_proj: cam.view_proj(aspect).to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, if persp { 1.0 } else { 0.0 }],
            forward: [f.x, f.y, f.z, pixel],
            key_light: rig.key,
            bed: [bed_min[0], bed_min[1], bed_max[0], bed_max[1]],
            feature_colors,
            tool_colors,
            ramp,
            travel_color: lin4([tr, tg, tb], ta as f32 / 255.0),
            retract_color: lin4(color::RETRACT_COLOR, 1.0),
            unretract_color: lin4(color::UNRETRACT_COLOR, 1.0),
            plate_color: lin4([38, 41, 49], 0.94),
            grid_color: lin4([60, 65, 78], 0.85),
            grid_bold_color: lin4([86, 93, 112], 1.0),
            mode: [
                o.color_mode.index(),
                o.hidden_mask(),
                flag_bits,
                color::RAMP.len() as u32,
            ],
            lines: [
                o.progress_line.unwrap_or(u32::MAX),
                o.highlight_line.unwrap_or(u32::MAX),
                o.line_limit.unwrap_or(u32::MAX),
                self.hover.map_or(u32::MAX, |h| h.line),
            ],
            layers: [
                lo.min(u32::MAX as usize) as u32,
                hi.min(u32::MAX as usize) as u32,
                hi.min(u32::MAX as usize) as u32,
                0,
            ],
            range: [scalar.0, scalar.1, MARKER_PX, 0.0],
            fill_light: rig.fill,
            rim_light: rig.rim,
            fog: {
                let [r, g, b] = srgb_to_linear_rgb(o.background);
                let strength = if persp { DEPTH_CUE } else { 0.0 };
                [r, g, b, strength]
            },
            fog_range: [fog_start, fog_inv_span, 0.0, 0.0],
            shading: [CAVITY, DETAIL_PIXELS.0, DETAIL_PIXELS.1, EXPOSURE],
        };

        let (prisms, ribbons) = match (range, &self.info) {
            (Some((a, b)), Some(info)) => {
                let lod = if o.style == RenderStyle::Lines {
                    0.0
                } else {
                    self.lod_pixels
                };
                select_clusters(
                    info,
                    (a, b),
                    &Frustum::from_matrix(Mat4::from_cols_array_2d(&globals.view_proj)),
                    cam,
                    pixel,
                    lod,
                )
            }
            _ => (Vec::new(), Vec::new()),
        };
        // Travels of the visible layers (only the top one in the layer view).
        let travels = match (o.show_travel, range, &self.info) {
            (true, Some((a, b)), Some(info)) => {
                let a = if top_mode { b } else { a };
                match (info.layers.get(a), info.layers.get(b)) {
                    (Some(first), Some(last)) => {
                        let start = first.travels.start;
                        Some(start..last.travels.end.max(start))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let clear = if self.renderer.encodes_gamma() {
            let [r, g, b] = o.background;
            [r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0]
        } else {
            let [r, g, b] = o.background;
            [
                srgb_to_linear(r) as f64,
                srgb_to_linear(g) as f64,
                srgb_to_linear(b) as f64,
                1.0,
            ]
        };
        DrawParams {
            globals,
            clear,
            style: o.style,
            prisms,
            ribbons,
            travels,
            retractions: o.show_retractions && range.is_some(),
            bed: o.show_bed,
            volume: o.show_build_volume,
            axes: o.show_axes,
            tool_head: self.tool_head_position(),
        }
    }

    fn paint_loading(&self, painter: &egui::Painter, rect: egui::Rect) {
        let text = if self.info.is_none() {
            "Preparing toolpath…".to_owned()
        } else if self.renderer.is_uploading() {
            format!("Uploading {:.0}%", self.renderer.upload_progress() * 100.0)
        } else {
            "Indexing for picking…".to_owned()
        };
        let pos = rect.left_top() + egui::vec2(12.0, 12.0);
        let galley = painter.layout_no_wrap(
            text,
            egui::FontId::proportional(13.0),
            egui::Color32::from_gray(210),
        );
        let bg = egui::Rect::from_min_size(pos, galley.size() + egui::vec2(16.0, 8.0));
        painter.rect_filled(bg, 6.0, egui::Color32::from_black_alpha(150));
        painter.galley(pos + egui::vec2(8.0, 4.0), galley, egui::Color32::WHITE);
    }

    fn show_tooltip(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        hover: Hover,
        pos: egui::Pos2,
        rect: egui::Rect,
    ) {
        let Some(tp) = &self.toolpath else {
            return;
        };
        let Some(seg) = tp.segments.get(hover.segment) else {
            return;
        };
        let layer = seg.layer as usize;
        let z = tp.layers.get(layer).map_or(seg.end[2], |l| l.z);
        let mut at = pos + egui::vec2(16.0, 16.0);
        if at.x + 220.0 > rect.right() {
            at.x = (pos.x - 236.0).max(rect.left());
        }
        if at.y + 120.0 > rect.bottom() {
            at.y = (pos.y - 136.0).max(rect.top());
        }
        egui::Area::new(id)
            .order(egui::Order::Tooltip)
            .fixed_pos(at)
            .interactable(false)
            .show(ctx, |ui| {
                if !seg.is_extrusion() {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        travel_tooltip(ui, id, seg, layer, z);
                    });
                    return;
                }
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        swatch(ui, color::feature_color(seg.feature));
                        ui.label(egui::RichText::new(seg.feature.name()).strong());
                    });
                    egui::Grid::new(id.with("grid"))
                        .num_columns(2)
                        .spacing([12.0, 2.0])
                        .show(ui, |ui| {
                            ui.label("Speed");
                            ui.label(format_value(ColorMode::Speed, seg.feedrate));
                            ui.end_row();
                            ui.label("Width × height");
                            ui.label(format!(
                                "{:.2} × {:.2} mm",
                                instance::render_width(seg.width),
                                instance::render_height(seg.height)
                            ));
                            ui.end_row();
                            ui.label("Layer");
                            ui.label(format!("{} (Z {:.2} mm)", layer + 1, z));
                            ui.end_row();
                            ui.label("Line");
                            ui.label(format!("{}", seg.line + 1));
                            ui.end_row();
                            ui.label("Tool / fan");
                            ui.label(format!(
                                "T{}  {:.0} %",
                                seg.tool,
                                seg.fan as f32 / 255.0 * 100.0
                            ));
                            ui.end_row();
                            if seg.time.is_finite() && seg.time > 0.0 {
                                ui.label("Time");
                                ui.label(format_duration(seg.time));
                                ui.end_row();
                            }
                        });
                });
            });
    }
}

impl std::fmt::Debug for GcodeViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcodeViewer")
            .field("options", &self.options)
            .field("bed", &self.bed)
            .field("camera", self.active_camera())
            .field(
                "segments",
                &self.toolpath.as_ref().map(|tp| tp.segments.len()),
            )
            .field("layers", &self.layer_count())
            .field("loading", &self.is_loading())
            .field("lod_pixels", &self.lod_pixels)
            .finish_non_exhaustive()
    }
}

impl Drop for GcodeViewer {
    fn drop(&mut self) {
        self.cancel_loader();
        if let (Some(r), Some(id)) = (&self.egui_renderer, self.texture_id) {
            r.write().free_texture(&id);
        }
    }
}

/// Largest batch formed by merging a range that comes just *before* the
/// previous one (layers visited top-down). Bigger batches would be drawn
/// bottom-up and lose the front-to-back order that early depth testing
/// relies on; smaller ones would cost one draw call per tiny layer (spiral
/// vase files have one layer per segment).
const BACKWARD_MERGE_MAX: u32 = 4 * CLUSTER_SIZE as u32;

/// Append `r` to `ranges`, merging it with the last range when they touch.
fn push_range(ranges: &mut Vec<std::ops::Range<u32>>, r: std::ops::Range<u32>) {
    if r.is_empty() {
        return;
    }
    match ranges.last_mut() {
        Some(last) if last.end == r.start => last.end = r.end,
        Some(last)
            if r.end == last.start && last.end.saturating_sub(r.start) <= BACKWARD_MERGE_MAX =>
        {
            last.start = r.start;
        }
        _ => ranges.push(r),
    }
}

/// Choose what to draw for the visible layers `a..=b`: clusters outside the
/// frustum are skipped; the rest are split into prisms (beads at least
/// `lod_pixels` wide on screen) and ribbon impostors. Layers are visited
/// front to back for the camera (top layers first when looking down) so
/// early depth testing rejects most hidden fragments. The work is
/// proportional to the number of clusters in view, not to the number of
/// layers.
fn select_clusters(
    info: &SceneInfo,
    (a, b): (usize, usize),
    frustum: &Frustum,
    cam: &Camera,
    pixel: f32,
    lod_pixels: f32,
) -> (Vec<std::ops::Range<u32>>, Vec<std::ops::Range<u32>>) {
    let mut prisms = Vec::new();
    let mut ribbons = Vec::new();
    let eye = cam.eye();
    let forward = cam.forward();
    let near = cam.near();
    let a = u32::try_from(a).unwrap_or(u32::MAX);
    let b = u32::try_from(b).unwrap_or(u32::MAX).max(a);
    // The part of a cluster that lies in the visible layers. Clusters that
    // span several layers only exist for ordered, disjoint layer ranges, so
    // their visible part is one contiguous instance range.
    let visible = |c: &Cluster| -> std::ops::Range<u32> {
        if c.layers.start >= a && c.layers.end.saturating_sub(1) <= b {
            return c.instances.clone();
        }
        let lo = info
            .layers
            .get(c.layers.start.max(a) as usize)
            .map_or(c.instances.start, |l| l.extrusions.start)
            .max(c.instances.start);
        let hi = info
            .layers
            .get(c.layers.end.saturating_sub(1).min(b) as usize)
            .map_or(c.instances.end, |l| l.extrusions.end)
            .min(c.instances.end);
        lo..hi.max(lo)
    };
    let mut visit = |c: &Cluster| {
        let (min, max) = (Vec3::from(c.min), Vec3::from(c.max));
        if c.instances.is_empty() || !frustum.intersects_aabb(min, max) {
            return;
        }
        let world_per_px = match cam.projection {
            Projection::Perspective => {
                let closest = eye.clamp(min, max);
                (closest - eye).dot(forward).max(near) * pixel
            }
            Projection::Orthographic => pixel,
        };
        let wide = c.max_width >= lod_pixels * world_per_px;
        push_range(if wide { &mut prisms } else { &mut ribbons }, visible(c));
    };
    // Clusters are stored in layer order: binary search the visible ones.
    let first = info.clusters.partition_point(|c| c.layers.end <= a);
    let last = info
        .clusters
        .partition_point(|c| c.layers.start <= b)
        .max(first);
    let clusters = info.clusters.get(first..last).unwrap_or_default();
    if forward.z <= 0.0 {
        // Top layers first; the clusters of one big layer stay in order so
        // they merge into one draw.
        let mut end = clusters.len();
        while let Some(top) = end.checked_sub(1).and_then(|i| clusters.get(i)) {
            let mut start = end - 1;
            while start > 0 && clusters[start - 1].layers == top.layers {
                start -= 1;
            }
            clusters[start..end].iter().for_each(&mut visit);
            end = start;
        }
    } else {
        clusters.iter().for_each(&mut visit);
    }
    (prisms, ribbons)
}

/// Tooltip rows for a hovered travel move.
fn travel_tooltip(
    ui: &mut egui::Ui,
    id: egui::Id,
    seg: &sprint_gcode::Segment,
    layer: usize,
    z: f32,
) {
    ui.horizontal(|ui| {
        let [r, g, b, _] = color::TRAVEL_COLOR;
        swatch(ui, [r, g, b]);
        ui.label(egui::RichText::new("Travel").strong());
    });
    egui::Grid::new(id.with("travel-grid"))
        .num_columns(2)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            ui.label("Speed");
            ui.label(format_value(ColorMode::Speed, seg.feedrate));
            ui.end_row();
            ui.label("Length");
            ui.label(format!("{:.2} mm", seg.length()));
            ui.end_row();
            let dz = seg.end[2] - seg.start[2];
            if dz.abs() > 1e-3 {
                ui.label("Z move");
                ui.label(format!("{dz:+.2} mm"));
                ui.end_row();
            }
            ui.label("Layer");
            ui.label(format!("{} (Z {:.2} mm)", layer + 1, z));
            ui.end_row();
            ui.label("Line");
            ui.label(format!("{}", seg.line + 1));
            ui.end_row();
            if seg.time.is_finite() && seg.time > 0.0 {
                ui.label("Time");
                ui.label(format_duration(seg.time));
                ui.end_row();
            }
            if seg.duration.is_finite() && seg.duration > 0.0 {
                ui.label("Duration");
                ui.label(if seg.duration < 60.0 {
                    format!("{:.2} s", seg.duration)
                } else {
                    format_duration(seg.duration)
                });
                ui.end_row();
            }
        });
}

/// A small color square.
fn swatch(ui: &mut egui::Ui, rgb: [u8; 3]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    let [r, g, b] = rgb;
    ui.painter()
        .rect_filled(rect, 2.0, egui::Color32::from_rgb(r, g, b));
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(90)),
        egui::StrokeKind::Inside,
    );
}

/// Gradient bar with tick labels.
fn gradient_bar(ui: &mut egui::Ui, g: &color::GradientLegend) {
    let width = ui.available_width().clamp(120.0, 260.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
    let mut mesh = egui::Mesh::default();
    let steps = 48;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let [r, gc, b] = ramp_color(t);
        let c = egui::Color32::from_rgb(r, gc, b);
        let x = rect.left() + t * rect.width();
        mesh.colored_vertex(egui::pos2(x, rect.top()), c);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), c);
        if i > 0 {
            let k = (i * 2) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(90)),
        egui::StrokeKind::Inside,
    );
    let (labels, _) = ui.allocate_exact_size(egui::vec2(width, 16.0), egui::Sense::hover());
    let font = egui::FontId::proportional(11.0);
    let color = ui.visuals().text_color();
    let painter = ui.painter();
    // Place min and max first, then the inner ticks that fit between them
    // without overlapping their neighbours.
    let placed_rect = |i: usize, text: &str| {
        let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
        let t = g.ticks.get(i).map_or(0.0, |t| t.0);
        let x = labels.left() + t * labels.width();
        let w = galley.size().x;
        let left = if g.ticks.len() == 1 {
            x - w * 0.5
        } else if i == 0 {
            x
        } else if i + 1 == g.ticks.len() {
            x - w
        } else {
            x - w * 0.5
        };
        let left = left.clamp(labels.left(), (labels.right() - w).max(labels.left()));
        (
            egui::Rect::from_min_size(egui::pos2(left, labels.top()), galley.size()),
            galley,
        )
    };
    let mut order: Vec<usize> = Vec::with_capacity(g.ticks.len());
    if let Some(last) = g.ticks.len().checked_sub(1) {
        order.push(0);
        if last > 0 {
            order.push(last);
        }
        order.extend(1..last);
    }
    let mut taken: Vec<egui::Rect> = Vec::new();
    for i in order {
        let Some((_, text)) = g.ticks.get(i) else {
            continue;
        };
        let (rect, galley) = placed_rect(i, text);
        if taken
            .iter()
            .any(|r| r.expand2(egui::vec2(6.0, 0.0)).intersects(rect))
        {
            continue;
        }
        taken.push(rect);
        painter.galley(rect.min, galley, color);
    }
}

/// Create a wgpu device and queue without a window, for
/// [`GcodeViewer::new_headless`] (tests, thumbnails, examples).
///
/// Prefers a discrete GPU and falls back to a software adapter (WARP /
/// llvmpipe) when no hardware adapter is available. The `WGPU_BACKEND`
/// environment variable (e.g. `dx12`, `vulkan`, `gl`) restricts the backends
/// tried. Returns `None` when no adapter at all can be created.
pub fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    gpu::headless_device()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use sprint_gcode::{Layer, MoveKind, Segment};

    use super::*;
    use crate::instance::build_scene;

    /// A spiral vase in which every segment is its own layer, which is how
    /// a Z-change based layer detector sees vase mode.
    fn vase(n: usize) -> Toolpath {
        let mut tp = Toolpath::default();
        let mut prev = [130.0f32, 110.0, 0.2];
        for i in 1..=n {
            let a = i as f32 / 200.0 * std::f32::consts::TAU;
            let z = 0.2 + i as f32 * 0.001;
            let p = [110.0 + 20.0 * a.cos(), 110.0 + 20.0 * a.sin(), z];
            tp.segments.push(Segment {
                start: prev,
                end: p,
                feedrate: 30.0,
                width: 0.45,
                height: 0.2,
                line: i as u32,
                layer: (i - 1) as u32,
                kind: MoveKind::Extrude,
                ..Default::default()
            });
            tp.layers.push(Layer {
                z,
                height: 0.2,
                segments: i - 1..i,
                lines: i as u32..i as u32 + 1,
                ..Default::default()
            });
            prev = p;
        }
        tp
    }

    fn info(tp: &Toolpath) -> SceneInfo {
        let s = build_scene(tp, &AtomicBool::new(false)).expect("not cancelled");
        SceneInfo {
            layers: s.layers,
            clusters: s.clusters,
            legend: s.legend,
            bounds: s.bounds,
            pick: None,
            travel_pick: None,
        }
    }

    fn draw_lists(info: &SceneInfo, preset: ViewPreset, lod: f32) -> DrawLists {
        let mut cam = Camera::default();
        cam.set_preset(preset);
        if let Some((min, max)) = info.bounds {
            cam.fit_bounds(Vec3::from(min), Vec3::from(max), 1.6);
        }
        let frustum = Frustum::from_matrix(cam.view_proj(1.6));
        let pixel = 2.0 * (cam.fov_y * 0.5).tan() / 800.0;
        select_clusters(info, (0, info.layers.len() - 1), &frustum, &cam, pixel, lod)
    }

    type DrawLists = (Vec<std::ops::Range<u32>>, Vec<std::ops::Range<u32>>);

    fn instances(lists: &DrawLists) -> u32 {
        lists
            .0
            .iter()
            .chain(&lists.1)
            .map(|r| r.end - r.start)
            .sum()
    }

    #[test]
    fn many_tiny_layers_are_batched_into_few_draws() {
        let tp = vase(20_000);
        let info = info(&tp);
        for preset in [ViewPreset::Iso, ViewPreset::Top, ViewPreset::Bottom] {
            for lod in [0.0, DEFAULT_LOD_PIXELS, f32::MAX] {
                let lists = draw_lists(&info, preset, lod);
                assert_eq!(
                    instances(&lists),
                    20_000,
                    "{preset:?}: everything is visible"
                );
                let draws = lists.0.len() + lists.1.len();
                assert!(
                    draws <= 40,
                    "{preset:?} lod {lod}: {draws} draw calls for 20k one-segment layers"
                );
            }
        }
    }

    /// The selected instance ranges, sorted; panics if they overlap.
    fn covered(lists: &DrawLists) -> Vec<std::ops::Range<u32>> {
        let mut all: Vec<_> = lists.0.iter().chain(&lists.1).cloned().collect();
        all.sort_by_key(|r| r.start);
        let mut merged: Vec<std::ops::Range<u32>> = Vec::new();
        for r in all {
            match merged.last_mut() {
                Some(last) if last.end == r.start => last.end = r.end,
                Some(last) => {
                    assert!(last.end < r.start, "overlapping draws {last:?} {r:?}");
                    merged.push(r);
                }
                None => merged.push(r),
            }
        }
        merged
    }

    #[test]
    fn layer_ranges_select_exactly_their_instances() {
        for tp in [vase(9_000), crate::demo::demo_toolpath()] {
            let info = info(&tp);
            let n = info.layers.len();
            let mut cam = Camera::default();
            let (min, max) = info.bounds.expect("bounds");
            cam.fit_bounds(Vec3::from(min), Vec3::from(max), 1.6);
            let frustum = Frustum::from_matrix(cam.view_proj(1.6));
            let pixel = 2.0 * (cam.fov_y * 0.5).tan() / 800.0;
            for (a, b) in [
                (0, n - 1),
                (0, 0),
                (n - 1, n - 1),
                (3, 7),
                (n / 3, n / 2),
                (1, n - 2),
            ] {
                for lod in [0.0, f32::MAX] {
                    let lists = select_clusters(&info, (a, b), &frustum, &cam, pixel, lod);
                    let want = info.layers[a].extrusions.start..info.layers[b].extrusions.end;
                    let want: Vec<_> = std::iter::once(want).filter(|r| !r.is_empty()).collect();
                    assert_eq!(covered(&lists), want, "layers {a}..={b}, lod {lod}");
                }
            }
        }
    }

    #[test]
    fn light_rig_keys_from_the_upper_left_front_in_every_view() {
        for preset in ViewPreset::ALL {
            let mut cam = Camera::default();
            cam.set_preset(preset);
            let rig = LightRig::for_camera(&cam);
            for light in [rig.key, rig.fill, rig.rim] {
                let d = Vec3::new(light[0], light[1], light[2]);
                assert!((d.length() - 1.0).abs() < 1e-4, "{preset:?}: {d:?}");
                assert!(light[3] > 0.0 && light[3] <= 1.0);
            }
            let key = Vec3::new(rig.key[0], rig.key[1], rig.key[2]);
            let fill = Vec3::new(rig.fill[0], rig.fill[1], rig.fill[2]);
            let rim = Vec3::new(rig.rim[0], rig.rim[1], rig.rim[2]);
            // Key: left, above and in front; fill: right and in front; rim:
            // behind.
            assert!(key.dot(cam.right()) < -0.3, "{preset:?}");
            assert!(key.dot(cam.up()) > 0.3, "{preset:?}");
            assert!(key.dot(-cam.forward()) > 0.4, "{preset:?}");
            assert!(fill.dot(cam.right()) > 0.4 && fill.dot(-cam.forward()) > 0.4);
            assert!(rim.dot(cam.forward()) > 0.5, "{preset:?}");
            assert!(rig.key[3] > 2.0 * rig.fill[3], "the key dominates");
        }
    }

    #[test]
    fn push_range_merges_in_both_directions_but_keeps_big_batches_ordered() {
        let mut v = Vec::new();
        push_range(&mut v, 10..20);
        push_range(&mut v, 20..30);
        assert_eq!(v, vec![10..30]);
        // Visiting layers top-down: a range just before the last one.
        push_range(&mut v, 0..10);
        assert_eq!(v, vec![0..30]);
        push_range(&mut v, 5..5);
        assert_eq!(v, vec![0..30]);
        // Big layers drawn top-down stay separate (front to back).
        let big = CLUSTER_SIZE as u32 * 8;
        let mut v = Vec::new();
        push_range(&mut v, big..2 * big);
        push_range(&mut v, 0..big);
        assert_eq!(v, vec![big..2 * big, 0..big]);
    }
}
