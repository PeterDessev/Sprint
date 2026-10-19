//! Bed mesh geometry, interpolation and the bed-screw helper.
//!
//! A [`BedMesh`] holds probed heights `points[row][col]` (row 0 = front,
//! column 0 = left). Marlin's bilinear report does not say where the points
//! were probed, so [`MeshGeometry::of`] falls back to Marlin's default
//! probing area (a [`PROBING_MARGIN`] around the bed, shifted by the probe
//! offset) when the mesh carries no positions.
//!
//! The screw helper interpolates the mesh at each leveling screw and turns
//! the height differences into turns of the screw relative to a reference
//! screw: "turn clockwise 1/4 turn (0.12 mm high)".

use sprint_proto::BedMesh;

/// Marlin's default `PROBING_MARGIN` (mm): how far from the bed edges the
/// probe stays.
pub const PROBING_MARGIN: f32 = 10.0;
/// Default distance of the leveling screws from the bed edges (mm).
pub const DEFAULT_SCREW_INSET: f32 = 30.0;
/// Height differences below this (mm) need no adjustment.
pub const SCREW_TOLERANCE: f32 = 0.02;

/// Where the mesh points lie on the bed.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshGeometry {
    /// X of each column (mm), strictly increasing.
    pub xs: Vec<f32>,
    /// Y of each row (mm), strictly increasing.
    pub ys: Vec<f32>,
    /// The positions came from the firmware (not assumed).
    pub reported: bool,
}

fn strictly_increasing(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite()) && v.windows(2).all(|w| w[0] < w[1])
}

fn linspace(lo: f32, hi: f32, n: usize) -> Vec<f32> {
    match n {
        0 => Vec::new(),
        1 => vec![(lo + hi) / 2.0],
        _ => (0..n)
            .map(|i| lo + (hi - lo) * i as f32 / (n - 1) as f32)
            .collect(),
    }
}

/// Marlin's reachable probe range along one axis: at least
/// [`PROBING_MARGIN`] from both edges and where the probe (offset from the
/// nozzle) can reach.
fn probe_range(size: f32, offset: f32) -> (f32, f32) {
    let lo = PROBING_MARGIN.max(offset);
    let hi = (size - PROBING_MARGIN).min(size + offset);
    if hi > lo {
        (lo, hi)
    } else {
        // A probe offset too large for the bed: fall back to the margins.
        (
            PROBING_MARGIN.min(size / 4.0),
            (size - PROBING_MARGIN).max(size * 0.75),
        )
    }
}

impl MeshGeometry {
    /// The geometry of `mesh`: its reported positions when they match the
    /// grid and increase, otherwise Marlin's default probing area on a bed
    /// of `bed_size` with the probe's XY offset (from M851; zero when
    /// unknown). `None` for an empty mesh or a non-positive bed size.
    pub fn of(
        mesh: &BedMesh,
        bed_size: [f32; 2],
        probe_offset: Option<[f32; 2]>,
    ) -> Option<MeshGeometry> {
        let (rows, cols) = (mesh.rows(), mesh.cols());
        if rows == 0 || cols == 0 {
            return None;
        }
        let xs = mesh
            .x_positions
            .as_ref()
            .filter(|xs| xs.len() == cols && strictly_increasing(xs));
        let ys = mesh
            .y_positions
            .as_ref()
            .filter(|ys| ys.len() == rows && strictly_increasing(ys));
        if let (Some(xs), Some(ys)) = (xs, ys) {
            return Some(MeshGeometry {
                xs: xs.clone(),
                ys: ys.clone(),
                reported: true,
            });
        }
        if !bed_size.iter().all(|s| s.is_finite() && *s > 0.0) {
            return None;
        }
        let offset = probe_offset
            .filter(|o| o.iter().all(|v| v.is_finite()))
            .unwrap_or([0.0, 0.0]);
        let (x0, x1) = probe_range(bed_size[0], offset[0]);
        let (y0, y1) = probe_range(bed_size[1], offset[1]);
        Some(MeshGeometry {
            xs: linspace(x0, x1, cols),
            ys: linspace(y0, y1, rows),
            reported: false,
        })
    }

    /// The probed area: (min x, min y, max x, max y).
    pub fn bounds(&self) -> Option<[f32; 4]> {
        Some([
            *self.xs.first()?,
            *self.ys.first()?,
            *self.xs.last()?,
            *self.ys.last()?,
        ])
    }
}

/// An interpolated mesh height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshSample {
    /// Height (mm).
    pub z: f32,
    /// The position lies outside the probed area; the nearest edge of the
    /// mesh was used (flat extrapolation, like Marlin).
    pub outside: bool,
}

/// Locate `p` on `axis`: the cell index and the fraction within it
/// (clamped to the axis).
fn locate(axis: &[f32], p: f32) -> (usize, f32) {
    let n = axis.len();
    if n < 2 {
        return (0, 0.0);
    }
    if p <= axis[0] {
        return (0, 0.0);
    }
    if p >= axis[n - 1] {
        return (n - 2, 1.0);
    }
    let i = axis.windows(2).position(|w| p < w[1]).unwrap_or(n - 2);
    let span = axis[i + 1] - axis[i];
    let t = if span > 0.0 {
        (p - axis[i]) / span
    } else {
        0.0
    };
    (i, t.clamp(0.0, 1.0))
}

/// The mesh height at bed position (`x`, `y`) by bilinear interpolation.
///
/// Outside the probed area the nearest edge is used ([`MeshSample::outside`]
/// says so). Missing corner points are left out and the remaining weights
/// renormalised; when a whole cell is missing, the nearest probed point is
/// used. `None` when the mesh has no probed point or the position is not
/// finite.
pub fn interpolate(mesh: &BedMesh, geom: &MeshGeometry, x: f32, y: f32) -> Option<MeshSample> {
    if !x.is_finite() || !y.is_finite() || geom.xs.is_empty() || geom.ys.is_empty() {
        return None;
    }
    let [x0, y0, x1, y1] = geom.bounds()?;
    let outside = x < x0 || x > x1 || y < y0 || y > y1;
    let value = |row: usize, col: usize| mesh.get(row, col);
    let (ci, tx) = locate(&geom.xs, x);
    let (ri, ty) = locate(&geom.ys, y);
    let corners: Vec<(usize, usize, f32)> = if geom.xs.len() < 2 || geom.ys.len() < 2 {
        // A single row or column: interpolate along the other axis only.
        let cols: Vec<(usize, f32)> = if geom.xs.len() < 2 {
            vec![(0, 1.0)]
        } else {
            vec![(ci, 1.0 - tx), (ci + 1, tx)]
        };
        let rows: Vec<(usize, f32)> = if geom.ys.len() < 2 {
            vec![(0, 1.0)]
        } else {
            vec![(ri, 1.0 - ty), (ri + 1, ty)]
        };
        rows.iter()
            .flat_map(|&(r, wr)| cols.iter().map(move |&(c, wc)| (r, c, wr * wc)))
            .collect()
    } else {
        vec![
            (ri, ci, (1.0 - tx) * (1.0 - ty)),
            (ri, ci + 1, tx * (1.0 - ty)),
            (ri + 1, ci, (1.0 - tx) * ty),
            (ri + 1, ci + 1, tx * ty),
        ]
    };
    let (mut sum, mut weight) = (0.0f64, 0.0f64);
    for (r, c, w) in corners {
        if let Some(v) = value(r, c) {
            sum += f64::from(v) * f64::from(w);
            weight += f64::from(w);
        }
    }
    let z = if weight > 1e-9 {
        (sum / weight) as f32
    } else {
        nearest(mesh, geom, x, y)?
    };
    Some(MeshSample { z, outside })
}

/// The probed point nearest to (`x`, `y`).
fn nearest(mesh: &BedMesh, geom: &MeshGeometry, x: f32, y: f32) -> Option<f32> {
    let mut best: Option<(f32, f32)> = None;
    for (r, &py) in geom.ys.iter().enumerate() {
        for (c, &px) in geom.xs.iter().enumerate() {
            if let Some(v) = mesh.get(r, c) {
                let d = (px - x).powi(2) + (py - y).powi(2);
                if best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, v));
                }
            }
        }
    }
    best.map(|(_, v)| v)
}

/// A bed leveling screw.
#[derive(Clone, Debug, PartialEq)]
pub struct Screw {
    /// Display name ("Front left").
    pub name: String,
    /// X position (mm).
    pub x: f32,
    /// Y position (mm).
    pub y: f32,
}

impl Screw {
    /// A screw at (`x`, `y`).
    pub fn new(name: impl Into<String>, x: f32, y: f32) -> Screw {
        Screw {
            name: name.into(),
            x,
            y,
        }
    }
}

/// The usual four corner screws, `inset` mm in from each edge of a bed of
/// `bed_size`: front left, front right, back right, back left (clockwise
/// seen from above, starting at the front left). Non-finite or oversized
/// insets fall back to a quarter of the bed.
pub fn default_screws(bed_size: [f32; 2], inset: f32) -> Vec<Screw> {
    let [w, d] = bed_size.map(|s| if s.is_finite() && s > 0.0 { s } else { 220.0 });
    let fit = |size: f32| {
        if inset.is_finite() && inset >= 0.0 && inset * 2.0 < size {
            inset
        } else {
            size / 4.0
        }
    };
    let (ix, iy) = (fit(w), fit(d));
    vec![
        Screw::new("Front left", ix, iy),
        Screw::new("Front right", w - ix, iy),
        Screw::new("Back right", w - ix, d - iy),
        Screw::new("Back left", ix, d - iy),
    ]
}

/// A leveling screw thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thread {
    /// "M3", "M4", "M5".
    pub name: &'static str,
    /// Pitch: mm of height per full turn.
    pub pitch: f32,
}

/// The common bed screw threads (ISO metric coarse).
pub const THREADS: [Thread; 3] = [
    Thread {
        name: "M3",
        pitch: 0.5,
    },
    Thread {
        name: "M4",
        pitch: 0.7,
    },
    Thread {
        name: "M5",
        pitch: 0.8,
    },
];

/// Which way to turn a screw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnDirection {
    /// Clockwise.
    Clockwise,
    /// Counter-clockwise.
    CounterClockwise,
}

impl TurnDirection {
    /// "clockwise" / "counter-clockwise".
    pub fn label(&self) -> &'static str {
        match self {
            TurnDirection::Clockwise => "clockwise",
            TurnDirection::CounterClockwise => "counter-clockwise",
        }
    }
}

/// How to adjust one screw.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrewAdjustment {
    /// Index into the screw list.
    pub index: usize,
    /// Mesh height at the screw (mm).
    pub z: f32,
    /// Height relative to the reference screw (mm; positive = high, i.e.
    /// closer to the nozzle).
    pub delta: f32,
    /// Turns needed (≥ 0).
    pub turns: f32,
    /// Direction, `None` when within [`SCREW_TOLERANCE`] (or the reference).
    pub direction: Option<TurnDirection>,
    /// This is the reference screw.
    pub reference: bool,
    /// The screw lies outside the probed area (its height is the nearest
    /// mesh edge's).
    pub outside: bool,
}

impl ScrewAdjustment {
    /// "Turn clockwise 1/4 turn (0.12 mm high)", "Reference", "OK (0.01 mm
    /// low)".
    pub fn instruction(&self) -> String {
        if self.reference {
            return "Reference".to_owned();
        }
        let side = if self.delta >= 0.0 { "high" } else { "low" };
        let amount = format!("{:.2} mm {side}", self.delta.abs());
        match self.direction {
            None => format!("OK ({amount})"),
            Some(dir) => format!(
                "Turn {} {} ({amount})",
                dir.label(),
                turns_label(self.turns)
            ),
        }
    }

    /// Severity: 0 = fine, 1 = small (under half a turn), 2 = large.
    pub fn severity(&self) -> u8 {
        match self.direction {
            None => 0,
            Some(_) if self.turns < 0.5 => 1,
            Some(_) => 2,
        }
    }
}

/// Screw adjustments that bring every screw to the height of the screw at
/// `reference`.
///
/// `clockwise_lowers` says what a clockwise turn does on this printer
/// (`true` for the usual knobs under the bed: tightening pulls the bed down,
/// away from the nozzle). Screws whose height cannot be interpolated are
/// left out; an empty list when the reference cannot be interpolated or the
/// pitch is not positive.
pub fn screw_adjustments(
    mesh: &BedMesh,
    geom: &MeshGeometry,
    screws: &[Screw],
    reference: usize,
    pitch: f32,
    clockwise_lowers: bool,
) -> Vec<ScrewAdjustment> {
    if !(pitch.is_finite() && pitch > 0.0) {
        return Vec::new();
    }
    let Some(ref_screw) = screws.get(reference) else {
        return Vec::new();
    };
    let Some(ref_sample) = interpolate(mesh, geom, ref_screw.x, ref_screw.y) else {
        return Vec::new();
    };
    screws
        .iter()
        .enumerate()
        .filter_map(|(index, screw)| {
            let sample = interpolate(mesh, geom, screw.x, screw.y)?;
            let delta = sample.z - ref_sample.z;
            let is_ref = index == reference;
            let turns = if is_ref { 0.0 } else { delta.abs() / pitch };
            let direction = if is_ref || delta.abs() < SCREW_TOLERANCE {
                None
            } else {
                // A high corner must go down.
                let lower = delta > 0.0;
                Some(if lower == clockwise_lowers {
                    TurnDirection::Clockwise
                } else {
                    TurnDirection::CounterClockwise
                })
            };
            Some(ScrewAdjustment {
                index,
                z: sample.z,
                delta,
                turns,
                direction,
                reference: is_ref,
                outside: sample.outside,
            })
        })
        .collect()
}

/// A number of turns rounded to eighths: "1/8 turn", "1/4 turn",
/// "1 turn", "1 3/4 turns", "less than 1/8 turn".
pub fn turns_label(turns: f32) -> String {
    if !turns.is_finite() || turns < 0.0 {
        return "—".to_owned();
    }
    let eighths = (turns * 8.0).round().min(8.0 * 1000.0) as u32;
    if eighths == 0 {
        return "less than 1/8 turn".to_owned();
    }
    let whole = eighths / 8;
    let frac = match eighths % 8 {
        0 => "",
        1 => "1/8",
        2 => "1/4",
        3 => "3/8",
        4 => "1/2",
        5 => "5/8",
        6 => "3/4",
        _ => "7/8",
    };
    let amount = match (whole, frac) {
        (0, f) => f.to_owned(),
        (w, "") => w.to_string(),
        (w, f) => format!("{w} {f}"),
    };
    let unit = if eighths > 8 { "turns" } else { "turn" };
    format!("{amount} {unit}")
}

/// A rough verdict on a mesh's flatness from its range (max - min, mm).
pub fn flatness_label(range: f32) -> (&'static str, u8) {
    if !range.is_finite() {
        ("Unknown", 1)
    } else if range < 0.1 {
        ("Excellent", 0)
    } else if range < 0.2 {
        ("Good", 0)
    } else if range < 0.4 {
        ("Fair", 1)
    } else {
        ("Poor", 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tilted plane z = a*x + b*y + c on a 3x3 grid at 10/110/210.
    fn plane(a: f32, b: f32, c: f32) -> (BedMesh, MeshGeometry) {
        let xs = vec![10.0, 110.0, 210.0];
        let ys = vec![10.0, 110.0, 210.0];
        let points = ys
            .iter()
            .map(|y| xs.iter().map(|x| a * x + b * y + c).collect())
            .collect();
        let mut mesh = BedMesh::from_rows(points, "bilinear");
        mesh.x_positions = Some(xs);
        mesh.y_positions = Some(ys);
        let geom = MeshGeometry::of(&mesh, [220.0, 220.0], None).expect("geometry");
        (mesh, geom)
    }

    #[test]
    fn geometry_prefers_reported_positions() {
        let (mesh, geom) = plane(0.0, 0.0, 0.0);
        assert!(geom.reported);
        assert_eq!(geom.xs, vec![10.0, 110.0, 210.0]);
        assert_eq!(geom.bounds(), Some([10.0, 10.0, 210.0, 210.0]));
        // Positions of the wrong length are ignored.
        let mut bad = mesh.clone();
        bad.x_positions = Some(vec![0.0, 1.0]);
        let g = MeshGeometry::of(&bad, [220.0, 220.0], None).expect("assumed");
        assert!(!g.reported);
    }

    #[test]
    fn geometry_assumes_marlin_probe_area() {
        let mesh = BedMesh::from_rows(vec![vec![0.0; 5]; 5], "bilinear");
        let g = MeshGeometry::of(&mesh, [220.0, 220.0], Some([10.0, 10.0])).expect("g");
        assert_eq!(g.xs, vec![10.0, 60.0, 110.0, 160.0, 210.0]);
        // A probe left of the nozzle limits the right edge.
        let g = MeshGeometry::of(&mesh, [220.0, 220.0], Some([-40.0, 0.0])).expect("g");
        assert_eq!(g.xs.first(), Some(&10.0));
        assert_eq!(g.xs.last(), Some(&180.0));
        assert!(MeshGeometry::of(&BedMesh::default(), [220.0, 220.0], None).is_none());
        assert!(MeshGeometry::of(&mesh, [f32::NAN, 220.0], None).is_none());
        // An absurd offset falls back to the margins.
        let g = MeshGeometry::of(&mesh, [220.0, 220.0], Some([500.0, 0.0])).expect("g");
        assert!(g.xs.first() < g.xs.last());
    }

    #[test]
    fn bilinear_reproduces_planes() {
        let (mesh, geom) = plane(0.001, -0.002, 0.05);
        for (x, y) in [(10.0, 10.0), (60.0, 160.0), (123.4, 77.7), (210.0, 210.0)] {
            let s = interpolate(&mesh, &geom, x, y).expect("sample");
            let want = 0.001 * x - 0.002 * y + 0.05;
            assert!((s.z - want).abs() < 1e-5, "({x},{y}): {} vs {want}", s.z);
            assert!(!s.outside);
        }
    }

    #[test]
    fn outside_clamps_to_the_edge() {
        let (mesh, geom) = plane(0.001, 0.0, 0.0);
        let s = interpolate(&mesh, &geom, 0.0, 300.0).expect("sample");
        assert!(s.outside);
        assert!((s.z - 0.01).abs() < 1e-6);
        assert!(interpolate(&mesh, &geom, f32::NAN, 0.0).is_none());
    }

    #[test]
    fn missing_points_are_skipped() {
        let (mut mesh, geom) = plane(0.0, 0.0, 0.1);
        mesh.points[0][0] = f32::NAN;
        let s = interpolate(&mesh, &geom, 20.0, 20.0).expect("sample");
        assert!((s.z - 0.1).abs() < 1e-6);
        // A whole missing cell uses the nearest probed point.
        for r in 0..2 {
            for c in 0..2 {
                mesh.points[r][c] = f32::NAN;
            }
        }
        mesh.points[2][0] = 0.3;
        let s = interpolate(&mesh, &geom, 20.0, 20.0).expect("sample");
        assert!((s.z - 0.1).abs() < 1e-6, "{s:?}");
        let empty = BedMesh::from_rows(vec![vec![f32::NAN; 3]; 3], "x");
        assert!(interpolate(&empty, &geom, 20.0, 20.0).is_none());
    }

    #[test]
    fn single_row_and_column_meshes() {
        let mut mesh = BedMesh::from_rows(vec![vec![0.0, 0.2]], "x");
        mesh.x_positions = Some(vec![0.0, 100.0]);
        mesh.y_positions = Some(vec![50.0]);
        let g = MeshGeometry::of(&mesh, [100.0, 100.0], None).expect("g");
        let s = interpolate(&mesh, &g, 50.0, 50.0).expect("s");
        assert!((s.z - 0.1).abs() < 1e-6);
    }

    #[test]
    fn default_screws_are_inset_corners() {
        let s = default_screws([220.0, 220.0], DEFAULT_SCREW_INSET);
        assert_eq!(s.len(), 4);
        assert_eq!((s[0].x, s[0].y), (30.0, 30.0));
        assert_eq!((s[2].x, s[2].y), (190.0, 190.0));
        assert_eq!(s[1].name, "Front right");
        // A silly inset falls back to a quarter of the bed.
        let s = default_screws([100.0, 200.0], 80.0);
        assert_eq!((s[0].x, s[0].y), (25.0, 80.0));
        let s = default_screws([f32::NAN, 0.0], 30.0);
        assert_eq!(s[2].x, 190.0);
    }

    #[test]
    fn screw_turns_relative_to_reference() {
        // Right side 0.12 mm higher than the left: z = 0.12 * (x - 30) / 160.
        let (mesh, geom) = plane(0.12 / 160.0, 0.0, -0.12 * 30.0 / 160.0);
        let screws = default_screws([220.0, 220.0], 30.0);
        let adj = screw_adjustments(&mesh, &geom, &screws, 0, 0.5, true);
        assert_eq!(adj.len(), 4);
        assert!(adj[0].reference);
        assert_eq!(adj[0].instruction(), "Reference");
        assert_eq!(adj[3].direction, None);
        assert!(adj[3].instruction().starts_with("OK"));
        let fr = &adj[1];
        assert!((fr.delta - 0.12).abs() < 1e-5);
        assert!((fr.turns - 0.24).abs() < 1e-4);
        assert_eq!(fr.direction, Some(TurnDirection::Clockwise));
        assert_eq!(fr.instruction(), "Turn clockwise 1/4 turn (0.12 mm high)");
        assert_eq!(fr.severity(), 1);
        // Referenced to a high screw, the low ones go the other way.
        let adj = screw_adjustments(&mesh, &geom, &screws, 1, 0.5, true);
        assert_eq!(adj[0].direction, Some(TurnDirection::CounterClockwise));
        assert_eq!(
            adj[0].instruction(),
            "Turn counter-clockwise 1/4 turn (0.12 mm low)"
        );
        // Printers where clockwise raises the bed flip the direction.
        let adj = screw_adjustments(&mesh, &geom, &screws, 0, 0.5, false);
        assert_eq!(adj[1].direction, Some(TurnDirection::CounterClockwise));
        // Coarser threads need fewer turns.
        let adj = screw_adjustments(&mesh, &geom, &screws, 0, 0.8, true);
        assert!((adj[1].turns - 0.15).abs() < 1e-4);
    }

    #[test]
    fn screw_adjustments_reject_bad_input() {
        let (mesh, geom) = plane(0.0, 0.0, 0.0);
        let screws = default_screws([220.0, 220.0], 30.0);
        assert!(screw_adjustments(&mesh, &geom, &screws, 9, 0.5, true).is_empty());
        assert!(screw_adjustments(&mesh, &geom, &screws, 0, 0.0, true).is_empty());
        assert!(screw_adjustments(&mesh, &geom, &screws, 0, f32::NAN, true).is_empty());
        assert!(screw_adjustments(&mesh, &geom, &[], 0, 0.5, true).is_empty());
    }

    #[test]
    fn turn_labels_round_to_eighths() {
        assert_eq!(turns_label(0.03), "less than 1/8 turn");
        assert_eq!(turns_label(0.12), "1/8 turn");
        assert_eq!(turns_label(0.24), "1/4 turn");
        assert_eq!(turns_label(0.5), "1/2 turn");
        assert_eq!(turns_label(1.0), "1 turn");
        assert_eq!(turns_label(1.74), "1 3/4 turns");
        assert_eq!(turns_label(2.0), "2 turns");
        assert_eq!(turns_label(0.95), "1 turn");
        assert_eq!(turns_label(f32::NAN), "—");
        assert!(turns_label(1e9).ends_with("turns"));
    }

    #[test]
    fn flatness_verdicts() {
        assert_eq!(flatness_label(0.05).0, "Excellent");
        assert_eq!(flatness_label(0.15).0, "Good");
        assert_eq!(flatness_label(0.3).0, "Fair");
        assert_eq!(flatness_label(0.5), ("Poor", 2));
        assert_eq!(flatness_label(f32::NAN).0, "Unknown");
    }
}
