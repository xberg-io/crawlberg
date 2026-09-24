//! Aggregation of per-fixture quality metrics into a [`DatasetQualityReport`].

use ahash::AHashMap;

use crate::stats::sanitize_f64;
use crate::types::{DatasetQualityReport, ScrapeBenchmarkResult, ScrapeFixture, ScrapeQualityMetrics};

/// Mean values for each quality metric, in the same order as [`DatasetQualityReport`]'s
/// `mean_*` fields.
type QualityMeans = (f64, f64, f64, f64, f64, f64);

fn is_expected_failure(fixture_map: &AHashMap<&str, &ScrapeFixture>, fixture_id: &str) -> bool {
    fixture_map.get(fixture_id).is_some_and(|f| f.error.is_some())
}

fn scoreable_metrics<'a>(
    results: &'a [ScrapeBenchmarkResult],
    fixture_map: &AHashMap<&str, &ScrapeFixture>,
) -> Vec<&'a ScrapeQualityMetrics> {
    results
        .iter()
        .filter(|r| {
            if !fixture_map.is_empty() && is_expected_failure(fixture_map, r.fixture_id.as_str()) {
                return false;
            }
            let is_success_status = r.status_code.is_some_and(|code| (200..=299).contains(&code));
            if !is_success_status || r.content_size == 0 {
                return false;
            }
            true
        })
        .filter_map(|r| r.quality.as_ref())
        .collect()
}

fn mean_quality_metrics(scored: &[&ScrapeQualityMetrics]) -> QualityMeans {
    if scored.is_empty() {
        return (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    }
    let n = scored.len() as f64;
    let sum_f1_text: f64 = scored.iter().map(|q| q.f1_text).sum();
    let sum_f1_numeric: f64 = scored.iter().map(|q| q.f1_numeric).sum();
    let sum_quality: f64 = scored.iter().map(|q| q.quality_score).sum();
    let sum_precision: f64 = scored.iter().map(|q| q.precision).sum();
    let sum_recall: f64 = scored.iter().map(|q| q.recall).sum();
    let sum_noise: f64 = scored.iter().map(|q| q.noise_penalty).sum();
    (
        sanitize_f64(sum_f1_text / n),
        sanitize_f64(sum_f1_numeric / n),
        sanitize_f64(sum_quality / n),
        sanitize_f64(sum_precision / n),
        sanitize_f64(sum_recall / n),
        sanitize_f64(sum_noise / n),
    )
}

/// Aggregate quality metrics from results that have them into a [`DatasetQualityReport`].
///
/// When `fixtures` is non-empty, fixtures that represent expected failures
/// (those with `error.is_some()`) are excluded from the scoreable pool so they
/// don't penalise coverage. Results with a non-2xx status code or zero content
/// size are also excluded from scoring.
pub(crate) fn build_quality_report(
    results: &[ScrapeBenchmarkResult],
    fixtures: &[ScrapeFixture],
) -> DatasetQualityReport {
    let fixture_map: AHashMap<&str, &ScrapeFixture> = fixtures.iter().map(|f| (f.id.as_str(), f)).collect();

    let expected_failure_count = if fixture_map.is_empty() {
        0
    } else {
        results
            .iter()
            .filter(|r| is_expected_failure(&fixture_map, r.fixture_id.as_str()))
            .count()
    };

    let total_urls = results.len();
    let scoreable_urls = total_urls.saturating_sub(expected_failure_count);
    let successful_urls = results.iter().filter(|r| r.success).count();

    let scored = scoreable_metrics(results, &fixture_map);
    let scored_urls = scored.len();

    let coverage = if scoreable_urls == 0 {
        0.0
    } else {
        scored_urls as f64 / scoreable_urls as f64
    };

    let (mean_f1_text, mean_f1_numeric, mean_quality_score, mean_precision, mean_recall, mean_noise_penalty) =
        mean_quality_metrics(&scored);

    DatasetQualityReport {
        coverage: sanitize_f64(coverage),
        mean_f1_text,
        mean_f1_numeric,
        mean_quality_score,
        mean_precision,
        mean_recall,
        mean_noise_penalty,
        total_urls: scoreable_urls,
        successful_urls,
        scored_urls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::test_support::{make_quality, make_result};

    #[test]
    fn test_quality_report_empty_results() {
        let report = build_quality_report(&[], &[]);
        assert_eq!(report.total_urls, 0);
        assert_eq!(report.scored_urls, 0);
        assert_eq!(report.coverage, 0.0);
        assert_eq!(report.mean_quality_score, 0.0);
    }

    #[test]
    fn test_quality_report_no_quality_metrics() {
        let results = vec![make_result(true, 100.0, None)];
        let report = build_quality_report(&results, &[]);
        assert_eq!(report.total_urls, 1);
        assert_eq!(report.scored_urls, 0);
        assert_eq!(report.coverage, 0.0);
    }

    #[test]
    fn test_quality_report_averages() {
        let results = vec![
            make_result(true, 100.0, Some(make_quality(0.8, 0.6, 0.686))),
            make_result(true, 200.0, Some(make_quality(0.4, 1.0, 0.571))),
        ];
        let report = build_quality_report(&results, &[]);

        assert_eq!(report.total_urls, 2);
        assert_eq!(report.scored_urls, 2);
        assert!((report.coverage - 1.0).abs() < 1e-9);
        assert!((report.mean_f1_text - 0.6).abs() < 1e-9);
    }

    #[test]
    fn test_quality_report_excludes_expected_failures() {
        use crate::types::ScrapeFixture;
        let mut result_ok = make_result(true, 100.0, Some(make_quality(0.8, 0.9, 0.847)));
        result_ok.fixture_id = "ok".to_owned();

        let mut result_err = make_result(false, 50.0, None);
        result_err.fixture_id = "expected_fail".to_owned();

        let fixtures = vec![
            ScrapeFixture {
                id: "ok".to_owned(),
                url: "https://example.com".to_owned(),
                truth_text: None,
                lie_text: None,
                error: None,
                split: None,
                tags: vec![],
                expected_status: None,
                verify_selectors: vec![],
                verify_text: vec![],
                category: None,
            },
            ScrapeFixture {
                id: "expected_fail".to_owned(),
                url: "https://example.com/fail".to_owned(),
                truth_text: None,
                lie_text: None,
                error: Some("expected network error".to_owned()),
                split: None,
                tags: vec![],
                expected_status: None,
                verify_selectors: vec![],
                verify_text: vec![],
                category: None,
            },
        ];

        let report = build_quality_report(&[result_ok, result_err], &fixtures);
        assert_eq!(report.total_urls, 1);
        assert_eq!(report.scored_urls, 1);
        assert!((report.coverage - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_quality_report_excludes_non_2xx_and_empty_content() {
        let mut result_404 = make_result(false, 50.0, Some(make_quality(0.0, 0.0, 0.0)));
        result_404.status_code = Some(404);
        result_404.content_size = 0;

        let mut result_empty = make_result(true, 80.0, Some(make_quality(0.0, 0.0, 0.0)));
        result_empty.status_code = Some(200);
        result_empty.content_size = 0;

        let mut result_ok = make_result(true, 100.0, Some(make_quality(0.8, 0.9, 0.847)));
        result_ok.status_code = Some(200);
        result_ok.content_size = 512;

        let report = build_quality_report(&[result_404, result_empty, result_ok], &[]);
        assert_eq!(report.scored_urls, 1);
    }
}
