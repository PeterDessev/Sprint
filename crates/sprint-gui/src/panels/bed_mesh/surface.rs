//! Isometric surface preview of a bed mesh, painted with egui's
//! [`egui::Painter`]: the mesh as a shaded slab seen from the front left
//! and above, heights exaggerated so a tenth of a millimetre shows. Drag to
//! turn it, double-click to reset.

use egui::{
    Align2, Color32, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo,
    WidgetType, pos2, vec2,
};
use sprint_proto::BedMesh;

use crate::insight::mesh::{MeshGeometry, interpolate};
use crate::insight::ui as kit;
use crate::theme::{self, Palette, TextRole, with_alpha};
use crate::widgets::diverging_color;

/// Default view direction (degrees): from the front left.
pub const DEFAULT_AZIMUTH: f32 = 28.0;
/// Camera elevation above the bed plane (degrees).
const ELEVATION: f32 = 34.0;
/// Most surface cells drawn (the probed grid is subdivided up to this).
const MAX_CELLS: usize = 3600;
/// The exaggerated relief spans this fraction of the probed area's extent.
const RELIEF: f32 = 0.2;

/// A camera turning around the bed's vertical axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    azimuth: f32,
    elevation: f32,
}

impl Camera {
    /// A camera at `azimuth_deg` (0 = straight from the front, positive =
    /// from the left) and the default elevation.
    pub fn new(azimuth_deg: f32) -> Camera {
        let azimuth = if azimuth_deg.is_finite() {
            azimuth_deg
        } else {
            DEFAULT_AZIMUTH
        };
        Camera {
            azimuth: azimuth.to_radians(),
            elevation: ELEVATION.to_radians(),
        }
    }

    /// Screen offset (x right, y down; in world units) and depth (larger =
    /// further from the viewer) of a world point relative to the centre.
    pub fn project(&self, p: [f32; 3]) -> (Vec2, f32) {
        let (s, c) = self.azimuth.sin_cos();
        let xr = p[0] * c - p[1] * s;
        let yr = p[0] * s + p[1] * c;
        let (se, ce) = self.elevation.sin_cos();
        (vec2(xr, -(yr * se) - p[2] * ce), yr * ce - p[2] * se)
    }
}

/// The mesh resampled on a finer grid, centred on the probed area.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    /// Points per row.
    pub nx: usize,
    /// Rows.
    pub ny: usize,
    /// `[x, y, z]` relative to the centre of the probed area (mm), row by
    /// row from the front; z is NaN where nothing could be interpolated.
    pub points: Vec<[f32; 3]>,
    /// Subdivisions per probed cell.
    pub subdivisions: usize,
    /// Half extents of the probed area (mm).
    pub half: [f32; 2],
}

impl Surface {
    /// Resample `mesh` (at least 2 × 2 points). `None` when it is smaller,
    /// has no probed point or its geometry is degenerate.
    pub fn build(mesh: &BedMesh, geom: &MeshGeometry) -> Option<Surface> {
        let (cols, rows) = (geom.xs.len(), geom.ys.len());
        if cols < 2 || rows < 2 || mesh.count() == 0 {
            return None;
        }
        let [x0, y0, x1, y1] = geom.bounds()?;
        if !(x1 > x0 && y1 > y0) {
            return None;
        }
        let cells = (cols - 1) * (rows - 1);
        let subdivisions = ((MAX_CELLS as f32 / cells as f32).sqrt().floor() as usize).clamp(1, 8);
        let nx = (cols - 1) * subdivisions + 1;
        let ny = (rows - 1) * subdivisions + 1;
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let axis = |lo: &[f32], i: usize| -> f32 {
            let cell = (i / subdivisions).min(lo.len() - 2);
            let t = (i - cell * subdivisions) as f32 / subdivisions as f32;
            lo[cell] + (lo[cell + 1] - lo[cell]) * t
        };
        let mut points = Vec::with_capacity(nx * ny);
        for j in 0..ny {
            let y = axis(&geom.ys, j);
            for i in 0..nx {
                let x = axis(&geom.xs, i);
                let z = interpolate(mesh, geom, x, y).map_or(f32::NAN, |s| s.z);
                points.push([x - cx, y - cy, z]);
            }
        }
        Some(Surface {
            nx,
            ny,
            points,
            subdivisions,
            half: [(x1 - x0) / 2.0, (y1 - y0) / 2.0],
        })
    }

    fn at(&self, i: usize, j: usize) -> [f32; 3] {
        self.points[j * self.nx + i]
    }

    /// Lowest and highest z.
    pub fn z_range(&self) -> Option<(f32, f32)> {
        let mut it = self.points.iter().map(|p| p[2]).filter(|z| z.is_finite());
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), z| (lo.min(z), hi.max(z))))
    }

    /// Height exaggeration so the relief spans [`RELIEF`] of the extent.
    pub fn exaggeration(&self) -> f32 {
        let extent = self.half[0].max(self.half[1]) * 2.0;
        match self.z_range() {
            Some((lo, hi)) if hi - lo > 1e-4 => (RELIEF * extent / (hi - lo)).clamp(1.0, 5000.0),
            _ => 1.0,
        }
    }

    /// The surface cells (index of the lower-left point) with all four
    /// corners known, sorted from the furthest to the nearest.
    pub fn cells_back_to_front(&self, camera: &Camera, zscale: f32) -> Vec<(usize, usize)> {
        let mut cells: Vec<(f32, usize, usize)> = Vec::new();
        for j in 0..self.ny.saturating_sub(1) {
            for i in 0..self.nx.saturating_sub(1) {
                let corners = [
                    self.at(i, j),
                    self.at(i + 1, j),
                    self.at(i + 1, j + 1),
                    self.at(i, j + 1),
                ];
                if corners.iter().any(|p| !p[2].is_finite()) {
                    continue;
                }
                let depth = corners
                    .iter()
                    .map(|p| camera.project([p[0], p[1], p[2] * zscale]).1)
                    .sum::<f32>()
                    / 4.0;
                cells.push((depth, i, j));
            }
        }
        cells.sort_by(|a, b| b.0.total_cmp(&a.0));
        cells.into_iter().map(|(_, i, j)| (i, j)).collect()
    }
}

/// Lambert shading factor of a cell (0.6..=1.0) for a light from the front
/// left above.
fn shade(corners: &[[f32; 3]; 4]) -> f32 {
    let d1 = sub(corners[2], corners[0]);
    let d2 = sub(corners[3], corners[1]);
    let n = cross(d1, d2);
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len.is_nan() || len <= 0.0 {
        return 1.0;
    }
    let light = [-0.35, -0.55, 0.76];
    let dot = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]) / len;
    0.6 + 0.4 * dot.clamp(0.0, 1.0)
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn scaled(color: Color32, factor: f32) -> Color32 {
    let f = |c: u8| (f32::from(c) * factor).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(f(color.r()), f(color.g()), f(color.b()), color.a())
}

/// Draw the surface preview `height` points tall. `azimuth` (degrees) is
/// changed by dragging and reset by a double-click; `scale` is the
/// colormap half-range (the heatmap's, so colors match).
pub fn show(
    ui: &mut Ui,
    surface: &Surface,
    azimuth: &mut f32,
    scale: f32,
    height: f32,
) -> egui::Response {
    let p = theme::ui_palette(ui);
    let width = ui.available_width().max(120.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "Surface preview"));
    if response.dragged() {
        let dx = response.drag_delta().x;
        if dx.is_finite() {
            *azimuth = (*azimuth + dx * 0.4).rem_euclid(360.0);
        }
    }
    if response.double_clicked() {
        *azimuth = DEFAULT_AZIMUTH;
    }
    let response = response.on_hover_cursor(egui::CursorIcon::Grab);
    if ui.is_rect_visible(rect) {
        paint(ui, &p, rect, surface, Camera::new(*azimuth), scale);
    }
    response
}

/// Fit projected world offsets into `rect`.
struct Fit {
    origin: Pos2,
    scale: f32,
}

impl Fit {
    fn new(rect: Rect, offsets: impl Iterator<Item = Vec2>) -> Option<Fit> {
        let mut bb = Rect::NOTHING;
        for o in offsets {
            if o.x.is_finite() && o.y.is_finite() {
                bb.extend_with(o.to_pos2());
            }
        }
        if !(bb.width() > 0.0 && bb.height() > 0.0) {
            return None;
        }
        let inner = rect.shrink2(vec2(16.0, 22.0));
        let scale = (inner.width() / bb.width()).min(inner.height() / bb.height());
        let origin = inner.center() - bb.center().to_vec2() * scale;
        Some(Fit { origin, scale })
    }

    fn pos(&self, offset: Vec2) -> Pos2 {
        self.origin + offset * self.scale
    }
}

fn paint(ui: &Ui, p: &Palette, rect: Rect, surface: &Surface, camera: Camera, scale: f32) {
    let painter = ui.painter_at(rect);
    let Some((zmin, zmax)) = surface.z_range() else {
        return;
    };
    let zscale = surface.exaggeration();
    let scale = if scale.is_finite() && scale > 1e-6 {
        scale
    } else {
        zmax.abs().max(zmin.abs()).max(1e-3)
    };
    let [hx, hy] = surface.half;
    // The slab's base sits a little under the lowest point.
    let base = zmin * zscale - (hx.max(hy) * 0.06);
    let world = |pt: [f32; 3]| [pt[0], pt[1], pt[2] * zscale];
    let corners = [[-hx, -hy], [hx, -hy], [hx, hy], [-hx, hy]];
    let offsets = surface
        .points
        .iter()
        .filter(|pt| pt[2].is_finite())
        .map(|pt| camera.project(world(*pt)).0)
        .chain(corners.iter().map(|c| camera.project([c[0], c[1], base]).0));
    let Some(fit) = Fit::new(rect, offsets) else {
        return;
    };
    let screen = |pt: [f32; 3]| fit.pos(camera.project(pt).0);

    // The base plate.
    let plate: Vec<Pos2> = corners.iter().map(|c| screen([c[0], c[1], base])).collect();
    painter.add(Shape::convex_polygon(
        plate,
        with_alpha(p.text_faint, 0.10),
        Stroke::new(1.0, with_alpha(p.text_faint, 0.35)),
    ));

    // Side walls along the four edges, back ones before the surface and
    // front ones after it.
    let edge_points = |side: usize| -> Vec<[f32; 3]> {
        let (nx, ny) = (surface.nx, surface.ny);
        let idx: Vec<(usize, usize)> = match side {
            0 => (0..nx).map(|i| (i, 0)).collect(),
            1 => (0..ny).map(|j| (nx - 1, j)).collect(),
            2 => (0..nx).rev().map(|i| (i, ny - 1)).collect(),
            _ => (0..ny).rev().map(|j| (0, j)).collect(),
        };
        idx.into_iter()
            .map(|(i, j)| world(surface.at(i, j)))
            .collect()
    };
    let wall_fill = if p.dark {
        scaled(p.raised, 1.35)
    } else {
        scaled(p.border, 0.97)
    };
    let draw_wall = |painter: &egui::Painter, side: usize| {
        let top = edge_points(side);
        let mut mesh = Mesh::default();
        for w in top.windows(2) {
            if !(w[0][2].is_finite() && w[1][2].is_finite()) {
                continue;
            }
            let k = mesh.vertices.len() as u32;
            let shade_factor = if side.is_multiple_of(2) { 1.0 } else { 0.86 };
            let color = scaled(wall_fill, shade_factor);
            mesh.colored_vertex(screen(w[0]), color);
            mesh.colored_vertex(screen(w[1]), color);
            mesh.colored_vertex(screen([w[1][0], w[1][1], base]), color);
            mesh.colored_vertex(screen([w[0][0], w[0][1], base]), color);
            mesh.add_triangle(k, k + 1, k + 2);
            mesh.add_triangle(k, k + 2, k + 3);
        }
        painter.add(mesh);
    };
    let center_depth = camera.project([0.0, 0.0, base]).1;
    let side_depth = |side: usize| {
        let c = match side {
            0 => [0.0, -hy],
            1 => [hx, 0.0],
            2 => [0.0, hy],
            _ => [-hx, 0.0],
        };
        camera.project([c[0], c[1], base]).1
    };
    let (front, back): (Vec<usize>, Vec<usize>) =
        (0..4).partition(|s| side_depth(*s) < center_depth);
    for side in &back {
        draw_wall(&painter, *side);
    }

    // The surface, far cells first.
    let mut mesh = Mesh::default();
    for (i, j) in surface.cells_back_to_front(&camera, zscale) {
        let corners = [
            surface.at(i, j),
            surface.at(i + 1, j),
            surface.at(i + 1, j + 1),
            surface.at(i, j + 1),
        ];
        let w = corners.map(world);
        let light = shade(&w);
        let k = mesh.vertices.len() as u32;
        for c in &corners {
            let color = scaled(diverging_color(c[2] / scale), light);
            mesh.colored_vertex(screen(world(*c)), color);
        }
        mesh.add_triangle(k, k + 1, k + 2);
        mesh.add_triangle(k, k + 2, k + 3);
    }
    painter.add(mesh);

    // The probed grid lines.
    let line = Stroke::new(
        1.0,
        if p.dark {
            Color32::from_black_alpha(70)
        } else {
            Color32::from_black_alpha(45)
        },
    );
    let step = surface.subdivisions.max(1);
    let polyline = |pts: Vec<[f32; 3]>| {
        let mut run: Vec<Pos2> = Vec::new();
        for pt in pts {
            if pt[2].is_finite() {
                run.push(screen(world(pt)));
            } else if run.len() > 1 {
                painter.add(Shape::line(std::mem::take(&mut run), line));
            } else {
                run.clear();
            }
        }
        if run.len() > 1 {
            painter.add(Shape::line(run, line));
        }
    };
    for j in (0..surface.ny).step_by(step) {
        polyline((0..surface.nx).map(|i| surface.at(i, j)).collect());
    }
    for i in (0..surface.nx).step_by(step) {
        polyline((0..surface.ny).map(|j| surface.at(i, j)).collect());
    }

    for side in &front {
        draw_wall(&painter, *side);
    }

    // Orientation: "Front" under the front edge.
    let font = FontId::proportional(11.0);
    let front_mid = screen([0.0, -hy, base]);
    let back_mid = screen([0.0, hy, base]);
    let away = (front_mid - back_mid).normalized();
    let label_pos = front_mid + away * 12.0;
    if rect.shrink(4.0).contains(label_pos) {
        painter.text(label_pos, Align2::CENTER_CENTER, "Front", font, p.text_dim);
    }
    let caption = theme::font(ui, TextRole::Caption);
    let note = format!("Height ×{}", exaggeration_label(zscale));
    let g = kit::truncated(ui, &note, caption, p.text_faint, rect.width());
    painter.galley(
        pos2(rect.max.x - g.size().x - 4.0, rect.min.y + 2.0),
        g,
        p.text_faint,
    );
}

/// "120", "1.5k": the exaggeration for the caption.
pub fn exaggeration_label(zscale: f32) -> String {
    if !zscale.is_finite() {
        return "1".to_owned();
    }
    if zscale >= 1000.0 {
        format!("{:.1}k", zscale / 1000.0)
    } else if zscale >= 10.0 {
        format!("{}", (zscale / 5.0).round() * 5.0)
    } else {
        format!("{}", zscale.round().max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(cols: usize, rows: usize) -> MeshGeometry {
        let lin = |n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| 10.0 + 200.0 * i as f32 / (n - 1).max(1) as f32)
                .collect()
        };
        MeshGeometry {
            xs: lin(cols),
            ys: lin(rows),
            reported: true,
        }
    }

    fn tilted(cols: usize, rows: usize) -> BedMesh {
        let rows: Vec<Vec<f32>> = (0..rows)
            .map(|r| {
                (0..cols)
                    .map(|c| 0.01 * c as f32 - 0.02 * r as f32)
                    .collect()
            })
            .collect();
        BedMesh::from_rows(rows, "bilinear")
    }

    #[test]
    fn surface_resamples_and_centres() {
        let s = Surface::build(&tilted(5, 5), &geom(5, 5)).expect("surface");
        assert_eq!(s.nx, (5 - 1) * s.subdivisions + 1);
        assert_eq!(s.points.len(), s.nx * s.ny);
        assert!(s.nx * s.ny <= (MAX_CELLS as f64 * 1.3) as usize);
        assert_eq!(s.half, [100.0, 100.0]);
        // Corners keep the probed values.
        let first = s.points[0];
        assert_eq!([first[0], first[1]], [-100.0, -100.0]);
        assert!(first[2].abs() < 1e-6);
        let last = *s.points.last().expect("points");
        assert!((last[2] - (0.04 - 0.08)).abs() < 1e-5);
        let (lo, hi) = s.z_range().expect("range");
        assert!((lo + 0.08).abs() < 1e-5 && (hi - 0.04).abs() < 1e-5);
        // 20% of 200 mm over a 0.12 mm range.
        assert!((s.exaggeration() - 40.0 / 0.12).abs() < 0.5);
    }

    #[test]
    fn surface_needs_a_two_by_two_grid() {
        assert!(Surface::build(&tilted(1, 5), &geom(1, 5)).is_none());
        assert!(Surface::build(&BedMesh::default(), &geom(3, 3)).is_none());
        let empty = BedMesh::from_rows(vec![vec![f32::NAN; 3]; 3], "bilinear");
        assert!(Surface::build(&empty, &geom(3, 3)).is_none());
    }

    #[test]
    fn flat_meshes_are_not_exaggerated() {
        let flat = BedMesh::from_rows(vec![vec![0.05; 3]; 3], "bilinear");
        let s = Surface::build(&flat, &geom(3, 3)).expect("surface");
        assert_eq!(s.exaggeration(), 1.0);
    }

    #[test]
    fn cells_are_drawn_far_to_near() {
        let s = Surface::build(&tilted(3, 3), &geom(3, 3)).expect("surface");
        let cam = Camera::new(0.0);
        let cells = s.cells_back_to_front(&cam, 1.0);
        assert_eq!(cells.len(), (s.nx - 1) * (s.ny - 1));
        // Seen from the front, the back row comes first.
        assert_eq!(cells.first().map(|c| c.1), Some(s.ny - 2));
        assert_eq!(cells.last().map(|c| c.1), Some(0));
    }

    #[test]
    fn camera_projects_front_low_and_back_high() {
        let cam = Camera::new(0.0);
        let (front, df) = cam.project([0.0, -100.0, 0.0]);
        let (back, db) = cam.project([0.0, 100.0, 0.0]);
        assert!(front.y > back.y, "the front is lower on screen");
        assert!(df < db, "the front is nearer");
        let (up, _) = cam.project([0.0, 0.0, 10.0]);
        assert!(up.y < 0.0, "higher points are higher on screen");
        assert_eq!(Camera::new(f32::NAN), Camera::new(DEFAULT_AZIMUTH));
    }

    #[test]
    fn shading_and_labels() {
        let flat = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let s = shade(&flat);
        assert!((0.6..=1.0).contains(&s));
        let degenerate = [[0.0; 3]; 4];
        assert_eq!(shade(&degenerate), 1.0);
        assert_eq!(exaggeration_label(333.3), "335");
        assert_eq!(exaggeration_label(1500.0), "1.5k");
        assert_eq!(exaggeration_label(3.4), "3");
        assert_eq!(exaggeration_label(f32::NAN), "1");
    }
}
