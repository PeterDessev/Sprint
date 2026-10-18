//! Bed leveling mesh model.
//!
//! Parsing of the firmware's multi-line mesh reports lives in the reply
//! parser; this module holds the normalised grid and its statistics.

use serde::{Deserialize, Serialize};

use crate::nan::same_f32_slice;

/// Bed leveling grid. `points[row][col]`, row 0 = front (min Y), col 0 =
/// left (min X). Missing points are NaN (and compare equal to each other).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BedMesh {
    /// Z offsets in mm, `points[row][col]`.
    pub points: Vec<Vec<f32>>,
    /// X coordinate of each column, when the firmware reported it.
    pub x_positions: Option<Vec<f32>>,
    /// Y coordinate of each row, when the firmware reported it.
    pub y_positions: Option<Vec<f32>>,
    /// Source format: `"bilinear"`, `"mbl"`, `"ubl"`, `"klipper_probed"` or
    /// `"klipper_mesh"` (or anything else a caller chooses).
    pub kind: String,
}

impl BedMesh {
    /// Build a mesh from rows, padding short rows with NaN so every row has
    /// the same number of columns.
    pub fn from_rows(mut points: Vec<Vec<f32>>, kind: impl Into<String>) -> BedMesh {
        let cols = points.iter().map(Vec::len).max().unwrap_or(0);
        for row in &mut points {
            row.resize(cols, f32::NAN);
        }
        BedMesh {
            points,
            x_positions: None,
            y_positions: None,
            kind: kind.into(),
        }
    }

    /// Fill `x_positions` / `y_positions` with evenly spaced coordinates
    /// spanning the given probe area (mm), one per column / row.
    pub fn set_bounds(&mut self, min_x: f32, max_x: f32, min_y: f32, max_y: f32) {
        self.x_positions = Some(linspace(min_x, max_x, self.cols()));
        self.y_positions = Some(linspace(min_y, max_y, self.rows()));
    }

    /// Number of rows (Y direction).
    pub fn rows(&self) -> usize {
        self.points.len()
    }

    /// Number of columns (X direction): the longest row.
    pub fn cols(&self) -> usize {
        self.points.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Value at `(row, col)`; `None` when out of range or missing (NaN).
    pub fn get(&self, row: usize, col: usize) -> Option<f32> {
        self.points
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .filter(|v| v.is_finite())
    }

    /// Iterator over the finite (probed) values.
    pub fn values(&self) -> impl Iterator<Item = f32> + '_ {
        self.points
            .iter()
            .flat_map(|row| row.iter().copied())
            .filter(|v| v.is_finite())
    }

    /// Number of finite (probed) values.
    pub fn count(&self) -> usize {
        self.values().count()
    }

    /// Lowest probed value.
    pub fn min(&self) -> Option<f32> {
        self.values().reduce(f32::min)
    }

    /// Highest probed value.
    pub fn max(&self) -> Option<f32> {
        self.values().reduce(f32::max)
    }

    /// `max - min` over probed values.
    pub fn range(&self) -> Option<f32> {
        Some(self.max()? - self.min()?)
    }

    /// Mean of the probed values.
    pub fn mean(&self) -> Option<f32> {
        let (sum, n) = self
            .values()
            .fold((0.0f64, 0usize), |(s, n), v| (s + f64::from(v), n + 1));
        (n > 0).then(|| (sum / n as f64) as f32)
    }

    /// Population standard deviation of the probed values.
    pub fn std_dev(&self) -> Option<f32> {
        let mean = f64::from(self.mean()?);
        let (sum_sq, n) = self.values().fold((0.0f64, 0usize), |(s, n), v| {
            let d = f64::from(v) - mean;
            (s + d * d, n + 1)
        });
        (n > 0).then(|| (sum_sq / n as f64).sqrt() as f32)
    }
}

impl PartialEq for BedMesh {
    /// Field-wise equality where missing points (NaN) equal each other, so
    /// a mesh with holes equals its own clone.
    fn eq(&self, other: &Self) -> bool {
        let positions_eq = |a: &Option<Vec<f32>>, b: &Option<Vec<f32>>| match (a, b) {
            (Some(a), Some(b)) => same_f32_slice(a, b),
            (None, None) => true,
            _ => false,
        };
        self.kind == other.kind
            && self.points.len() == other.points.len()
            && self
                .points
                .iter()
                .zip(&other.points)
                .all(|(a, b)| same_f32_slice(a, b))
            && positions_eq(&self.x_positions, &other.x_positions)
            && positions_eq(&self.y_positions, &other.y_positions)
    }
}

/// `n` evenly spaced values from `start` to `end` inclusive.
pub(crate) fn linspace(start: f32, end: f32, n: usize) -> Vec<f32> {
    match n {
        0 => Vec::new(),
        1 => vec![start],
        _ => (0..n)
            .map(|i| start + (end - start) * i as f32 / (n - 1) as f32)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn statistics() {
        let mesh = BedMesh::from_rows(
            vec![vec![0.1, -0.1, 0.0], vec![0.2, f32::NAN, -0.2]],
            "bilinear",
        );
        assert_eq!(mesh.rows(), 2);
        assert_eq!(mesh.cols(), 3);
        assert_eq!(mesh.count(), 5);
        assert!(approx(mesh.min().unwrap(), -0.2));
        assert!(approx(mesh.max().unwrap(), 0.2));
        assert!(approx(mesh.range().unwrap(), 0.4));
        assert!(approx(mesh.mean().unwrap(), 0.0));
        // values: .1 -.1 0 .2 -.2 -> variance = (.01+.01+0+.04+.04)/5 = .02
        assert!(approx(mesh.std_dev().unwrap(), 0.02f32.sqrt()));
        assert_eq!(mesh.get(1, 1), None);
        assert_eq!(mesh.get(0, 0), Some(0.1));
        assert_eq!(mesh.get(5, 0), None);
    }

    #[test]
    fn empty_and_all_missing() {
        let empty = BedMesh::default();
        assert_eq!(empty.rows(), 0);
        assert_eq!(empty.cols(), 0);
        assert_eq!(empty.min(), None);
        assert_eq!(empty.range(), None);
        assert_eq!(empty.mean(), None);
        assert_eq!(empty.std_dev(), None);
        let nan = BedMesh::from_rows(vec![vec![f32::NAN; 3]; 2], "ubl");
        assert_eq!(nan.count(), 0);
        assert_eq!(nan.max(), None);
    }

    #[test]
    fn ragged_rows_are_padded() {
        let mesh = BedMesh::from_rows(vec![vec![1.0], vec![1.0, 2.0, 3.0]], "x");
        assert_eq!(mesh.points[0].len(), 3);
        assert!(mesh.points[0][2].is_nan());
    }

    #[test]
    fn bounds_fill_positions() {
        let mut mesh = BedMesh::from_rows(vec![vec![0.0; 3]; 2], "ubl");
        mesh.set_bounds(10.0, 210.0, 20.0, 200.0);
        assert_eq!(mesh.x_positions, Some(vec![10.0, 110.0, 210.0]));
        assert_eq!(mesh.y_positions, Some(vec![20.0, 200.0]));
    }

    #[test]
    fn missing_points_compare_equal() {
        // Regression: derived equality made a mesh with NaN holes unequal
        // to its own clone.
        let mut mesh = BedMesh::from_rows(vec![vec![0.1, f32::NAN], vec![0.2, 0.3]], "ubl");
        mesh.set_bounds(0.0, 200.0, 0.0, 200.0);
        assert_eq!(mesh, mesh.clone());
        let mut other = mesh.clone();
        other.points[0][1] = 0.0;
        assert_ne!(mesh, other);
        let mut other = mesh.clone();
        other.kind = "bilinear".into();
        assert_ne!(mesh, other);
        let mut other = mesh.clone();
        other.x_positions = None;
        assert_ne!(mesh, other);
        let mut other = mesh.clone();
        other.points.pop();
        assert_ne!(mesh, other);
    }

    #[test]
    fn linspace_endpoints() {
        assert_eq!(linspace(0.0, 10.0, 3), vec![0.0, 5.0, 10.0]);
        assert_eq!(linspace(1.0, 2.0, 1), vec![1.0]);
        assert!(linspace(0.0, 1.0, 0).is_empty());
    }
}
