//! Bilinear bed leveling: a plausible warped bed and Marlin's grid report.

use crate::format::signed3;
use crate::rng::Rng;

/// Marlin `PROBING_MARGIN` in mm.
const PROBING_MARGIN: f64 = 10.0;

/// A probed leveling grid. `z[row][col]`, row 0 = front (min Y).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Mesh {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub z: Vec<Vec<f64>>,
}

/// The simulated bed surface: tilt, bowl, twist and some waviness, fixed
/// per seed so repeated G29 runs agree (apart from probe noise).
#[derive(Clone, Copy, Debug)]
pub(crate) struct BedShape {
    tilt_x: f64,
    tilt_y: f64,
    bowl: f64,
    twist: f64,
    wave: f64,
    phase: f64,
}

impl BedShape {
    /// The bed for a seed.
    pub(crate) fn new(seed: u64) -> BedShape {
        let mut r = Rng::new(seed, 0x6d65_7368);
        BedShape {
            tilt_x: r.uniform(-0.12, 0.12),
            tilt_y: r.uniform(-0.12, 0.12),
            bowl: r.uniform(-0.15, 0.15),
            twist: r.uniform(-0.06, 0.06),
            wave: r.uniform(0.0, 0.03),
            phase: r.uniform(0.0, std::f64::consts::TAU),
        }
    }

    /// Height deviation at normalised coordinates `u`, `v` in -1..=1.
    fn height(&self, u: f64, v: f64) -> f64 {
        self.tilt_x * u
            + self.tilt_y * v
            + self.bowl * (u * u + v * v - 0.66)
            + self.twist * u * v
            + self.wave * (2.5 * u + self.phase).sin() * (2.0 * v).cos()
    }
}

/// Evenly spaced probe positions between `lo` and `hi`.
fn positions(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    let n = n.max(2);
    (0..n)
        .map(|i| lo + (hi - lo) * i as f64 / (n - 1) as f64)
        .collect()
}

/// Probe-reachable range along one axis (Marlin's probe_min/probe_max).
fn probe_range(size: f64, offset: f64) -> (f64, f64) {
    let lo = PROBING_MARGIN.max(offset);
    let hi = (size - PROBING_MARGIN).min(size + offset);
    if hi - lo > 1.0 {
        (lo, hi)
    } else {
        (
            PROBING_MARGIN.min(size / 4.0),
            (size - PROBING_MARGIN).max(size * 0.75),
        )
    }
}

impl Mesh {
    /// Probe a `cols` x `rows` grid on a bed of `size` with the given probe
    /// XY offset. `run` varies the probe noise between G29 runs.
    pub(crate) fn probe(
        cols: usize,
        rows: usize,
        size: [f64; 2],
        probe_offset: [f64; 2],
        shape: &BedShape,
        seed: u64,
        run: u64,
    ) -> Mesh {
        let (x0, x1) = probe_range(size[0], probe_offset[0]);
        let (y0, y1) = probe_range(size[1], probe_offset[1]);
        let x = positions(x0, x1, cols);
        let y = positions(y0, y1, rows);
        let mut noise = Rng::new(seed, 0x7072_6f62 ^ run);
        let z = y
            .iter()
            .map(|&py| {
                x.iter()
                    .map(|&px| {
                        let u = (px / size[0].max(1.0)) * 2.0 - 1.0;
                        let v = (py / size[1].max(1.0)) * 2.0 - 1.0;
                        let h = shape.height(u, v) + noise.uniform(-0.003, 0.003);
                        (h * 1000.0).round() / 1000.0
                    })
                    .collect()
            })
            .collect();
        Mesh { x, y, z }
    }

    /// Number of columns.
    pub(crate) fn cols(&self) -> usize {
        self.x.len()
    }

    /// Number of rows.
    pub(crate) fn rows(&self) -> usize {
        self.y.len()
    }

    /// Marlin's "Bilinear Leveling Grid:" report.
    pub(crate) fn report(&self) -> Vec<String> {
        let mut out = vec!["Bilinear Leveling Grid:".to_string()];
        let mut header = String::new();
        for c in 0..self.cols() {
            let pad = if c < 10 { 6 } else { 5 };
            header.push_str(&" ".repeat(pad));
            header.push_str(&c.to_string());
        }
        out.push(header);
        for (r, row) in self.z.iter().enumerate() {
            let mut line = if r < 10 {
                format!(" {r}")
            } else {
                r.to_string()
            };
            for v in row {
                line.push(' ');
                line.push_str(&signed3(*v));
            }
            out.push(line);
        }
        out
    }

    /// Bilinear interpolation of the correction at a bed position
    /// (clamped to the grid; extrapolation is flat).
    pub(crate) fn z_at(&self, px: f64, py: f64) -> f64 {
        fn locate(axis: &[f64], p: f64) -> (usize, f64) {
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
            for i in 0..n - 1 {
                if p <= axis[i + 1] {
                    let span = axis[i + 1] - axis[i];
                    let t = if span > 0.0 {
                        (p - axis[i]) / span
                    } else {
                        0.0
                    };
                    return (i, t);
                }
            }
            (n - 2, 1.0)
        }
        if self.rows() < 2 || self.cols() < 2 || !px.is_finite() || !py.is_finite() {
            return 0.0;
        }
        let (c, tx) = locate(&self.x, px);
        let (r, ty) = locate(&self.y, py);
        let at = |r: usize, c: usize| {
            self.z
                .get(r)
                .and_then(|row| row.get(c))
                .copied()
                .unwrap_or(0.0)
        };
        let a = at(r, c) * (1.0 - tx) + at(r, c + 1) * tx;
        let b = at(r + 1, c) * (1.0 - tx) + at(r + 1, c + 1) * tx;
        a * (1.0 - ty) + b * ty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(cols: usize, rows: usize) -> Mesh {
        Mesh::probe(
            cols,
            rows,
            [220.0, 220.0],
            [10.0, 10.0],
            &BedShape::new(42),
            42,
            0,
        )
    }

    #[test]
    fn grid_report_format() {
        let m = mesh(5, 3);
        let r = m.report();
        assert_eq!(r[0], "Bilinear Leveling Grid:");
        assert_eq!(r[1], "      0      1      2      3      4");
        assert_eq!(r.len(), 2 + 3);
        for line in &r[2..] {
            let parts: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(parts.len(), 6, "{line}");
            for v in &parts[1..] {
                assert!(v.starts_with('+') || v.starts_with('-'), "{v}");
                assert_eq!(v.split('.').nth(1).map(str::len), Some(3));
            }
        }
        assert!(r[2].starts_with(" 0 "));
    }

    #[test]
    fn bed_is_plausible_and_deterministic() {
        let a = mesh(7, 7);
        let b = mesh(7, 7);
        let flat: Vec<f64> = a.z.iter().flatten().copied().collect();
        let min = flat.iter().copied().fold(f64::INFINITY, f64::min);
        let max = flat.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(max - min > 0.02 && max - min < 0.8, "{min} {max}");
        for (ra, rb) in a.z.iter().zip(&b.z) {
            for (x, y) in ra.iter().zip(rb) {
                assert!((x - y).abs() < 1e-9);
            }
        }
        assert_eq!(a.x.first().copied(), Some(10.0));
        assert_eq!(a.x.last().copied(), Some(210.0));
    }

    #[test]
    fn interpolation_hits_grid_points() {
        let m = mesh(4, 4);
        for (r, &y) in m.y.iter().enumerate() {
            for (c, &x) in m.x.iter().enumerate() {
                assert!((m.z_at(x, y) - m.z[r][c]).abs() < 1e-9);
            }
        }
        assert!(m.z_at(-100.0, 500.0).is_finite());
        assert_eq!(m.z_at(f64::NAN, 0.0), 0.0);
    }

    #[test]
    fn wide_grid_header_spacing() {
        let m = mesh(12, 2);
        let r = m.report();
        assert!(r[1].ends_with("      9     10     11"), "{}", r[1]);
    }
}
