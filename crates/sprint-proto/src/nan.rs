//! Helpers for the NaN sentinels used by the data types: value-less flags
//! in [`SettingCommand`](crate::SettingCommand), missing mesh points in
//! [`BedMesh`](crate::BedMesh) and placeholder hotends in
//! [`TempReport`](crate::TempReport).
//!
//! A sentinel NaN means "no value here", so two of them compare equal
//! (IEEE equality would make such values unequal even to themselves).
//!
//! Formats without NaN (JSON) serialise it as `null`; the `de_*` functions
//! (used with `#[serde(deserialize_with)]`) read `null` back as NaN, while
//! formats that do support NaN (TOML, RON) keep working unchanged.

use serde::{Deserialize, Deserializer};

/// Deserialise `SettingCommand::params`, reading `null` values as NaN flags.
pub(crate) fn de_params<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(char, f64)>, D::Error> {
    let raw: Vec<(char, Option<f64>)> = Vec::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|(letter, v)| (letter, v.unwrap_or(f64::NAN)))
        .collect())
}

/// Deserialise `BedMesh::points`, reading `null` points as NaN (missing).
pub(crate) fn de_grid<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<f32>>, D::Error> {
    let raw: Vec<Vec<Option<f32>>> = Vec::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|row| row.into_iter().map(|v| v.unwrap_or(f32::NAN)).collect())
        .collect())
}

/// Deserialise an `f32` sentinel field, reading `null` as NaN.
pub(crate) fn de_f32<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(Option::<f32>::deserialize(d)?.unwrap_or(f32::NAN))
}

/// `a == b`, with NaN equal to NaN.
pub(crate) fn same_f32(a: f32, b: f32) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// `a == b`, with NaN equal to NaN.
pub(crate) fn same_f64(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// Element-wise [`same_f32`] over two slices.
pub(crate) fn same_f32_slice(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| same_f32(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nan_is_equal_to_nan_only() {
        assert!(same_f32(f32::NAN, f32::NAN));
        assert!(same_f32(1.5, 1.5));
        assert!(same_f32(0.0, -0.0));
        assert!(!same_f32(f32::NAN, 0.0));
        assert!(!same_f32(1.0, 2.0));
        assert!(same_f64(f64::NAN, f64::NAN));
        assert!(!same_f64(f64::NAN, 1.0));
        assert!(same_f32_slice(&[1.0, f32::NAN], &[1.0, f32::NAN]));
        assert!(!same_f32_slice(&[1.0, f32::NAN], &[1.0]));
        assert!(!same_f32_slice(&[1.0, f32::NAN], &[1.0, 2.0]));
    }
}
