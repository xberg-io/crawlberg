//! Statistical helper functions for duration analysis.
//!
//! All functions operate on slices of `f64` values representing durations in
//! milliseconds (though the implementations are unit-agnostic).

/// Compute a percentile using the R-7 linear interpolation method (the same
/// algorithm used by NumPy, R, and most statistical packages).
///
/// Sorts `samples` in place as a side effect.
///
/// Returns `None` if `samples` is empty or `percentile` is outside `[0.0, 1.0]`.
///
/// # Examples
///
/// ```
/// use benchmark_harness::stats::percentile_r7;
///
/// let mut data = vec![1.0_f64, 2.0, 3.0, 4.0, 5.0];
/// assert_eq!(percentile_r7(&mut data, 0.5), Some(3.0));
/// assert_eq!(percentile_r7(&mut data, 0.0), Some(1.0));
/// assert_eq!(percentile_r7(&mut data, 1.0), Some(5.0));
/// ```
pub fn percentile_r7(samples: &mut [f64], percentile: f64) -> Option<f64> {
    if samples.is_empty() || !(0.0..=1.0).contains(&percentile) {
        return None;
    }

    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = samples.len();
    if n == 1 {
        return Some(samples[0]);
    }

    let h = (n - 1) as f64 * percentile;
    let lo = h.floor() as usize;
    let hi = h.ceil() as usize;

    if lo == hi {
        Some(samples[lo])
    } else {
        let frac = h - lo as f64;
        Some(samples[lo] + frac * (samples[hi] - samples[lo]))
    }
}

/// Replace NaN and infinite values with `0.0` so they do not propagate into
/// reported statistics.
///
/// # Examples
///
/// ```
/// use benchmark_harness::stats::sanitize_f64;
///
/// assert_eq!(sanitize_f64(f64::NAN), 0.0);
/// assert_eq!(sanitize_f64(f64::INFINITY), 0.0);
/// assert_eq!(sanitize_f64(42.0), 42.0);
/// ```
pub fn sanitize_f64(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

/// Compute the sample variance of `samples` (divides by `n − 1`).
///
/// Returns `0.0` when fewer than two samples are provided.
///
/// # Examples
///
/// ```
/// use benchmark_harness::stats::calculate_variance;
///
/// let data = vec![2.0_f64, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
/// let variance = calculate_variance(&data);
/// // Expected sample variance ≈ 4.571
/// assert!((variance - 4.571_428).abs() < 1e-3);
/// ```
pub fn calculate_variance(samples: &[f64]) -> f64 {
    let n = samples.len();
    if n < 2 {
        return 0.0;
    }

    let mean = samples.iter().sum::<f64>() / n as f64;
    let sum_sq = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>();
    sum_sq / (n - 1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_r7_empty_returns_none() {
        let mut data: Vec<f64> = vec![];
        assert_eq!(percentile_r7(&mut data, 0.5), None);
    }

    #[test]
    fn percentile_r7_out_of_range_returns_none() {
        let mut data = vec![1.0_f64, 2.0, 3.0];
        assert_eq!(percentile_r7(&mut data, -0.1), None);
        assert_eq!(percentile_r7(&mut data, 1.1), None);
    }

    #[test]
    fn percentile_r7_single_element() {
        let mut data = vec![42.0_f64];
        assert_eq!(percentile_r7(&mut data, 0.5), Some(42.0));
    }

    #[test]
    fn percentile_r7_boundaries() {
        let mut data = vec![10.0_f64, 20.0, 30.0, 40.0, 50.0];
        assert_eq!(percentile_r7(&mut data, 0.0), Some(10.0));
        assert_eq!(percentile_r7(&mut data, 1.0), Some(50.0));
    }

    #[test]
    fn percentile_r7_median_odd() {
        let mut data = vec![1.0_f64, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile_r7(&mut data, 0.5), Some(3.0));
    }

    #[test]
    fn percentile_r7_median_even() {
        let mut data = vec![1.0_f64, 2.0, 3.0, 4.0];
        assert_eq!(percentile_r7(&mut data, 0.5), Some(2.5));
    }

    #[test]
    fn percentile_r7_p95() {
        let mut data: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        let p95 = percentile_r7(&mut data, 0.95).unwrap();
        assert!((p95 - 95.05).abs() < 1e-9);
    }

    #[test]
    fn sanitize_f64_finite_passes_through() {
        assert_eq!(sanitize_f64(3.5), 3.5);
        assert_eq!(sanitize_f64(0.0), 0.0);
        assert_eq!(sanitize_f64(-1.0), -1.0);
    }

    #[test]
    fn sanitize_f64_non_finite_becomes_zero() {
        assert_eq!(sanitize_f64(f64::NAN), 0.0);
        assert_eq!(sanitize_f64(f64::INFINITY), 0.0);
        assert_eq!(sanitize_f64(f64::NEG_INFINITY), 0.0);
    }

    #[test]
    fn calculate_variance_empty_returns_zero() {
        assert_eq!(calculate_variance(&[]), 0.0);
    }

    #[test]
    fn calculate_variance_single_returns_zero() {
        assert_eq!(calculate_variance(&[5.0]), 0.0);
    }

    #[test]
    fn calculate_variance_known_value() {
        let data = vec![2.0_f64, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let variance = calculate_variance(&data);
        assert!((variance - 4.571_428).abs() < 1e-3);
    }

    #[test]
    fn calculate_variance_identical_values() {
        let data = vec![7.0_f64; 10];
        assert_eq!(calculate_variance(&data), 0.0);
    }
}
