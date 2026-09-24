//! Comparison of two benchmark runs (baseline vs candidate) and its report.

use ahash::AHashMap;

use crate::stats::{percentile_r7, sanitize_f64};
use crate::types::{ComparisonReport, FixtureComparison, ScrapeBenchmarkResult};

use super::performance::build_performance_report;

/// Per-fixture comparisons, plus the latency and quality-score samples collected while
/// building them.
type FixtureComparisonAccumulation = (Vec<FixtureComparison>, Vec<f64>, Vec<f64>, Vec<f64>);

fn build_fixture_comparisons(
    baseline_results: &[ScrapeBenchmarkResult],
    candidate_results: &[ScrapeBenchmarkResult],
) -> FixtureComparisonAccumulation {
    let baseline_map: AHashMap<&str, &ScrapeBenchmarkResult> =
        baseline_results.iter().map(|r| (r.fixture_id.as_str(), r)).collect();

    let mut fixture_comparisons: Vec<FixtureComparison> = Vec::new();
    let mut latency_deltas: Vec<f64> = Vec::new();
    let mut baseline_quality_scores: Vec<f64> = Vec::new();
    let mut candidate_quality_scores: Vec<f64> = Vec::new();

    for candidate in candidate_results {
        let Some(baseline) = baseline_map.get(candidate.fixture_id.as_str()) else {
            continue;
        };

        let latency_delta_pct = if baseline.duration_ms > 0.0 {
            sanitize_f64((candidate.duration_ms - baseline.duration_ms) / baseline.duration_ms * 100.0)
        } else {
            0.0
        };

        latency_deltas.push(latency_delta_pct);

        let baseline_quality = baseline.quality.as_ref().map(|q| q.quality_score);
        let candidate_quality = candidate.quality.as_ref().map(|q| q.quality_score);

        if let Some(bq) = baseline_quality {
            baseline_quality_scores.push(bq);
        }
        if let Some(cq) = candidate_quality {
            candidate_quality_scores.push(cq);
        }

        let quality_delta = match (baseline_quality, candidate_quality) {
            (Some(bq), Some(cq)) => Some(sanitize_f64(cq - bq)),
            _ => None,
        };

        fixture_comparisons.push(FixtureComparison {
            fixture_id: candidate.fixture_id.clone(),
            url: candidate.url.clone(),
            baseline_duration_ms: baseline.duration_ms,
            candidate_duration_ms: candidate.duration_ms,
            latency_delta_pct,
            baseline_quality,
            candidate_quality,
            quality_delta,
        });
    }

    (
        fixture_comparisons,
        latency_deltas,
        baseline_quality_scores,
        candidate_quality_scores,
    )
}

fn mean_quality_delta(baseline_scores: &[f64], candidate_scores: &[f64]) -> Option<f64> {
    if baseline_scores.is_empty() || candidate_scores.is_empty() {
        return None;
    }
    let mean_baseline: f64 = baseline_scores.iter().sum::<f64>() / baseline_scores.len() as f64;
    let mean_candidate: f64 = candidate_scores.iter().sum::<f64>() / candidate_scores.len() as f64;
    Some(sanitize_f64(mean_candidate - mean_baseline))
}

/// Compare two sets of benchmark results (baseline vs candidate).
///
/// Matches fixtures by [`ScrapeBenchmarkResult::fixture_id`] and computes
/// per-fixture and aggregate deltas for latency, throughput, quality, and
/// memory.
///
/// Fixtures that appear only in one run are skipped; only overlapping
/// `fixture_id` values contribute to aggregate metrics.
///
/// # Sign convention
///
/// - `latency_delta_pct` and `memory_delta_pct`: negative means the candidate
///   is *better* (faster / less memory).
/// - `throughput_delta_pct`: positive means the candidate is *better*.
/// - `quality_delta`: positive means the candidate has higher quality.
pub fn compare_results(
    baseline_results: &[ScrapeBenchmarkResult],
    candidate_results: &[ScrapeBenchmarkResult],
    baseline_name: &str,
    candidate_name: &str,
) -> ComparisonReport {
    let (fixture_comparisons, mut latency_deltas, baseline_quality_scores, candidate_quality_scores) =
        build_fixture_comparisons(baseline_results, candidate_results);

    let latency_delta_pct = percentile_r7(&mut latency_deltas, 0.50).unwrap_or(0.0);
    let latency_delta_pct = sanitize_f64(latency_delta_pct);

    let quality_delta = mean_quality_delta(&baseline_quality_scores, &candidate_quality_scores);

    let baseline_perf = build_performance_report(baseline_results);
    let candidate_perf = build_performance_report(candidate_results);

    let throughput_delta_pct = if baseline_perf.throughput_pages_per_sec > 0.0 {
        sanitize_f64(
            (candidate_perf.throughput_pages_per_sec - baseline_perf.throughput_pages_per_sec)
                / baseline_perf.throughput_pages_per_sec
                * 100.0,
        )
    } else {
        0.0
    };

    let memory_delta_pct = if baseline_perf.peak_memory_bytes > 0 {
        sanitize_f64(
            (candidate_perf.peak_memory_bytes as f64 - baseline_perf.peak_memory_bytes as f64)
                / baseline_perf.peak_memory_bytes as f64
                * 100.0,
        )
    } else {
        0.0
    };

    ComparisonReport {
        baseline: baseline_name.to_owned(),
        candidate: candidate_name.to_owned(),
        latency_delta_pct,
        throughput_delta_pct,
        quality_delta,
        memory_delta_pct,
        fixture_comparisons,
    }
}

/// Pick a trend word for `value`, using the sign convention where negative is `when_negative`
/// and positive is `when_positive`.
fn trend_label(
    value: f64,
    when_negative: &'static str,
    when_positive: &'static str,
    when_zero: &'static str,
) -> &'static str {
    if value < 0.0 {
        when_negative
    } else if value > 0.0 {
        when_positive
    } else {
        when_zero
    }
}

fn print_top_fixture_deltas(fixture_comparisons: &[FixtureComparison]) {
    let mut sorted: Vec<&FixtureComparison> = fixture_comparisons.iter().collect();
    sorted.sort_by(|a, b| {
        b.latency_delta_pct
            .partial_cmp(&a.latency_delta_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let regressions: Vec<_> = sorted.iter().filter(|f| f.latency_delta_pct > 0.0).take(5).collect();
    if !regressions.is_empty() {
        eprintln!("---");
        eprintln!("Top regressions (slowest):");
        for f in regressions {
            eprintln!(
                "  {:+.1}%  {}  ({:.1} -> {:.1} ms)",
                f.latency_delta_pct, f.fixture_id, f.baseline_duration_ms, f.candidate_duration_ms,
            );
        }
    }

    let improvements: Vec<_> = sorted
        .iter()
        .rev()
        .filter(|f| f.latency_delta_pct < 0.0)
        .take(5)
        .collect();
    if !improvements.is_empty() {
        eprintln!("---");
        eprintln!("Top improvements (fastest):");
        for f in improvements {
            eprintln!(
                "  {:+.1}%  {}  ({:.1} -> {:.1} ms)",
                f.latency_delta_pct, f.fixture_id, f.baseline_duration_ms, f.candidate_duration_ms,
            );
        }
    }
}

/// Print a comparison report to **stderr**.
///
/// Shows aggregate deltas and the top-5 regressions and top-5 improvements
/// by per-fixture latency delta.
pub fn print_comparison(report: &ComparisonReport) {
    eprintln!("=== Comparison: {} vs {} ===", report.candidate, report.baseline);
    eprintln!("Matched fixtures: {}", report.fixture_comparisons.len());
    eprintln!("---");

    let latency_label = trend_label(report.latency_delta_pct, "faster", "slower", "unchanged");
    eprintln!("Latency    : {:+.1}% ({})", report.latency_delta_pct, latency_label);

    let throughput_label = trend_label(report.throughput_delta_pct, "worse", "better", "unchanged");
    eprintln!(
        "Throughput : {:+.1}% ({})",
        report.throughput_delta_pct, throughput_label
    );

    let memory_label = trend_label(report.memory_delta_pct, "less", "more", "unchanged");
    eprintln!("Memory     : {:+.1}% ({})", report.memory_delta_pct, memory_label);

    if let Some(qd) = report.quality_delta {
        let quality_label = trend_label(qd, "worse", "better", "unchanged");
        eprintln!("Quality    : {:+.4} ({})", qd, quality_label);
    }

    print_top_fixture_deltas(&report.fixture_comparisons);

    eprintln!("================================");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::test_support::{make_quality, make_result_named};

    #[test]
    fn test_compare_results_empty_sets() {
        let report = compare_results(&[], &[], "base", "cand");
        assert_eq!(report.baseline, "base");
        assert_eq!(report.candidate, "cand");
        assert_eq!(report.latency_delta_pct, 0.0);
        assert_eq!(report.throughput_delta_pct, 0.0);
        assert!(report.quality_delta.is_none());
        assert!(report.fixture_comparisons.is_empty());
    }

    #[test]
    fn test_compare_results_candidate_faster() {
        let baseline = vec![make_result_named("f1", "base", 200.0, None, 0)];
        let candidate = vec![make_result_named("f1", "cand", 100.0, None, 0)];
        let report = compare_results(&baseline, &candidate, "base", "cand");

        assert_eq!(report.fixture_comparisons.len(), 1);
        let fc = &report.fixture_comparisons[0];
        assert_eq!(fc.fixture_id, "f1");
        assert!((fc.latency_delta_pct - (-50.0)).abs() < 1e-9);
        assert!((report.latency_delta_pct - (-50.0)).abs() < 1e-9);
    }

    #[test]
    fn test_compare_results_candidate_slower() {
        let baseline = vec![make_result_named("f1", "base", 100.0, None, 0)];
        let candidate = vec![make_result_named("f1", "cand", 150.0, None, 0)];
        let report = compare_results(&baseline, &candidate, "base", "cand");

        assert!((report.latency_delta_pct - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_compare_results_unmatched_fixtures_skipped() {
        let baseline = vec![make_result_named("f1", "base", 100.0, None, 0)];
        let candidate = vec![
            make_result_named("f1", "cand", 100.0, None, 0),
            make_result_named("f2", "cand", 100.0, None, 0),
        ];
        let report = compare_results(&baseline, &candidate, "base", "cand");
        assert_eq!(report.fixture_comparisons.len(), 1);
        assert_eq!(report.fixture_comparisons[0].fixture_id, "f1");
    }

    #[test]
    fn test_compare_results_quality_delta() {
        let bq = make_quality(0.8, 0.8, 0.8);
        let cq = make_quality(0.9, 0.9, 0.9);
        let baseline = vec![make_result_named("f1", "base", 100.0, Some(bq), 0)];
        let candidate = vec![make_result_named("f1", "cand", 100.0, Some(cq), 0)];
        let report = compare_results(&baseline, &candidate, "base", "cand");

        let qd = report.quality_delta.expect("quality_delta should be Some");
        assert!((qd - 0.1).abs() < 1e-9);
        let fc = &report.fixture_comparisons[0];
        assert!((fc.quality_delta.unwrap() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn test_compare_results_quality_delta_none_when_missing() {
        let baseline = vec![make_result_named("f1", "base", 100.0, None, 0)];
        let cq = make_quality(0.9, 0.9, 0.9);
        let candidate = vec![make_result_named("f1", "cand", 100.0, Some(cq), 0)];
        let report = compare_results(&baseline, &candidate, "base", "cand");

        assert!(report.quality_delta.is_none());
        assert!(report.fixture_comparisons[0].quality_delta.is_none());
    }

    #[test]
    fn test_compare_results_memory_delta() {
        let mb = 1024 * 1024;
        let baseline = vec![make_result_named("f1", "base", 100.0, None, 100 * mb)];
        let candidate = vec![make_result_named("f1", "cand", 100.0, None, 80 * mb)];
        let report = compare_results(&baseline, &candidate, "base", "cand");
        assert!((report.memory_delta_pct - (-20.0)).abs() < 1e-9);
    }

    #[test]
    fn test_compare_results_median_latency_delta() {
        let baseline = vec![
            make_result_named("f1", "base", 100.0, None, 0),
            make_result_named("f2", "base", 100.0, None, 0),
            make_result_named("f3", "base", 100.0, None, 0),
        ];
        let candidate = vec![
            make_result_named("f1", "cand", 50.0, None, 0),
            make_result_named("f2", "cand", 100.0, None, 0),
            make_result_named("f3", "cand", 150.0, None, 0),
        ];
        let report = compare_results(&baseline, &candidate, "base", "cand");
        assert_eq!(report.fixture_comparisons.len(), 3);
        assert!((report.latency_delta_pct - 0.0).abs() < 1e-9);
    }
}
