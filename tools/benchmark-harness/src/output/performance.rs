//! Aggregation of latency and memory observations into a [`DatasetPerformanceReport`].

use crate::stats::{percentile_r7, sanitize_f64};
use crate::types::{DatasetPerformanceReport, ScrapeBenchmarkResult};

/// Aggregate duration and memory observations into a [`DatasetPerformanceReport`].
pub(crate) fn build_performance_report(results: &[ScrapeBenchmarkResult]) -> DatasetPerformanceReport {
    let mut durations: Vec<f64> = results
        .iter()
        .filter(|r| r.success && r.duration_ms > 0.0)
        .map(|r| r.duration_ms)
        .collect();

    let latency_p50_ms = percentile_r7(&mut durations, 0.50).unwrap_or(0.0);
    let latency_p95_ms = percentile_r7(&mut durations, 0.95).unwrap_or(0.0);
    let latency_p99_ms = percentile_r7(&mut durations, 0.99).unwrap_or(0.0);

    let peak_memory_bytes = results.iter().map(|r| r.metrics.peak_memory_bytes).max().unwrap_or(0);

    let total_duration_secs: f64 = results
        .iter()
        .filter(|r| r.success && r.duration_ms > 0.0)
        .map(|r| r.duration_ms / 1_000.0)
        .sum();

    let successful_count = results.iter().filter(|r| r.success).count();
    let throughput_pages_per_sec = if total_duration_secs > 0.0 {
        sanitize_f64(successful_count as f64 / total_duration_secs)
    } else {
        0.0
    };

    DatasetPerformanceReport {
        latency_p50_ms: sanitize_f64(latency_p50_ms),
        latency_p95_ms: sanitize_f64(latency_p95_ms),
        latency_p99_ms: sanitize_f64(latency_p99_ms),
        throughput_pages_per_sec,
        peak_memory_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::test_support::make_result;

    #[test]
    fn test_performance_report_empty_results() {
        let report = build_performance_report(&[]);
        assert_eq!(report.latency_p50_ms, 0.0);
        assert_eq!(report.throughput_pages_per_sec, 0.0);
        assert_eq!(report.peak_memory_bytes, 0);
    }

    #[test]
    fn test_performance_report_latency_percentiles() {
        let durations = [100.0, 200.0, 300.0, 400.0, 500.0];
        let results: Vec<_> = durations.iter().map(|&d| make_result(true, d, None)).collect();
        let report = build_performance_report(&results);

        assert!((report.latency_p50_ms - 300.0).abs() < 1e-9);
        assert!(report.latency_p95_ms >= 400.0);
        assert!(report.latency_p99_ms >= 450.0);
    }

    #[test]
    fn test_performance_report_peak_memory() {
        let mut results = vec![make_result(true, 100.0, None), make_result(true, 200.0, None)];
        results[0].metrics.peak_memory_bytes = 50 * 1024 * 1024;
        results[1].metrics.peak_memory_bytes = 200 * 1024 * 1024;
        let report = build_performance_report(&results);
        assert_eq!(report.peak_memory_bytes, 200 * 1024 * 1024);
    }
}
