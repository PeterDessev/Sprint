//! Helpers for the NaN sentinels used by the data types: value-less flags
//! in [`SettingCommand`](crate::SettingCommand), missing mesh points in
//! [`BedMesh`](crate::BedMesh) and placeholder hotends in
//! [`TempReport`](crate::TempReport).
//!
//! A sentinel NaN means "no value here", so two of them compare equal
//! (IEEE equality would make such values unequal even to themselves).

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
