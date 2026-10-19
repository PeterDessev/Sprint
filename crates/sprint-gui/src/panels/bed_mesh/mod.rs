//! Bed mesh tab: the leveling mesh as a heatmap (front row at the bottom,
//! as seen from above) with its statistics, an isometric surface preview,
//! probing (G29) and loading (M420 V), leveling on/off and fade height
//! (M420 S/Z), and a bed-screw helper that says how far to turn each
//! leveling screw.

mod screws;
mod surface;

pub use screws::ScrewHelper;

use egui::{
    Align, Align2, CornerRadius, Layout, Mesh, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder,
    pos2, vec2,
};
use sprint_proto::{BedMesh, SettingCommand};

use crate::context::AppCtx;
use crate::icons;
use crate::insight::mesh::{MeshGeometry, flatness_label};
use crate::insight::{format, ui as kit};
use crate::theme::{self, Tone, with_alpha};
use crate::widgets::{
    ActionButton, Badge, Card, ConfirmDialog, DialogResult, EmptyState, Heatmap, LinearProgress,
    NumericField, StatusPill, Toggle, diverging_color, signed_value,
};

/// Below this width the cards stack in one column.
const TWO_COLUMNS: f32 = 900.0;
/// Give up waiting for a probe's mesh after this long (seconds).
const PROBE_TIMEOUT: f64 = 900.0;
/// Fade height when the firmware reports none (Marlin's default, mm).
const DEFAULT_FADE: f32 = 10.0;

/// A probe started from this tab.
#[derive(Clone, Debug)]
struct Probing {
    /// UI time when it was sent.
    started: f64,
    /// The mesh before probing (to notice the new one).
    before: Option<BedMesh>,
}

/// Bed mesh tab: heatmap, statistics, probing and bed screw adjustment.
#[derive(Debug)]
pub struct BedMeshTab {
    screws: ScrewHelper,
    /// Surface preview direction (degrees).
    azimuth: f32,
    confirm_probe: bool,
    probing: Option<Probing>,
    /// The stored mesh was requested for this connection.
    auto_requested: bool,
    /// Fade height being edited (mm); `None` = the reported one.
    fade: Option<f32>,
}

impl Default for BedMeshTab {
    fn default() -> Self {
        BedMeshTab {
            screws: ScrewHelper::default(),
            azimuth: surface::DEFAULT_AZIMUTH,
            confirm_probe: false,
            probing: None,
            auto_requested: false,
            fade: None,
        }
    }
}

/// Leveling state from the settings (M420 S), else the last "Bed Leveling
/// ON/OFF" message.
fn leveling_enabled(state: &sprint_host::PrinterState) -> Option<bool> {
    if let Some(s) = state.settings.value("M420", 'S').filter(|v| v.is_finite()) {
        return Some(s >= 0.5);
    }
    state.firmware_messages.iter().rev().find_map(|m| {
        let m = m.trim();
        if m.eq_ignore_ascii_case("Bed Leveling ON") {
            Some(true)
        } else if m.eq_ignore_ascii_case("Bed Leveling OFF") {
            Some(false)
        } else {
            None
        }
    })
}

/// The reported fade height (mm, 0 = off).
fn fade_height(state: &sprint_host::PrinterState) -> Option<f32> {
    state
        .settings
        .value("M420", 'Z')
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v as f32)
}

/// The probe's XY offset (M851).
fn probe_xy(state: &sprint_host::PrinterState) -> Option<[f32; 2]> {
    state.settings.probe_offset().map(|o| [o[0], o[1]])
}

impl BedMeshTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        self.track(ui, ctx);
        let state = ctx.state;
        let bed_size = ctx.config.active().bed_size;
        let mesh = state.mesh.as_ref().filter(|m| m.count() > 0);
        let geom = mesh.and_then(|m| MeshGeometry::of(m, bed_size, probe_xy(state)));
        self.header(ui, ctx, mesh);
        ui.add_space(theme::GAP);
        if let Some(p) = &self.probing {
            let elapsed = ui.input(|i| i.time) - p.started;
            ui.add(
                LinearProgress::indeterminate()
                    .label(&format!("Probing the bed… {}", format::clock(elapsed))),
            );
            ui.add_space(theme::GAP);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        match (mesh, geom) {
            (Some(mesh), Some(geom)) => {
                egui::ScrollArea::vertical()
                    .id_salt("bed-mesh-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.body(ui, ctx, mesh, &geom, bed_size);
                        ui.add_space(theme::GAP_LG);
                    });
            }
            _ => self.empty(ui, ctx),
        }
        self.dialogs(ui, ctx);
    }

    /// The surface preview direction (degrees).
    pub fn azimuth(&self) -> f32 {
        self.azimuth
    }

    /// The bed screw helper.
    pub fn screws(&self) -> &ScrewHelper {
        &self.screws
    }

    /// Whether a probe started here is still waiting for its mesh.
    pub fn is_probing(&self) -> bool {
        self.probing.is_some()
    }

    /// Connection bookkeeping, the automatic load and probe completion.
    fn track(&mut self, ui: &Ui, ctx: &mut AppCtx<'_>) {
        if !ctx.connected() {
            self.auto_requested = false;
            self.probing = None;
            return;
        }
        if !self.auto_requested && ctx.state.mesh.is_none() && !ctx.job_active() {
            ctx.printer.request_bed_mesh();
            self.auto_requested = true;
        }
        if let Some(p) = &self.probing {
            let now = ui.input(|i| i.time);
            if ctx.state.mesh.is_some() && ctx.state.mesh != p.before {
                let range = ctx.state.mesh.as_ref().and_then(BedMesh::range);
                let toast = ctx.toasts.success("Bed probed");
                if let Some(r) = range {
                    toast.message(format!(
                        "Range {} mm ({})",
                        format::fixed(f64::from(r), 3),
                        flatness_label(r).0.to_lowercase()
                    ));
                }
                // G29 switches leveling on: refresh M420's state.
                let m503 = ctx
                    .state
                    .firmware
                    .as_ref()
                    .is_none_or(|f| f.flavor.supports_m503());
                if m503 {
                    ctx.printer.request_settings();
                }
                self.probing = None;
            } else if now - p.started > PROBE_TIMEOUT || now < p.started {
                self.probing = None;
            }
        }
    }

    fn header(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, mesh: Option<&BedMesh>) {
        let state = ctx.state;
        let subtitle = match mesh {
            Some(m) => {
                let kind = match m.kind.as_str() {
                    "bilinear" => "Bilinear",
                    "mbl" => "Mesh bed leveling",
                    "ubl" => "Unified bed leveling",
                    "klipper_probed" => "Klipper probed",
                    "klipper_mesh" => "Klipper mesh",
                    "" => "Mesh",
                    other => other,
                };
                let mut s = format!("{kind} · {} × {} points", m.cols(), m.rows());
                if let Some(r) = m.range() {
                    s.push_str(&format!(" · {} mm range", format::fixed(f64::from(r), 3)));
                }
                s
            }
            None => "Probe the bed or load the stored mesh".to_owned(),
        };
        let connected = ctx.connected();
        let job = ctx.job_active();
        let probing = self.probing.is_some();
        let printer = ctx.printer;
        let mut probe = false;
        kit::page_header(ui, icons::MESH, "Bed mesh", Some(&subtitle), |ui| {
            let tip = if !connected {
                "Connect to a printer first"
            } else if job {
                "Not while a job runs"
            } else if probing {
                "Probing…"
            } else {
                "Home if needed and probe a new mesh (G29)"
            };
            if ui
                .add_enabled(
                    connected && !job && !probing,
                    ActionButton::new("Probe bed")
                        .icon(icons::MESH)
                        .primary()
                        .tooltip(tip),
                )
                .clicked()
            {
                probe = true;
            }
            if ui
                .add_enabled(
                    connected,
                    ActionButton::new("Load")
                        .icon(icons::DOWNLOAD)
                        .tooltip("Read the mesh stored in the printer (M420 V)"),
                )
                .clicked()
            {
                printer.request_bed_mesh();
            }
            if let Some(on) = leveling_enabled(state) {
                let (text, tone) = if on {
                    ("Leveling on", Tone::Success)
                } else {
                    ("Leveling off", Tone::Neutral)
                };
                ui.add(StatusPill::new(text, tone));
            }
        });
        if probe {
            self.confirm_probe = true;
        }
    }

    fn empty(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        ui.add_space(40.0);
        if !ctx.connected() {
            let _ = EmptyState::new(icons::MESH, "Not connected")
                .message("Connect to a printer to load or probe its bed mesh.")
                .show(ui);
            return;
        }
        let message = if self.probing.is_some() {
            "Probing… the mesh appears here when the printer reports it."
        } else {
            "The printer has not reported a mesh. Probe the bed (G29) to measure one; it homes first if needed."
        };
        let r = EmptyState::new(icons::MESH, "No bed mesh")
            .message(message)
            .action("Probe bed")
            .action_icon(icons::MESH)
            .show(ui);
        if r.action_clicked() && !ctx.job_active() && self.probing.is_none() {
            self.confirm_probe = true;
        }
    }

    fn body(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        mesh: &BedMesh,
        geom: &MeshGeometry,
        bed_size: [f32; 2],
    ) {
        let width = ui.available_width();
        if width >= TWO_COLUMNS {
            let gap = theme::GAP_LG;
            let left_w = ((width - gap) * 0.54).floor();
            let right_w = width - gap - left_w;
            let top = ui.cursor().min;
            let bottom = ui.max_rect().bottom().max(top.y + 1.0);
            let mut left = ui.new_child(
                UiBuilder::new()
                    .id_salt("bed-mesh-left")
                    .max_rect(Rect::from_min_max(top, pos2(top.x + left_w, bottom)))
                    .layout(Layout::top_down(Align::Min)),
            );
            left.set_width(left_w);
            self.topography(&mut left, mesh, geom);
            left.add_space(gap);
            self.surface(&mut left, mesh, geom);
            let mut right = ui.new_child(
                UiBuilder::new()
                    .id_salt("bed-mesh-right")
                    .max_rect(Rect::from_min_max(
                        pos2(top.x + left_w + gap, top.y),
                        pos2(top.x + width, bottom),
                    ))
                    .layout(Layout::top_down(Align::Min)),
            );
            right.set_width(right_w);
            self.leveling(&mut right, ctx);
            right.add_space(gap);
            self.screws.ui(&mut right, mesh, geom, bed_size);
            let height = left.min_rect().height().max(right.min_rect().height());
            ui.advance_cursor_after_rect(Rect::from_min_size(top, vec2(width, height)));
        } else {
            self.topography(ui, mesh, geom);
            ui.add_space(theme::GAP_LG);
            self.leveling(ui, ctx);
            ui.add_space(theme::GAP_LG);
            self.screws.ui(ui, mesh, geom, bed_size);
            ui.add_space(theme::GAP_LG);
            self.surface(ui, mesh, geom);
        }
    }

    fn topography(&mut self, ui: &mut Ui, mesh: &BedMesh, geom: &MeshGeometry) {
        let range = mesh.range();
        let (verdict, severity) = flatness_label(range.unwrap_or(f32::NAN));
        Card::new("Topography")
            .icon(icons::MESH)
            .subtitle("from above, front at the bottom")
            .show_with_header(
                ui,
                |ui| {
                    let tone = match severity {
                        0 => Tone::Success,
                        1 => Tone::Warning,
                        _ => Tone::Danger,
                    };
                    ui.add(Badge::new(verdict).tone(tone));
                },
                |ui| {
                    let scale = color_scale(mesh);
                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                        Heatmap::new(&mesh.points)
                            .positions(Some(&geom.xs), Some(&geom.ys))
                            .legend(false)
                            .scale(scale)
                            .max_cell(72.0)
                            .id_salt("bed-mesh-heatmap")
                            .show(ui);
                        ui.add_space(8.0);
                        let bar_w = (geom.xs.len() as f32 * 72.0).min(ui.available_width()).max(160.0);
                        color_bar(ui, scale, bar_w);
                    });
                    ui.add_space(10.0);
                    let p = theme::ui_palette(ui);
                    let v = |x: Option<f32>| x.map_or_else(|| format::MISSING.to_owned(), |x| format!("{} mm", signed_value(x, 3)));
                    let range_text = range.map_or_else(|| format::MISSING.to_owned(), |r| format!("{} mm", format::fixed(f64::from(r), 3)));
                    let range_color = match severity {
                        0 => None,
                        1 => Some(p.warning),
                        _ => Some(p.danger),
                    };
                    let columns = if ui.available_width() >= 420.0 { 5 } else { 3 };
                    kit::metric_grid(
                        ui,
                        columns,
                        &[
                            ("Lowest", v(mesh.min()), None),
                            ("Highest", v(mesh.max()), None),
                            ("Range", range_text, range_color),
                            ("Mean", v(mesh.mean()), None),
                            ("Std dev", mesh.std_dev().map_or_else(|| format::MISSING.to_owned(), |s| format!("{} mm", format::fixed(f64::from(s), 3))), None),
                        ],
                    );
                    let total = mesh.rows() * mesh.cols();
                    let missing = total.saturating_sub(mesh.count());
                    if missing > 0 {
                        kit::caption(ui, &format!("{missing} of {total} points were not probed (hatched)."));
                    }
                    if !geom.reported
                        && let Some([x0, y0, x1, y1]) = geom.bounds()
                    {
                            kit::caption(
                                ui,
                                &format!(
                                    "The firmware does not report probe positions; assuming Marlin's default area X{}–{} Y{}–{} mm.",
                                    format::trimmed(f64::from(x0), 1),
                                    format::trimmed(f64::from(x1), 1),
                                    format::trimmed(f64::from(y0), 1),
                                    format::trimmed(f64::from(y1), 1),
                                ),
                            );
                    }
                },
            );
    }

    fn surface(&mut self, ui: &mut Ui, mesh: &BedMesh, geom: &MeshGeometry) {
        Card::new("Surface")
            .icon(icons::VIEW_3D)
            .subtitle("drag to turn, double-click to reset")
            .show(ui, |ui| match surface::Surface::build(mesh, geom) {
                Some(s) => {
                    let height = (ui.available_width() * 0.62).clamp(180.0, 340.0);
                    surface::show(ui, &s, &mut self.azimuth, color_scale(mesh), height);
                }
                None => {
                    kit::caption(ui, "A surface needs at least 2 × 2 probed points.");
                }
            });
    }

    fn leveling(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let state = ctx.state;
        let enabled = leveling_enabled(state);
        let reported_fade = fade_height(state);
        let connected = ctx.connected();
        let job = ctx.job_active();
        Card::new("Leveling")
            .icon(icons::LAYERS)
            .subtitle("M420")
            .show(ui, |ui| {
                let mut on = enabled.unwrap_or(false);
                let toggle = ui.add_enabled(
                    connected && !job,
                    Toggle::new(&mut on).label("Apply the mesh while printing"),
                );
                let toggle = if job {
                    toggle.on_disabled_hover_text("Not while a job runs")
                } else {
                    toggle
                };
                if toggle.changed() {
                    let mut params = vec![('S', if on { 1.0 } else { 0.0 })];
                    if let Some(z) = reported_fade {
                        params.push(('Z', f64::from(z)));
                    }
                    ctx.printer.apply_setting(&SettingCommand::new("M420", &params));
                }
                ui.add_space(2.0);
                kit::caption(
                    ui,
                    if enabled.is_none() {
                        "The firmware has not reported whether leveling is on."
                    } else if on {
                        "Z follows the mesh, so the first layer stays even across the bed."
                    } else {
                        "The mesh is stored but not applied: moves ignore the bed's shape."
                    },
                );
                ui.add_space(8.0);
                let mut fade = self.fade.or(reported_fade).unwrap_or(DEFAULT_FADE);
                let changed_from_reported = reported_fade.is_none_or(|r| (r - fade).abs() > 1e-3);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            NumericField::new(&mut fade)
                                .label("Fade height")
                                .unit("mm")
                                .decimals(1)
                                .range(0.0..=100.0)
                                .label_width(96.0)
                                .field_width(96.0)
                                .hint("The correction fades out linearly up to this height; 0 keeps it on for the whole print"),
                        )
                        .changed()
                    {
                        self.fade = Some(fade);
                    }
                    if ui
                        .add_enabled(
                            connected && self.fade.is_some() && changed_from_reported,
                            ActionButton::new("Set").small().tooltip(format!("M420 Z{}", format::trimmed(f64::from(fade), 1))),
                        )
                        .clicked()
                    {
                        let mut params = Vec::new();
                        if let Some(on) = enabled {
                            params.push(('S', if on { 1.0 } else { 0.0 }));
                        }
                        params.push(('Z', f64::from(fade)));
                        ctx.printer.apply_setting(&SettingCommand::new("M420", &params));
                        self.fade = None;
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            connected,
                            ActionButton::new("Save to EEPROM")
                                .icon(icons::SAVE)
                                .small()
                                .tooltip("M500: keep the mesh and these settings after a restart (saves every setting)"),
                        )
                        .clicked()
                    {
                        ctx.printer.save_settings();
                        ctx.toasts.success("Saved to EEPROM").message("The mesh and the leveling settings survive a restart now.");
                    }
                });
            });
    }

    fn dialogs(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        if !self.confirm_probe {
            return;
        }
        let points = ctx
            .state
            .mesh
            .as_ref()
            .map(|m| m.rows() * m.cols())
            .filter(|n| *n > 0);
        let message = format!(
            "The printer homes all axes if needed, then probes {} with G29 and reports the new mesh. Heat the bed to printing temperature first for a representative mesh, and make sure the nozzle is clean and the bed is clear.",
            points.map_or_else(|| "the grid".to_owned(), |n| format!("{n} points"))
        );
        let answer = ConfirmDialog::new("bed-mesh-probe", "Probe the bed?")
            .message(&message)
            .details("G28 (if needed)\nG29\nM420 V")
            .confirm_label("Home and probe")
            .icon(icons::MESH)
            .show(ui.ctx());
        match answer {
            Some(DialogResult::Confirmed) => {
                self.confirm_probe = false;
                if ctx.connected() && !ctx.job_active() {
                    ctx.printer.probe_bed_mesh();
                    self.probing = Some(Probing {
                        started: ui.input(|i| i.time),
                        before: ctx.state.mesh.clone(),
                    });
                }
            }
            Some(DialogResult::Cancelled) => self.confirm_probe = false,
            None => {}
        }
    }
}

/// The colormap half-range: the largest deviation from zero.
fn color_scale(mesh: &BedMesh) -> f32 {
    let s = mesh
        .min()
        .zip(mesh.max())
        .map_or(0.0, |(lo, hi)| lo.abs().max(hi.abs()));
    if s.is_finite() && s > 1e-4 { s } else { 0.1 }
}

/// The diverging color bar with its end values.
fn color_bar(ui: &mut Ui, scale: f32, width: f32) {
    let p = theme::ui_palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let bar = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.min.y + 7.0));
    let mut mesh = Mesh::default();
    let steps = 32u32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = egui::lerp(bar.x_range(), t);
        let c = diverging_color(t * 2.0 - 1.0);
        mesh.colored_vertex(pos2(x, bar.min.y), c);
        mesh.colored_vertex(pos2(x, bar.max.y), c);
        if i > 0 {
            let k = 2 * i;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    let painter = ui.painter();
    painter.add(mesh);
    painter.rect_stroke(
        bar,
        CornerRadius::same(2),
        Stroke::new(1.0, with_alpha(p.border_strong, 0.6)),
        StrokeKind::Outside,
    );
    let font = theme::font(ui, theme::TextRole::Caption);
    let y = bar.max.y + 3.0;
    painter.text(
        pos2(bar.min.x, y),
        Align2::LEFT_TOP,
        format!("{} mm low", signed_value(-scale, 3)),
        font.clone(),
        p.text_dim,
    );
    painter.text(
        pos2(bar.center().x, y),
        Align2::CENTER_TOP,
        "0",
        font.clone(),
        p.text_dim,
    );
    painter.text(
        pos2(bar.max.x, y),
        Align2::RIGHT_TOP,
        format!("{} mm high", signed_value(scale, 3)),
        font,
        p.text_dim,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::PrinterState;

    #[test]
    fn leveling_state_from_settings_or_messages() {
        let mut state = PrinterState::default();
        assert_eq!(leveling_enabled(&state), None);
        state.firmware_messages.push_back("Bed Leveling OFF".into());
        state.firmware_messages.push_back("Bed Leveling ON".into());
        assert_eq!(leveling_enabled(&state), Some(true));
        state
            .settings
            .insert(SettingCommand::new("M420", &[('S', 0.0), ('Z', 5.0)]));
        assert_eq!(leveling_enabled(&state), Some(false));
        assert_eq!(fade_height(&state), Some(5.0));
    }

    #[test]
    fn color_scale_is_never_zero() {
        assert_eq!(color_scale(&BedMesh::default()), 0.1);
        let flat = BedMesh::from_rows(vec![vec![0.0; 3]; 3], "bilinear");
        assert_eq!(color_scale(&flat), 0.1);
        let m = BedMesh::from_rows(vec![vec![-0.2, 0.1], vec![0.05, 0.0]], "bilinear");
        assert!((color_scale(&m) - 0.2).abs() < 1e-6);
    }
}
