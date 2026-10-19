//! Bed-screw helper: how far to turn each leveling screw so the bed
//! matches a reference screw, from the mesh heights interpolated at the
//! screws ([`crate::insight::mesh`]). A bed plan shows where the screws
//! are and what to do with each; a list spells it out; the positions,
//! thread and turning direction are configurable.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2,
};
use sprint_proto::BedMesh;

use crate::icons;
use crate::insight::mesh::{
    DEFAULT_SCREW_INSET, MeshGeometry, Screw, ScrewAdjustment, THREADS, TurnDirection,
    default_screws, interpolate, screw_adjustments,
};
use crate::insight::{format, ui as kit};
use crate::theme::{self, Palette, TextRole, with_alpha};
use crate::widgets::{
    ActionButton, Card, IconButton, NumericField, SegmentedControl, diverging_color, signed_value,
};

/// Most screws the editor allows.
pub const MAX_SCREWS: usize = 9;
/// Fewest screws (a bed needs three points).
const MIN_SCREWS: usize = 3;

/// State of the screw helper.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrewHelper {
    screws: Vec<Screw>,
    /// The bed size the corner screws were placed for; `None` once the user
    /// edited the positions.
    placed_for: Option<[f32; 2]>,
    inset: f32,
    thread: usize,
    reference: usize,
    clockwise_lowers: bool,
    editing: bool,
}

impl Default for ScrewHelper {
    fn default() -> Self {
        ScrewHelper {
            screws: Vec::new(),
            placed_for: None,
            inset: DEFAULT_SCREW_INSET,
            thread: 0,
            reference: 0,
            clockwise_lowers: true,
            editing: false,
        }
    }
}

/// Color of a screw marker.
fn marker_color(p: &Palette, adj: Option<&ScrewAdjustment>) -> Color32 {
    match adj {
        None => p.text_faint,
        Some(a) if a.reference => p.accent,
        Some(a) => match a.severity() {
            0 => p.success,
            1 => p.warning,
            _ => p.danger,
        },
    }
}

/// Glyph inside a screw marker.
fn marker_glyph(adj: Option<&ScrewAdjustment>) -> &'static str {
    match adj {
        None => icons::HELP,
        Some(a) if a.reference => icons::phosphor::CROSSHAIR,
        Some(a) => match a.direction {
            None => icons::CHECK,
            Some(TurnDirection::Clockwise) => icons::phosphor::ARROW_CLOCKWISE,
            Some(TurnDirection::CounterClockwise) => icons::phosphor::ARROW_COUNTER_CLOCKWISE,
        },
    }
}

/// The short label next to a marker: "REF", "OK", "1/4", "1 1/2".
fn short_label(adj: Option<&ScrewAdjustment>) -> String {
    match adj {
        None => "?".to_owned(),
        Some(a) if a.reference => "REF".to_owned(),
        Some(a) if a.direction.is_none() => "OK".to_owned(),
        Some(a) => {
            let label = crate::insight::mesh::turns_label(a.turns);
            let short = label
                .trim_end_matches(" turns")
                .trim_end_matches(" turn")
                .to_owned();
            if short.starts_with("less") {
                "<1/8".to_owned()
            } else {
                short
            }
        }
    }
}

impl ScrewHelper {
    /// The screws (placed at the corners of `bed_size` until edited).
    pub fn screws(&self) -> &[Screw] {
        &self.screws
    }

    /// Index of the reference screw.
    pub fn reference(&self) -> usize {
        self.reference
    }

    /// Choose the reference screw (ignored when out of range).
    pub fn set_reference(&mut self, index: usize) {
        if index < self.screws.len() {
            self.reference = index;
        }
    }

    /// The thread pitch in use (mm per turn).
    pub fn pitch(&self) -> f32 {
        THREADS.get(self.thread).map_or(0.5, |t| t.pitch)
    }

    /// Place the corner screws for `bed_size` unless the user edited them.
    pub fn sync(&mut self, bed_size: [f32; 2]) {
        let custom = self.placed_for.is_none() && !self.screws.is_empty();
        if !custom && self.placed_for != Some(bed_size) {
            self.screws = default_screws(bed_size, self.inset);
            self.placed_for = Some(bed_size);
        }
        if self.reference >= self.screws.len() {
            self.reference = 0;
        }
    }

    /// The adjustment of every screw, by screw index.
    pub fn adjustments(&self, mesh: &BedMesh, geom: &MeshGeometry) -> Vec<Option<ScrewAdjustment>> {
        let list = screw_adjustments(
            mesh,
            geom,
            &self.screws,
            self.reference,
            self.pitch(),
            self.clockwise_lowers,
        );
        let mut out = vec![None; self.screws.len()];
        for adj in list {
            if let Some(slot) = out.get_mut(adj.index) {
                *slot = Some(adj);
            }
        }
        out
    }

    /// Draw the card.
    pub fn ui(&mut self, ui: &mut Ui, mesh: &BedMesh, geom: &MeshGeometry, bed_size: [f32; 2]) {
        self.sync(bed_size);
        let adjustments = self.adjustments(mesh, geom);
        let thread = THREADS.get(self.thread).copied().unwrap_or(THREADS[0]);
        let subtitle = format!(
            "{} · {} mm per turn",
            thread.name,
            format::trimmed(f64::from(thread.pitch), 2)
        );
        let pending = adjustments
            .iter()
            .flatten()
            .filter(|a| a.direction.is_some())
            .count();
        Card::new("Bed screws")
            .icon(icons::phosphor::WRENCH)
            .subtitle(&subtitle)
            .show_with_header(
                ui,
                |ui| {
                    let (text, tone) = if adjustments.iter().all(Option::is_none) {
                        ("No data".to_owned(), theme::Tone::Neutral)
                    } else if pending == 0 {
                        ("Level".to_owned(), theme::Tone::Success)
                    } else {
                        (
                            format!("{pending} to adjust"),
                            if adjustments.iter().flatten().any(|a| a.severity() >= 2) {
                                theme::Tone::Danger
                            } else {
                                theme::Tone::Warning
                            },
                        )
                    };
                    ui.add(crate::widgets::Badge::new(&text).tone(tone));
                },
                |ui| {
                    self.controls(ui);
                    ui.add_space(10.0);
                    if let Some(index) = bed_plan(
                        ui,
                        mesh,
                        geom,
                        bed_size,
                        &self.screws,
                        &adjustments,
                    ) {
                        self.set_reference(index);
                    }
                    ui.add_space(10.0);
                    let mut new_reference = None;
                    for (i, screw) in self.screws.iter().enumerate() {
                        if screw_row(ui, screw, adjustments.get(i).and_then(Option::as_ref)) {
                            new_reference = Some(i);
                        }
                    }
                    if let Some(i) = new_reference {
                        self.set_reference(i);
                    }
                    ui.add_space(6.0);
                    kit::caption(
                        ui,
                        "Click a screw to make it the reference. Heights between probe points are interpolated; screws outside the probed area use its nearest edge. Adjust, then probe again to check.",
                    );
                    ui.add_space(6.0);
                    self.editor(ui, bed_size);
                },
            );
    }

    fn controls(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
            let threads: Vec<(usize, String)> = THREADS
                .iter()
                .enumerate()
                .map(|(i, t)| (i, format!("{} · {}", t.name, format::trimmed(f64::from(t.pitch), 1))))
                .collect();
            ui.add(SegmentedControl::new(&mut self.thread, &threads).id_salt("screw-thread"));
            let directions = [
                (true, "Clockwise lowers the bed"),
                (false, "Clockwise raises it"),
            ];
            ui.add(
                SegmentedControl::new(&mut self.clockwise_lowers, &directions)
                    .id_salt("screw-direction"),
            )
            .on_hover_text(
                "What turning a screw clockwise (seen from where you turn it) does to the bed on this printer",
            );
        });
    }

    fn editor(&mut self, ui: &mut Ui, bed_size: [f32; 2]) {
        let label = if self.editing {
            "Done editing positions"
        } else {
            "Edit screw positions"
        };
        if ui
            .add(
                ActionButton::new(label)
                    .icon(if self.editing {
                        icons::CHECK
                    } else {
                        icons::EDIT
                    })
                    .ghost()
                    .small(),
            )
            .clicked()
        {
            self.editing = !self.editing;
        }
        if !self.editing {
            return;
        }
        ui.add_space(4.0);
        let max_x = f64::from(bed_size[0].max(1.0)) * 1.5;
        let max_y = f64::from(bed_size[1].max(1.0)) * 1.5;
        let mut remove = None;
        let mut edited = false;
        let count = self.screws.len();
        for (i, screw) in self.screws.iter_mut().enumerate() {
            ui.push_id(("screw-edit", i), |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let name = ui.add(
                        egui::TextEdit::singleline(&mut screw.name)
                            .desired_width(96.0)
                            .char_limit(24),
                    );
                    edited |= name.changed();
                    let mut x = f64::from(screw.x);
                    let mut y = f64::from(screw.y);
                    let rx = ui.add(
                        NumericField::new(&mut x)
                            .label("X")
                            .unit("mm")
                            .decimals(1)
                            .range(-max_x..=max_x)
                            .field_width(84.0),
                    );
                    let ry = ui.add(
                        NumericField::new(&mut y)
                            .label("Y")
                            .unit("mm")
                            .decimals(1)
                            .range(-max_y..=max_y)
                            .field_width(84.0),
                    );
                    if rx.changed() || ry.changed() {
                        screw.x = x as f32;
                        screw.y = y as f32;
                        edited = true;
                    }
                    if ui
                        .add_enabled(
                            count > MIN_SCREWS,
                            IconButton::new(icons::DELETE)
                                .ghost()
                                .small()
                                .tooltip("Remove this screw"),
                        )
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            self.screws.remove(i);
            if self.reference >= self.screws.len() || self.reference == i {
                self.reference = 0;
            } else if self.reference > i {
                self.reference -= 1;
            }
            edited = true;
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui
                .add_enabled(
                    self.screws.len() < MAX_SCREWS,
                    ActionButton::new("Add screw").icon(icons::ADD).small(),
                )
                .clicked()
            {
                let n = self.screws.len() + 1;
                self.screws.push(Screw::new(
                    format!("Screw {n}"),
                    bed_size[0] / 2.0,
                    bed_size[1] / 2.0,
                ));
                edited = true;
            }
            ui.add_space(8.0);
            ui.add(
                NumericField::new(&mut self.inset)
                    .label("Corner inset")
                    .unit("mm")
                    .decimals(0)
                    .range(0.0..=100.0)
                    .field_width(76.0)
                    .hint("Distance of the corner screws from the bed edges"),
            );
            if ui
                .add(
                    ActionButton::new("Four corners")
                        .icon(icons::RESET)
                        .small()
                        .tooltip("Place four screws at the corners, this far in from the edges"),
                )
                .clicked()
            {
                self.screws = default_screws(bed_size, self.inset);
                self.placed_for = Some(bed_size);
                self.reference = 0;
                edited = false;
            }
        });
        if edited {
            self.placed_for = None;
        }
    }
}

/// One screw in the list; returns whether it was clicked.
fn screw_row(ui: &mut Ui, screw: &Screw, adj: Option<&ScrewAdjustment>) -> bool {
    let p = theme::ui_palette(ui);
    let width = ui.available_width();
    let height = 44.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let instruction = adj.map_or_else(
        || "No mesh height here".to_owned(),
        ScrewAdjustment::instruction,
    );
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            true,
            format!("{}: {instruction}", screw.name),
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let color = marker_color(&p, adj);
        if response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), p.raised);
        }
        let center = pos2(rect.min.x + 20.0, rect.center().y);
        painter.circle_filled(center, 13.0, with_alpha(color, 0.18));
        painter.circle_stroke(center, 13.0, Stroke::new(1.5, color));
        painter.text(
            center,
            Align2::CENTER_CENTER,
            marker_glyph(adj),
            kit::icon_font(ui, 14.0),
            color,
        );
        let x = rect.min.x + 42.0;
        let title_font = FontId::new(13.0, theme::font(ui, TextRole::Title).family);
        let right_w = (width * 0.55).min(300.0);
        let name_w = (width - 42.0 - right_w - 8.0).max(40.0);
        let name = kit::truncated(ui, &screw.name, title_font.clone(), p.text, name_w);
        let pos_text = format!(
            "X{} Y{}{}",
            format::trimmed(f64::from(screw.x), 1),
            format::trimmed(f64::from(screw.y), 1),
            if adj.is_some_and(|a| a.outside) {
                " · outside mesh"
            } else {
                ""
            }
        );
        let pos_g = kit::truncated(
            ui,
            &pos_text,
            FontId::proportional(11.5),
            p.text_dim,
            name_w,
        );
        painter.galley(pos2(x, rect.min.y + 5.0), name, p.text);
        painter.galley(pos2(x, rect.min.y + 24.0), pos_g, p.text_dim);
        // Right side: what to do, and the height.
        let (headline, detail) = match adj {
            None => ("—".to_owned(), String::new()),
            Some(a) if a.reference => (
                "Reference".to_owned(),
                format!("{} mm", signed_value(a.z, 3)),
            ),
            Some(a) => {
                let side = if a.delta >= 0.0 { "high" } else { "low" };
                let detail = format!("{} mm {side}", format::fixed(f64::from(a.delta.abs()), 2));
                match a.direction {
                    None => ("OK".to_owned(), detail),
                    Some(d) => (
                        format!(
                            "{} {}",
                            crate::insight::mesh::turns_label(a.turns),
                            match d {
                                TurnDirection::Clockwise => "clockwise",
                                TurnDirection::CounterClockwise => "counter-clockwise",
                            }
                        ),
                        detail,
                    ),
                }
            }
        };
        let head_color = match adj {
            Some(a) if a.direction.is_some() => color,
            Some(a) if a.reference => p.accent,
            _ => p.text,
        };
        let head = kit::truncated(ui, &headline, title_font, head_color, right_w);
        let det = kit::truncated(ui, &detail, FontId::proportional(11.5), p.text_dim, right_w);
        let rx = rect.max.x - 8.0;
        painter.galley(pos2(rx - head.size().x, rect.min.y + 5.0), head, head_color);
        painter.galley(pos2(rx - det.size().x, rect.min.y + 24.0), det, p.text_dim);
    }
    let clicked = response.clicked();
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Use this screw as the reference");
    clicked
}

/// The bed seen from above (front at the bottom) with the mesh as a faint
/// heat background, the probed area and the screws. Returns the index of a
/// clicked screw.
fn bed_plan(
    ui: &mut Ui,
    mesh: &BedMesh,
    geom: &MeshGeometry,
    bed_size: [f32; 2],
    screws: &[Screw],
    adjustments: &[Option<ScrewAdjustment>],
) -> Option<usize> {
    let p = theme::ui_palette(ui);
    let [bw, bd] = bed_size.map(|s| if s.is_finite() && s > 0.0 { s } else { 220.0 });
    let width = ui.available_width().clamp(120.0, 440.0);
    let height = (width * bd / bw).clamp(110.0, 340.0);
    let mut clicked = None;
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        let (outer, response) = ui.allocate_exact_size(vec2(width, height + 18.0), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "Bed plan"));
        // Leave room for markers on the edges.
        let pad = 16.0;
        let avail = Rect::from_min_max(
            outer.min + vec2(pad, pad),
            pos2(outer.max.x - pad, outer.max.y - 18.0 - pad + 8.0),
        );
        let s = (avail.width() / bw).min(avail.height() / bd);
        let bed = Rect::from_center_size(avail.center(), vec2(bw * s, bd * s));
        let to_screen = |x: f32, y: f32| pos2(bed.min.x + x * s, bed.max.y - y * s);
        let markers: Vec<Pos2> = screws
            .iter()
            .map(|sc| {
                let pt = to_screen(sc.x, sc.y);
                pos2(
                    pt.x.clamp(outer.min.x + 12.0, outer.max.x - 12.0),
                    pt.y.clamp(outer.min.y + 12.0, outer.max.y - 30.0),
                )
            })
            .collect();
        let hovered = response
            .hover_pos()
            .and_then(|pos| markers.iter().position(|m| m.distance(pos) <= 16.0));
        if response.clicked() {
            clicked = response
                .interact_pointer_pos()
                .and_then(|pos| markers.iter().position(|m| m.distance(pos) <= 16.0));
        }
        if !ui.is_rect_visible(outer) {
            return;
        }
        let painter = ui.painter_at(outer);
        painter.rect(
            bed,
            CornerRadius::same(theme::RADIUS),
            p.raised,
            Stroke::new(1.0, p.border_strong),
            StrokeKind::Inside,
        );
        // Mesh heat in the probed area.
        if let Some([x0, y0, x1, y1]) = geom.bounds() {
            let scale = mesh
                .min()
                .zip(mesh.max())
                .map_or(1.0, |(lo, hi)| lo.abs().max(hi.abs()))
                .max(1e-3);
            // A smooth, vertex-colored grid (no seams between cells).
            let n = 32usize;
            let (dx, dy) = ((x1 - x0) / n as f32, (y1 - y0) / n as f32);
            if dx > 0.0 && dy > 0.0 {
                let mut heat = egui::Mesh::default();
                let mut known = Vec::with_capacity((n + 1) * (n + 1));
                for j in 0..=n {
                    for i in 0..=n {
                        let (x, y) = (x0 + i as f32 * dx, y0 + j as f32 * dy);
                        let sample = interpolate(mesh, geom, x, y);
                        let color = sample.map_or(Color32::TRANSPARENT, |sm| {
                            with_alpha(diverging_color(sm.z / scale), 0.8)
                        });
                        known.push(sample.is_some());
                        heat.colored_vertex(to_screen(x, y), color);
                    }
                }
                let at = |i: usize, j: usize| (j * (n + 1) + i) as u32;
                for j in 0..n {
                    for i in 0..n {
                        let quad = [at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)];
                        if quad.iter().all(|&k| known[k as usize]) {
                            heat.add_triangle(quad[0], quad[1], quad[2]);
                            heat.add_triangle(quad[0], quad[2], quad[3]);
                        }
                    }
                }
                painter.add(heat);
            }
            let area = Rect::from_two_pos(to_screen(x0, y1), to_screen(x1, y0));
            let dash = Stroke::new(1.0, with_alpha(p.text_dim, 0.7));
            let corners = [
                area.left_top(),
                area.right_top(),
                area.right_bottom(),
                area.left_bottom(),
                area.left_top(),
            ];
            painter.extend(Shape::dashed_line(&corners, dash, 4.0, 3.0));
        }
        // Screws.
        for (i, pos) in markers.iter().enumerate() {
            let adj = adjustments.get(i).and_then(Option::as_ref);
            let color = marker_color(&p, adj);
            let r = if hovered == Some(i) { 14.0 } else { 12.5 };
            painter.circle_filled(*pos, r, p.surface);
            painter.circle_filled(*pos, r, with_alpha(color, 0.22));
            painter.circle_stroke(*pos, r, Stroke::new(2.0, color));
            painter.text(
                *pos,
                Align2::CENTER_CENTER,
                marker_glyph(adj),
                kit::icon_font(ui, 14.0),
                color,
            );
            // The short label towards the bed's centre.
            let label = short_label(adj);
            let toward_center = if pos.x < bed.center().x { 1.0 } else { -1.0 };
            let anchor = if toward_center > 0.0 {
                Align2::LEFT_CENTER
            } else {
                Align2::RIGHT_CENTER
            };
            let lp = pos2(pos.x + toward_center * (r + 5.0), pos.y);
            let font = FontId::new(12.0, theme::font(ui, TextRole::Title).family);
            let galley = painter.layout_no_wrap(label, font, color);
            let text_rect = anchor
                .anchor_size(lp, galley.size())
                .expand2(vec2(4.0, 1.0));
            painter.rect_filled(
                text_rect,
                CornerRadius::same(4),
                with_alpha(p.surface, 0.85),
            );
            painter.galley(text_rect.min + vec2(4.0, 1.0), galley, color);
        }
        painter.text(
            pos2(bed.center().x, outer.max.y - 2.0),
            Align2::CENTER_BOTTOM,
            "Front",
            FontId::proportional(11.0),
            p.text_dim,
        );
        if let Some(i) = hovered
            && let Some(screw) = screws.get(i)
        {
            let text = adjustments.get(i).and_then(Option::as_ref).map_or_else(
                || "No mesh height here".to_owned(),
                ScrewAdjustment::instruction,
            );
            response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text_at_pointer(format!("{}\n{text}", screw.name));
        }
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tilted() -> (BedMesh, MeshGeometry) {
        let rows: Vec<Vec<f32>> = (0..3)
            .map(|r| (0..3).map(|c| 0.1 * c as f32 + 0.05 * r as f32).collect())
            .collect();
        let geom = MeshGeometry {
            xs: vec![10.0, 110.0, 210.0],
            ys: vec![10.0, 110.0, 210.0],
            reported: true,
        };
        (BedMesh::from_rows(rows, "bilinear"), geom)
    }

    #[test]
    fn corners_follow_the_bed_until_edited() {
        let mut h = ScrewHelper::default();
        h.sync([220.0, 220.0]);
        assert_eq!(h.screws().len(), 4);
        assert_eq!((h.screws()[2].x, h.screws()[2].y), (190.0, 190.0));
        h.sync([300.0, 200.0]);
        assert_eq!(h.screws()[2].x, 270.0);
        // Edited positions stay.
        h.placed_for = None;
        h.screws[0].x = 5.0;
        h.sync([220.0, 220.0]);
        assert_eq!(h.screws()[0].x, 5.0);
    }

    #[test]
    fn adjustments_by_index_and_reference() {
        let (mesh, geom) = tilted();
        let mut h = ScrewHelper::default();
        h.sync([220.0, 220.0]);
        let adj = h.adjustments(&mesh, &geom);
        assert_eq!(adj.len(), 4);
        assert!(adj[0].as_ref().is_some_and(|a| a.reference));
        // Front right is 0.16 mm higher than front left: lower it.
        let fr = adj[1].as_ref().expect("front right");
        assert!((fr.delta - 0.16).abs() < 1e-4, "{}", fr.delta);
        assert_eq!(fr.direction, Some(TurnDirection::Clockwise));
        h.clockwise_lowers = false;
        let fr = h.adjustments(&mesh, &geom)[1].clone().expect("front right");
        assert_eq!(fr.direction, Some(TurnDirection::CounterClockwise));
        h.set_reference(2);
        assert_eq!(h.reference(), 2);
        h.set_reference(99);
        assert_eq!(h.reference(), 2);
        // M5: 0.8 mm per turn.
        h.thread = 2;
        assert_eq!(h.pitch(), 0.8);
    }

    #[test]
    fn labels_for_markers() {
        let base = ScrewAdjustment {
            index: 0,
            z: 0.0,
            delta: 0.125,
            turns: 0.25,
            direction: Some(TurnDirection::Clockwise),
            reference: false,
            outside: false,
        };
        assert_eq!(short_label(Some(&base)), "1/4");
        let small = ScrewAdjustment {
            turns: 0.04,
            ..base.clone()
        };
        assert_eq!(short_label(Some(&small)), "<1/8");
        let ok = ScrewAdjustment {
            direction: None,
            ..base.clone()
        };
        assert_eq!(short_label(Some(&ok)), "OK");
        let reference = ScrewAdjustment {
            reference: true,
            ..base.clone()
        };
        assert_eq!(short_label(Some(&reference)), "REF");
        assert_eq!(short_label(None), "?");
        let many = ScrewAdjustment { turns: 1.5, ..base };
        assert_eq!(short_label(Some(&many)), "1 1/2");
    }
}
