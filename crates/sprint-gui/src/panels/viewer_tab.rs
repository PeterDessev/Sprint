//! The 3D / layer view tab: the GPU toolpath viewer with a toolbar (view
//! mode, color mode, render style, travel / retraction toggles, view
//! presets, fit), a floating legend and file information card, a vertical
//! double layer slider on the right edge and a horizontal scrub slider for
//! the moves of the top layer. While a file prints, the printed part is
//! drawn solid, the rest ghosted, and the top layer can follow the print.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui_wgpu;
use egui::{
    Align, Color32, CornerRadius, Id, Key, Layout, Margin, Modifiers, Pos2, Rect, RichText, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, Vec2, vec2,
};
use sprint_gcode::{Layer, Toolpath};
use sprint_host::BedOrigin;
use sprint_viewer::{BedSpec, ColorMode, GcodeViewer, ViewPreset};

use crate::context::{AppAction, AppCtx, LoadedFile, Tab};
use crate::loader::{LoadStatus, Stage};
use crate::prefs::ViewerPrefs;
use crate::theme::{self, Palette, TextRole};
use crate::widgets::{
    ActionButton, Chip, EmptyState, IconButton, KeyValueGrid, LinearProgress, SegmentedControl,
};
use crate::{format, icons};

/// Width of the layer slider column.
const SLIDER_W: f32 = 78.0;
/// Height of the scrub bar.
const SCRUB_H: f32 = 44.0;
/// Margin of the overlays from the view's edges.
const EDGE: f32 = 12.0;
/// Outer width of the file information card.
const INFO_W: f32 = 282.0;

/// Things the shell lends the viewer tab besides the [`AppCtx`].
#[derive(Clone, Copy, Default)]
pub struct ViewerExtras<'a> {
    /// The loaded file's thumbnail.
    pub thumbnail: Option<&'a egui::TextureHandle>,
    /// The loader's running job.
    pub load: Option<&'a LoadStatus>,
}

/// Which layer handle is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Top,
    Bottom,
}

/// Move one end of an inclusive layer range, keeping it ordered and
/// within `0..count`.
pub fn move_layer(range: (usize, usize), count: usize, top: bool, delta: i64) -> (usize, usize) {
    if count == 0 {
        return (0, 0);
    }
    let last = count - 1;
    let (lo, hi) = (range.0.min(last), range.1.min(last));
    let shift = |v: usize| (v as i64 + delta).clamp(0, last as i64) as usize;
    if top {
        let hi = shift(hi);
        (lo.min(hi), hi)
    } else {
        let lo = shift(lo);
        (lo, hi.max(lo))
    }
}

/// The layer whose segments contain the source line `line` (the last
/// layer starting at or before it).
pub fn layer_of_line(toolpath: &Toolpath, line: u32) -> Option<usize> {
    toolpath.layer_at_line(line)
}

/// The viewer's bed for a printer profile.
pub fn bed_spec(profile: &sprint_host::PrinterProfile) -> BedSpec {
    BedSpec {
        size: profile.bed_size,
        max_z: profile.max_z,
        origin_center: profile.origin == BedOrigin::Center,
    }
    .sanitized()
}

/// The 3D / layer view tab.
pub struct ViewerTab {
    viewer: Option<GcodeViewer>,
    /// Persisted viewer preferences.
    pub prefs: ViewerPrefs,
    ov: Overlay,
}

/// Interaction state of the overlays (kept apart from the viewer so both
/// can be borrowed at once).
#[derive(Default)]
struct Overlay {
    shown: Option<usize>,
    bed: Option<BedSpec>,
    focused: bool,
    drag: Option<Handle>,
    scrub: Option<usize>,
    playing: bool,
    play_accum: f32,
    scrub_layer: Option<usize>,
    followed: Option<usize>,
}

impl ViewerTab {
    /// A tab drawing through `render_state` (None: the GPU view is
    /// unavailable and a placeholder explains why).
    pub fn new(render_state: Option<&egui_wgpu::RenderState>, prefs: ViewerPrefs) -> ViewerTab {
        let viewer = render_state.map(GcodeViewer::new);
        let mut tab = ViewerTab {
            viewer,
            prefs,
            ov: Overlay::default(),
        };
        if let Some(v) = &mut tab.viewer {
            tab.prefs.apply(v.options_mut());
        }
        tab
    }

    /// The GPU viewer, when available.
    pub fn viewer(&self) -> Option<&GcodeViewer> {
        self.viewer.as_ref()
    }

    /// The visible layer range (inclusive), once a toolpath is shown.
    pub fn layer_range(&self) -> Option<(usize, usize)> {
        let v = self.viewer.as_ref()?;
        v.options().clamped_layer_range(v.layer_count())
    }

    /// Show layers `lo..=hi`.
    pub fn set_layer_range(&mut self, lo: usize, hi: usize) {
        if let Some(v) = &mut self.viewer {
            v.options_mut().layer_range = (lo.min(hi), lo.max(hi));
            v.options_mut().line_limit = None;
        }
        self.ov.scrub = None;
    }

    /// Orient the camera to a preset (switches to the 3D view).
    pub fn set_view(&mut self, preset: ViewPreset) {
        if let Some(v) = &mut self.viewer {
            v.set_view(preset);
        }
        self.prefs.layer_view = false;
    }

    /// Widen the shown layer range so the layer holding source `line` is
    /// visible (a selection made outside the view), and drop a scrub that
    /// would hide it. The view stops following the print.
    pub fn reveal_line(&mut self, toolpath: &Toolpath, line: u32) {
        let Some(layer) = layer_of_line(toolpath, line) else {
            return;
        };
        let count = toolpath.layers.len();
        let Some(v) = &mut self.viewer else {
            return;
        };
        let Some((lo, hi)) = v.options().clamped_layer_range(count) else {
            return;
        };
        let range = (lo.min(layer), hi.max(layer));
        let scrubbed_away = layer == range.1 && v.options().line_limit.is_some_and(|l| l < line);
        if range != (lo, hi) || scrubbed_away {
            v.options_mut().layer_range = range;
            v.options_mut().line_limit = None;
            self.ov.user_moved_layers(&mut self.prefs);
        }
    }

    /// Fit the camera to the toolpath.
    pub fn fit(&mut self) {
        if let Some(v) = &mut self.viewer {
            v.fit_to_view();
        }
    }

    /// Draw the tab.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, extras: ViewerExtras<'_>) {
        let p = theme::ui_palette(ui);
        let Some(file) = ctx.file else {
            if let Some(v) = &mut self.viewer
                && v.toolpath().is_some()
            {
                v.clear();
            }
            self.ov.shown = None;
            self.empty(ui, ctx, extras.load, &p);
            return;
        };
        let bed = bed_spec(ctx.config.active());
        let toolpath = file.toolpath.clone();
        egui::Frame::new()
            .fill(p.panel)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.toolbar(ui, ctx, file, &p);
            });
        let view = ui.available_rect_before_wrap();
        ui.allocate_rect(view, Sense::hover());
        let Some(toolpath) = toolpath else {
            let mut child = ui.new_child(UiBuilder::new().max_rect(view));
            child.painter().rect_filled(view, 0.0, p.bg);
            self.not_analyzed(&mut child, ctx, file, extras.load, &p);
            return;
        };
        let Some(viewer) = self.viewer.as_mut() else {
            let mut child = ui.new_child(UiBuilder::new().max_rect(view));
            child.add_space(view.height() * 0.25);
            let r = EmptyState::new(icons::VIEW_3D, "3D view unavailable")
                .message("Sprint could not start a GPU renderer (wgpu) on this machine. The G-code, console and printer tabs work as usual.")
                .action("Show the G-code")
                .action_icon(icons::CODE)
                .show(&mut child);
            if r.action_clicked() {
                ctx.actions.push(AppAction::ShowTab(Tab::Gcode));
            }
            return;
        };

        // Keep the GPU scene, bed and options in sync with the shell.
        let key = Arc::as_ptr(&toolpath) as usize;
        if self.ov.shown != Some(key) {
            viewer.set_toolpath(Arc::clone(&toolpath));
            self.ov.shown = Some(key);
            self.ov.scrub = None;
            self.ov.scrub_layer = None;
            self.ov.followed = None;
        }
        if self.ov.bed != Some(bed) {
            viewer.set_bed(bed);
            self.ov.bed = Some(bed);
        }
        self.prefs.apply(viewer.options_mut());
        let bg = if p.dark {
            sprint_viewer::color::BACKGROUND
        } else {
            [229, 232, 237]
        };
        viewer.options_mut().background = bg;
        viewer.options_mut().highlight_line = ctx.selected_line;
        let progress = ctx
            .state
            .job
            .as_ref()
            .filter(|j| j.status.is_active() && format::job_matches_file(j, file.file.name()))
            .map(|j| j.current_line);
        viewer.options_mut().progress_line =
            progress.map(|done| (done.saturating_sub(1)).min(u32::MAX as usize) as u32);
        let layer_count = toolpath.layers.len();
        if let (Some(done), true) = (progress, self.prefs.follow_print)
            && layer_count > 0
            && self.ov.drag.is_none()
        {
            let line = done.saturating_sub(1) as u32;
            if let Some(layer) = layer_of_line(&toolpath, line)
                && self.ov.followed != Some(layer)
            {
                self.ov.followed = Some(layer);
                let lo = viewer.options().layer_range.0.min(layer);
                viewer.options_mut().layer_range = (lo, layer);
                viewer.options_mut().line_limit = None;
                self.ov.scrub = None;
            }
        }

        // Overlay geometry.
        let slider_rect = Rect::from_min_max(
            Pos2::new(view.right() - SLIDER_W - EDGE * 0.5, view.top() + EDGE),
            Pos2::new(view.right() - EDGE * 0.5, view.bottom() - EDGE),
        );
        let scrub_rect = Rect::from_min_max(
            Pos2::new(view.left() + EDGE, view.bottom() - EDGE - SCRUB_H),
            Pos2::new(slider_rect.left() - EDGE, view.bottom() - EDGE),
        );
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let over_slider = pointer.is_some_and(|pos| slider_rect.contains(pos));
        let over_scrub = pointer.is_some_and(|pos| scrub_rect.contains(pos));
        let over_view = pointer.is_some_and(|pos| view.contains(pos));

        // Wheel over the layer slider moves the top layer instead of zooming.
        if over_slider && layer_count > 0 {
            let wheel = ui.input_mut(|i| {
                i.smooth_scroll_delta = Vec2::ZERO;
                wheel_steps(&i.raw.events)
            });
            if wheel != 0 {
                let steps = wheel;
                let range = viewer.options().layer_range;
                viewer.options_mut().layer_range = move_layer(range, layer_count, true, steps);
                self.ov.user_moved_layers(&mut self.prefs);
            }
        } else if over_scrub {
            ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
        }

        // Layer keys while the view has focus or the pointer.
        let keys_active = (self.ov.focused || over_view) && !ui.ctx().egui_wants_keyboard_input();
        if keys_active && layer_count > 0 {
            let moves = ui.input_mut(|i| {
                let mut moves: Vec<(bool, i64)> = Vec::new();
                for (key, delta) in [
                    (Key::ArrowUp, 1),
                    (Key::ArrowDown, -1),
                    (Key::PageUp, 10),
                    (Key::PageDown, -10),
                    (Key::End, i64::MAX / 4),
                    (Key::Home, i64::MIN / 4),
                ] {
                    if i.consume_key(Modifiers::NONE, key) {
                        moves.push((true, delta));
                    }
                    if i.consume_key(Modifiers::SHIFT, key) {
                        moves.push((false, delta));
                    }
                }
                moves
            });
            for (top, delta) in moves {
                let range = viewer.options().layer_range;
                viewer.options_mut().layer_range = move_layer(range, layer_count, top, delta);
                self.ov.user_moved_layers(&mut self.prefs);
            }
        }

        // The view itself.
        let mut view_ui = ui.new_child(UiBuilder::new().max_rect(view));
        let response = viewer.ui(&mut view_ui);
        if let Some(line) = response.clicked_line {
            ctx.actions.push(AppAction::SelectLine(line));
        }
        if response.response.clicked() || response.response.drag_started() {
            self.ov.focused = true;
        } else if ui.input(|i| i.pointer.any_pressed()) && !over_view {
            self.ov.focused = false;
        }
        self.prefs.update_from(viewer.options());

        // Overlays (drawn after the view, so they take the clicks).
        let loading = viewer.is_loading();
        if self.prefs.show_legend {
            let legend_rect = Rect::from_min_max(
                view.min + vec2(EDGE, EDGE),
                Pos2::new(
                    (view.left() + EDGE + 300.0).min(slider_rect.left() - EDGE),
                    scrub_rect.top() - EDGE,
                ),
            );
            let mut legend_ui = ui.new_child(UiBuilder::new().max_rect(legend_rect));
            overlay_frame(&p).show(&mut legend_ui, |ui| {
                ui.set_max_width(280.0);
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(13.0));
                viewer.legend_ui(ui);
            });
        }
        if self.prefs.show_file_info {
            let right = slider_rect.left() - EDGE * 0.5;
            // Right-aligned by position: a right-aligned layout would let
            // the grid inside stretch the card to the left edge.
            let info_rect = Rect::from_min_max(
                Pos2::new((right - INFO_W).max(view.left() + EDGE), view.top() + EDGE),
                Pos2::new(right, scrub_rect.top() - EDGE),
            );
            let mut info_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(info_rect)
                    .layout(Layout::top_down(Align::Min)),
            );
            file_info_card(&mut info_ui, file, &toolpath, extras.thumbnail, &p);
        }
        if loading {
            loading_pill(ui, view, &p);
        }
        if layer_count > 0 {
            let mut range = viewer
                .options()
                .clamped_layer_range(layer_count)
                .unwrap_or((0, 0));
            let before = range;
            let mut slider_ui = ui.new_child(UiBuilder::new().max_rect(slider_rect));
            self.ov.layer_slider(
                &mut slider_ui,
                slider_rect,
                &toolpath.layers,
                &mut range,
                progress,
                &p,
            );
            if range != before {
                viewer.options_mut().layer_range = range;
                self.ov.user_moved_layers(&mut self.prefs);
            }
            let top = range.1;
            if self.ov.scrub_layer != Some(top) {
                self.ov.scrub_layer = Some(top);
                self.ov.scrub = None;
                self.ov.playing = false;
            }
            let mut scrub_ui = ui.new_child(UiBuilder::new().max_rect(scrub_rect));
            let limit = self.ov.scrub_bar(&mut scrub_ui, file, &toolpath, top, &p);
            viewer.options_mut().line_limit = limit;
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, file: &LoadedFile, p: &Palette) {
        let printing =
            ctx.state.job.as_ref().is_some_and(|j| {
                j.status.is_active() && format::job_matches_file(j, file.file.name())
            });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let mut layer_view = self.prefs.layer_view;
            if ui
                .add(
                    SegmentedControl::new(&mut layer_view, &[(false, "3D"), (true, "Layer")])
                        .id_salt("viewer-mode"),
                )
                .changed()
            {
                self.prefs.layer_view = layer_view;
                if !layer_view && let Some(v) = &mut self.viewer {
                    v.options_mut().view_mode = sprint_viewer::ViewMode::Perspective;
                }
            }
            ui.add_space(4.0);
            let mode = self.prefs.color_mode();
            egui::ComboBox::from_id_salt("viewer-color")
                .selected_text(format!("{}  {}", icons::phosphor::PALETTE, mode.name()))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for m in ColorMode::ALL {
                        if ui.selectable_label(m == mode, m.name()).clicked() {
                            self.prefs.set_color_mode(m);
                        }
                    }
                })
                .response
                .on_hover_text("Color by");
            let mut lines = self.prefs.lines;
            if ui
                .add(
                    SegmentedControl::new(&mut lines, &[(false, "Tubes"), (true, "Lines")])
                        .id_salt("viewer-style"),
                )
                .on_hover_text("Lit tubes, or thin lines for huge files and slow GPUs")
                .changed()
            {
                self.prefs.lines = lines;
            }
            ui.add_space(4.0);
            if ui
                .add(Chip::new("Travel").selected(self.prefs.show_travel))
                .on_hover_text("Show travel moves")
                .clicked()
            {
                self.prefs.show_travel = !self.prefs.show_travel;
            }
            if ui
                .add(Chip::new("Retractions").selected(self.prefs.show_retractions))
                .on_hover_text("Show retraction and prime points")
                .clicked()
            {
                self.prefs.show_retractions = !self.prefs.show_retractions;
            }
            ui.add_space(4.0);
            ui.menu_button(format!("{}  View", icons::VIEW_3D), |ui| {
                for (i, preset) in ViewPreset::ALL.into_iter().enumerate() {
                    let button =
                        egui::Button::new(preset.name()).shortcut_text(format!("{}", i + 1));
                    if ui.add(button).clicked() {
                        self.set_view(preset);
                    }
                }
                ui.separator();
                if ui
                    .add(egui::Button::new("Fit to view").shortcut_text("Z"))
                    .clicked()
                {
                    self.fit();
                }
                ui.separator();
                ui.checkbox(&mut self.prefs.show_bed, "Bed");
                ui.checkbox(&mut self.prefs.show_build_volume, "Build volume");
                ui.checkbox(&mut self.prefs.show_axes, "Axes");
                ui.checkbox(&mut self.prefs.show_tool_head, "Tool head");
            });
            if ui
                .add(
                    IconButton::new(icons::phosphor::CORNERS_OUT)
                        .ghost()
                        .tooltip("Fit to view (Z)"),
                )
                .clicked()
            {
                self.fit();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if ui
                    .add(
                        IconButton::new(icons::INFO)
                            .ghost()
                            .selected(self.prefs.show_file_info)
                            .tooltip("File information"),
                    )
                    .clicked()
                {
                    self.prefs.show_file_info = !self.prefs.show_file_info;
                }
                if ui
                    .add(
                        IconButton::new(icons::phosphor::LIST_BULLETS)
                            .ghost()
                            .selected(self.prefs.show_legend)
                            .tooltip("Legend"),
                    )
                    .clicked()
                {
                    self.prefs.show_legend = !self.prefs.show_legend;
                }
                if printing {
                    ui.add_space(6.0);
                    if ui
                        .add(
                            crate::widgets::Toggle::new(&mut self.prefs.follow_print)
                                .label("Follow print"),
                        )
                        .on_hover_text("Move the top layer with the print")
                        .changed()
                    {
                        self.ov.followed = None;
                    }
                }
                if let Some(line) = ctx.selected_line {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("Line {}", format::count(u64::from(line) + 1)))
                            .size(12.0)
                            .color(p.text_dim),
                    );
                }
            });
        });
    }

    fn empty(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, load: Option<&LoadStatus>, p: &Palette) {
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 0.0, p.bg);
        if let Some(load) = load {
            let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
            loading_card(&mut child, rect, load, ctx, p);
            ui.allocate_rect(rect, Sense::hover());
            return;
        }
        ui.add_space(rect.height() * 0.22);
        let r = EmptyState::new(icons::FILE, "No G-code loaded")
            .message("Open a sliced file to see its toolpath in 3D, or drop one anywhere on this window.")
            .action("Open G-code…")
            .action_icon(icons::OPEN)
            .show(ui);
        if r.action_clicked() {
            ctx.actions.push(AppAction::OpenFileDialog);
        }
        let recent: Vec<PathBuf> = ctx.config.recent_files.iter().take(5).cloned().collect();
        if !recent.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("RECENT")
                        .font(theme::font(ui, TextRole::Section))
                        .color(p.text_faint),
                );
                ui.add_space(2.0);
                for path in recent {
                    let name = crate::loader::display_name(&path);
                    let r = ui
                        .add(
                            egui::Button::new(icons::with_text(
                                icons::FILE,
                                &format::sanitize(&name, 60),
                            ))
                            .frame(false),
                        )
                        .on_hover_text(path.display().to_string());
                    if r.clicked() {
                        ctx.actions.push(AppAction::LoadFile(path));
                    }
                }
            });
        }
    }

    fn not_analyzed(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        file: &LoadedFile,
        load: Option<&LoadStatus>,
        p: &Palette,
    ) {
        let rect = ui.max_rect();
        if let Some(load) = load {
            loading_card(ui, rect, load, ctx, p);
            return;
        }
        let _ = file;
        ui.add_space(rect.height() * 0.25);
        let r = EmptyState::new(icons::LAYERS, "Toolpath not analyzed")
            .message(
                "The analysis was cancelled or failed. The file can still be read and printed.",
            )
            .action("Analyze")
            .action_icon(icons::REFRESH)
            .show(ui);
        if r.action_clicked() {
            ctx.actions.push(AppAction::Reanalyze);
        }
    }
}

impl Overlay {
    /// The user moved the layers: stop following the print and scrubbing.
    fn user_moved_layers(&mut self, prefs: &mut ViewerPrefs) {
        if self.followed.is_some() {
            prefs.follow_print = false;
            self.followed = None;
        }
        self.scrub = None;
    }

    #[allow(clippy::too_many_arguments)]
    fn layer_slider(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        layers: &[Layer],
        range: &mut (usize, usize),
        progress: Option<usize>,
        p: &Palette,
    ) {
        let n = layers.len();
        let track_x = rect.right() - 16.0;
        let top_y = rect.top() + 14.0;
        let bottom_y = rect.bottom() - 14.0;
        let span = (bottom_y - top_y).max(1.0);
        let y_of = |i: usize| {
            if n <= 1 {
                bottom_y
            } else {
                bottom_y - span * i as f32 / (n - 1) as f32
            }
        };
        let index_at = |y: f32| {
            if n <= 1 {
                0
            } else {
                (((bottom_y - y) / span) * (n - 1) as f32)
                    .round()
                    .clamp(0.0, (n - 1) as f32) as usize
            }
        };
        let hit = Rect::from_min_max(
            Pos2::new(track_x - 26.0, rect.top()),
            Pos2::new(rect.right(), rect.bottom()),
        );
        let id = Id::new("sprint-layer-slider");
        let response = ui.interact(hit, id, Sense::click_and_drag());
        let painter = ui.painter_at(rect.expand(4.0));
        // Track.
        let rail = Rect::from_min_max(
            Pos2::new(track_x - 2.5, top_y),
            Pos2::new(track_x + 2.5, bottom_y),
        );
        painter.rect_filled(
            rail.expand(3.0),
            CornerRadius::same(6),
            theme::with_alpha(p.surface, 0.85),
        );
        painter.rect_filled(rail, CornerRadius::same(3), p.border_strong);
        let (lo, hi) = *range;
        let sel = Rect::from_min_max(
            Pos2::new(rail.left(), y_of(hi)),
            Pos2::new(rail.right(), y_of(lo)),
        );
        painter.rect_filled(sel, CornerRadius::same(3), p.accent);
        // Print progress tick.
        if let Some(done) = progress {
            let line = done.saturating_sub(1) as u32;
            let layer = layers
                .partition_point(|l| l.lines.start <= line)
                .saturating_sub(1);
            let y = y_of(layer);
            painter.line_segment(
                [Pos2::new(track_x - 9.0, y), Pos2::new(track_x + 9.0, y)],
                Stroke::new(2.0, p.success),
            );
        }
        // Interaction.
        if (response.drag_started() || response.clicked())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let i = index_at(pos.y);
            let handle = if (y_of(hi) - pos.y).abs() <= (y_of(lo) - pos.y).abs() || i > hi {
                Handle::Top
            } else {
                Handle::Bottom
            };
            self.drag = Some(handle);
            set_handle(range, handle, i);
        }
        if response.dragged()
            && let (Some(handle), Some(pos)) = (self.drag, response.interact_pointer_pos())
        {
            set_handle(range, handle, index_at(pos.y));
        }
        if !response.dragged() {
            self.drag = None;
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        let (lo, hi) = *range;
        // Handles and labels.
        let label = |i: usize| {
            let z = layers.get(i).map_or(f32::NAN, |l| l.z);
            (format!("{}", i + 1), format!("{} mm", format::mm(z, 2)))
        };
        let hy = y_of(hi);
        let ly = y_of(lo);
        let separate = (ly - hy) >= 34.0;
        let top_label_y = if separate { hy } else { hy - 9.0 };
        let bottom_label_y = if separate { ly } else { ly + 9.0 };
        for (handle, y, label_y, i) in [
            (Handle::Bottom, ly, bottom_label_y, lo),
            (Handle::Top, hy, top_label_y, hi),
        ] {
            if handle == Handle::Bottom && lo == hi && n > 1 {
                // One handle is enough for a single layer.
            }
            let active = self.drag == Some(handle);
            let center = Pos2::new(track_x, y);
            painter.circle_filled(center, 8.0, if active { p.accent } else { p.surface });
            painter.circle_stroke(center, 8.0, Stroke::new(2.0, p.accent));
            let (index, z) = label(i);
            let font = egui::FontId::proportional(12.0);
            let g_index = painter.layout_no_wrap(
                index,
                egui::FontId::new(12.0, theme::font(ui, TextRole::Title).family),
                p.text,
            );
            let g_z = painter.layout_no_wrap(z, font, p.text_dim);
            let w = g_index.size().x.max(g_z.size().x) + 12.0;
            let h = 30.0;
            let tag =
                Rect::from_min_size(Pos2::new(track_x - 14.0 - w, label_y - h / 2.0), vec2(w, h));
            painter.rect_filled(
                tag,
                CornerRadius::same(5),
                theme::with_alpha(p.surface, 0.94),
            );
            painter.rect_stroke(
                tag,
                CornerRadius::same(5),
                Stroke::new(1.0, if active { p.accent } else { p.border }),
                StrokeKind::Inside,
            );
            painter.galley(
                Pos2::new(tag.right() - 6.0 - g_index.size().x, tag.top() + 2.0),
                g_index,
                p.text,
            );
            painter.galley(
                Pos2::new(tag.right() - 6.0 - g_z.size().x, tag.top() + 15.0),
                g_z,
                p.text_dim,
            );
        }
        response.on_hover_text(format!(
            "Layers {}–{} of {} (drag the handles; ↑/↓, PgUp/PgDn, Home/End; Shift moves the lower handle)",
            lo + 1,
            hi + 1,
            n
        ));
    }

    /// The scrub bar for the moves of layer `top`; returns the line limit.
    fn scrub_bar(
        &mut self,
        ui: &mut Ui,
        file: &LoadedFile,
        toolpath: &Toolpath,
        top: usize,
        p: &Palette,
    ) -> Option<u32> {
        let layer = toolpath.layers.get(top)?;
        let segs = layer.segments.clone();
        if segs.is_empty() {
            return None;
        }
        let count = segs.len();
        let last = count - 1;
        if self.playing {
            let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1);
            // A layer plays in about four seconds.
            self.play_accum += dt * (count as f32 / 4.0).max(10.0);
            let steps = self.play_accum.floor() as usize;
            self.play_accum -= steps as f32;
            let at = self.scrub.unwrap_or(0) + steps;
            if at >= last {
                self.scrub = None;
                self.playing = false;
            } else {
                self.scrub = Some(at);
            }
            ui.ctx().request_repaint();
        }
        let mut position = self.scrub.unwrap_or(last).min(last);
        overlay_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (icon, tip) = if self.playing {
                    (icons::PAUSE, "Pause")
                } else {
                    (icons::PLAY, "Play this layer's moves")
                };
                if ui
                    .add(IconButton::new(icon).ghost().small().tooltip(tip))
                    .clicked()
                {
                    self.playing = !self.playing;
                    if self.playing && self.scrub.is_none() {
                        self.scrub = Some(0);
                    }
                    self.play_accum = 0.0;
                }
                let segment = toolpath.segments.get(segs.start + position);
                let line = segment.map_or(0, |s| s.line);
                let text = format::sanitize(file.file.line(line as usize).trim(), 48);
                let label_w = 250.0;
                let slider_w = (ui.available_width() - label_w - 8.0).max(60.0);
                ui.spacing_mut().slider_width = slider_w;
                let slider = ui.add(
                    egui::Slider::new(&mut position, 0..=last)
                        .show_value(false)
                        .trailing_fill(true),
                );
                if slider.changed() {
                    self.playing = false;
                    self.scrub = (position < last).then_some(position);
                }
                slider.on_hover_text("Scrub through the moves of the top layer");
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(
                        RichText::new(format!(
                            "Move {} / {}  ·  line {}",
                            format::count(position as u64 + 1),
                            format::count(count as u64),
                            format::count(u64::from(line) + 1)
                        ))
                        .size(11.0)
                        .color(p.text_dim),
                    );
                    ui.add(
                        egui::Label::new(RichText::new(text).monospace().size(11.5).color(p.text))
                            .truncate(),
                    );
                });
            });
        });
        let segment = toolpath.segments.get(segs.start + position)?;
        self.scrub.map(|_| segment.line)
    }
}

/// Layers to move for this frame's mouse wheel events (one per wheel
/// notch, up = higher layers).
pub fn wheel_steps(events: &[egui::Event]) -> i64 {
    let mut lines = 0.0f32;
    for event in events {
        if let egui::Event::MouseWheel { unit, delta, .. } = event {
            lines += match unit {
                egui::MouseWheelUnit::Line => delta.y,
                egui::MouseWheelUnit::Point => delta.y / 40.0,
                egui::MouseWheelUnit::Page => delta.y * 10.0,
            };
        }
    }
    if !lines.is_finite() || lines == 0.0 {
        return 0;
    }
    let steps = lines.abs().round().clamp(1.0, 1000.0) as i64;
    steps * lines.signum() as i64
}

fn set_handle(range: &mut (usize, usize), handle: Handle, index: usize) {
    match handle {
        Handle::Top => {
            range.1 = index;
            range.0 = range.0.min(index);
        }
        Handle::Bottom => {
            range.0 = index;
            range.1 = range.1.max(index);
        }
    }
}

/// The translucent card frame of the overlays.
fn overlay_frame(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(theme::with_alpha(p.surface, 0.94))
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(theme::RADIUS_LG))
        .inner_margin(Margin::symmetric(10, 8))
        .shadow(egui::Shadow {
            offset: [0, 4],
            blur: 12,
            spread: 0,
            color: theme::with_alpha(Color32::BLACK, if p.dark { 0.35 } else { 0.10 }),
        })
}

fn loading_pill(ui: &Ui, view: Rect, p: &Palette) {
    let text = "Preparing the 3D view…";
    let painter = ui.painter_at(view);
    let galley = painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(13.0), p.text);
    let size = galley.size() + vec2(40.0, 14.0);
    let rect = Rect::from_center_size(Pos2::new(view.center().x, view.top() + 26.0), size);
    painter.rect_filled(
        rect,
        CornerRadius::same(255),
        theme::with_alpha(p.surface, 0.95),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(255),
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    let t = ui.input(|i| i.time) as f32;
    let dot = Pos2::new(rect.left() + 16.0, rect.center().y);
    painter.circle_filled(
        dot,
        4.0,
        theme::with_alpha(p.accent, 0.5 + 0.5 * (t * 4.0).sin().abs()),
    );
    painter.galley(
        Pos2::new(rect.left() + 28.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    ui.ctx().request_repaint();
}

/// The analysis progress card (centered in `rect`).
fn loading_card(ui: &mut Ui, rect: Rect, load: &LoadStatus, ctx: &mut AppCtx<'_>, p: &Palette) {
    let card = Rect::from_center_size(rect.center(), vec2(360.0, 120.0));
    let mut child = ui.new_child(UiBuilder::new().max_rect(card));
    crate::widgets::card_frame(p).show(&mut child, |ui| {
        ui.set_width(card.width() - 24.0);
        ui.horizontal(|ui| {
            ui.spinner();
            let what = match load.stage {
                Stage::Reading => "Reading",
                Stage::Analyzing => "Analyzing",
            };
            ui.label(
                RichText::new(format!("{what} {}", format::sanitize(&load.name, 36)))
                    .font(theme::font(ui, TextRole::Title))
                    .color(p.text),
            );
        });
        ui.add_space(6.0);
        match load.stage {
            Stage::Reading => ui.add(LinearProgress::indeterminate().height(6.0)),
            Stage::Analyzing => ui.add(
                LinearProgress::new(load.progress)
                    .height(6.0)
                    .show_percentage(true),
            ),
        };
        ui.add_space(6.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(ActionButton::new("Cancel").small()).clicked() {
                ctx.actions.push(AppAction::CancelLoading);
            }
            ui.label(
                RichText::new("Layers, times and the 3D view appear when it finishes.")
                    .size(11.5)
                    .color(p.text_faint),
            );
        });
    });
}

/// The floating file information card.
fn file_info_card(
    ui: &mut Ui,
    file: &LoadedFile,
    toolpath: &Toolpath,
    thumbnail: Option<&egui::TextureHandle>,
    p: &Palette,
) {
    let name = format::sanitize(file.file.name(), 40);
    let meta = &toolpath.metadata;
    let stats = &toolpath.stats;
    overlay_frame(p).show(ui, |ui| {
        ui.set_width(260.0);
        let id = ui.make_persistent_id("sprint-file-info-open");
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
        let header = ui.horizontal(|ui| {
            ui.label(RichText::new(icons::FILE).color(p.accent));
            ui.add(
                egui::Label::new(
                    RichText::new(&name)
                        .font(theme::font(ui, TextRole::Title))
                        .color(p.text),
                )
                .truncate(),
            )
            .on_hover_text(
                file.path
                    .as_ref()
                    .map_or_else(|| name.clone(), |p| p.display().to_string()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let icon = if state.is_open() {
                    icons::CARET_UP
                } else {
                    icons::CARET_DOWN
                };
                ui.add(
                    IconButton::new(icon)
                        .ghost()
                        .small()
                        .tooltip("Collapse / expand"),
                )
                .clicked()
            })
            .inner
        });
        if header.inner {
            state.toggle(ui);
        }
        state.show_body_unindented(ui, |ui| {
            ui.add_space(4.0);
            if let Some(tex) = thumbnail {
                let size = tex.size_vec2();
                if size.x > 0.0 && size.y > 0.0 {
                    let scale = (ui.available_width() / size.x).min(140.0 / size.y).min(2.0);
                    let shown = size * scale;
                    ui.vertical_centered(|ui| {
                        let (rect, _) = ui.allocate_exact_size(shown, Sense::hover());
                        ui.painter().rect_filled(
                            rect.expand(4.0),
                            CornerRadius::same(6),
                            theme::with_alpha(p.bg, 0.6),
                        );
                        ui.painter().image(
                            tex.id(),
                            rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    });
                    ui.add_space(6.0);
                }
            }
            KeyValueGrid::new("sprint-file-info")
                .row_spacing(4.0)
                .copyable(false)
                .show(ui, |kv| {
                    if let Some(slicer) = &meta.slicer {
                        kv.row("Slicer", &format::sanitize(slicer, 32));
                    }
                    let ours = f64::from(stats.estimated_time);
                    kv.row("Estimate", &format::duration(ours));
                    if let Some(theirs) = meta.estimated_time {
                        let diff = format::relative_difference(ours, f64::from(theirs))
                            .map(|d| format!("  ({d})"))
                            .unwrap_or_default();
                        kv.row(
                            "Slicer says",
                            &format!("{}{diff}", format::duration(f64::from(theirs))),
                        );
                    }
                    let grams = if stats.filament_weight > 0.0 {
                        format!(" · {:.1} g", stats.filament_weight)
                    } else {
                        meta.filament_used_g
                            .map(|g| format!(" · {g:.1} g"))
                            .unwrap_or_default()
                    };
                    kv.row(
                        "Filament",
                        &format!(
                            "{}{grams}",
                            format::length_mm(f64::from(stats.filament_length))
                        ),
                    );
                    let height = if stats.layer_height > 0.0 {
                        format!(" × {} mm", format::mm(stats.layer_height, 3))
                    } else {
                        String::new()
                    };
                    kv.row(
                        "Layers",
                        &format!("{}{height}", format::count(toolpath.layers.len() as u64)),
                    );
                    if !toolpath.bounds.is_empty() {
                        let [x, y, z] = toolpath.bounds.size();
                        kv.row(
                            "Size",
                            &format!(
                                "{} × {} × {} mm",
                                format::mm(x, 1),
                                format::mm(y, 1),
                                format::mm(z, 1)
                            ),
                        );
                    }
                    if let Some(kind) = &meta.filament_type {
                        kv.row("Material", &format::sanitize(kind, 24));
                    }
                    if !meta.object_names.is_empty() {
                        let all = meta.object_names.join(", ");
                        let shown = format::sanitize(&all, 36);
                        kv.row("Objects", &shown)
                            .on_hover_text(format::sanitize(&all, 600));
                    }
                });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_moves_stay_ordered_and_in_range() {
        assert_eq!(move_layer((0, 9), 10, true, -1), (0, 8));
        assert_eq!(move_layer((0, 9), 10, true, 5), (0, 9));
        assert_eq!(move_layer((5, 6), 10, true, -3), (3, 3));
        assert_eq!(move_layer((5, 6), 10, false, 3), (8, 8));
        assert_eq!(move_layer((5, 6), 10, false, i64::MIN / 4), (0, 6));
        assert_eq!(move_layer((5, 6), 10, true, i64::MAX / 4), (5, 9));
        assert_eq!(move_layer((0, usize::MAX), 10, true, 0), (0, 9));
        assert_eq!(move_layer((3, 4), 0, true, 1), (0, 0));
        let mut r = (2, 5);
        set_handle(&mut r, Handle::Top, 1);
        assert_eq!(r, (1, 1));
        set_handle(&mut r, Handle::Bottom, 7);
        assert_eq!(r, (7, 7));
    }

    #[test]
    fn bed_from_profile() {
        let mut profile = sprint_host::PrinterProfile {
            bed_size: [300.0, 200.0],
            max_z: 400.0,
            origin: BedOrigin::Center,
            ..Default::default()
        };
        let bed = bed_spec(&profile);
        assert_eq!(bed.size, [300.0, 200.0]);
        assert!(bed.origin_center);
        profile.bed_size = [f32::NAN, -1.0];
        assert_eq!(bed_spec(&profile).size, BedSpec::default().size);
    }
}
