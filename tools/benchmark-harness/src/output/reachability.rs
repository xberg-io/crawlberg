//! Aggregation and reporting for reachability verification results.

use ahash::AHashMap;

use crate::stats::sanitize_f64;
use crate::types::{CategoryReport, ReachabilityReport, ScrapeBenchmarkResult, ScrapeFixture};

/// Aggregate reachability results from all fixtures into a [`ReachabilityReport`].
///
/// Returns `None` when no fixture in the run defines any verification rules,
/// so callers can omit the section entirely for non-reachability runs.
pub(crate) fn build_reachability_report(
    results: &[ScrapeBenchmarkResult],
    fixtures: &[ScrapeFixture],
) -> Option<ReachabilityReport> {
    let reachable_results: Vec<&ScrapeBenchmarkResult> = results.iter().filter(|r| r.reachability.is_some()).collect();

    if reachable_results.is_empty() {
        return None;
    }

    let total = reachable_results.len();
    let verified = reachable_results
        .iter()
        .filter(|r| r.reachability.as_ref().is_some_and(|rr| rr.verified))
        .count();
    let false_positives = reachable_results
        .iter()
        .filter(|r| r.reachability.as_ref().is_some_and(|rr| rr.is_false_positive))
        .count();

    let success_rate = if total > 0 { verified as f64 / total as f64 } else { 0.0 };
    let false_positive_rate = if total > 0 {
        false_positives as f64 / total as f64
    } else {
        0.0
    };

    let fixture_map: AHashMap<&str, &ScrapeFixture> = fixtures.iter().map(|f| (f.id.as_str(), f)).collect();
    let mut category_map: AHashMap<String, Vec<&ScrapeBenchmarkResult>> = AHashMap::new();
    for result in &reachable_results {
        let cat = fixture_map
            .get(result.fixture_id.as_str())
            .and_then(|f| f.category.as_deref())
            .unwrap_or("uncategorized");
        category_map.entry(cat.to_owned()).or_default().push(result);
    }

    let categories: Vec<CategoryReport> = category_map
        .into_iter()
        .map(|(category, cat_results)| {
            let cat_total = cat_results.len();
            let cat_verified = cat_results
                .iter()
                .filter(|r| r.reachability.as_ref().is_some_and(|rr| rr.verified))
                .count();
            let cat_fp = cat_results
                .iter()
                .filter(|r| r.reachability.as_ref().is_some_and(|rr| rr.is_false_positive))
                .count();
            let cat_success_rate = if cat_total > 0 {
                cat_verified as f64 / cat_total as f64
            } else {
                0.0
            };
            let avg_ms = if cat_results.is_empty() {
                0.0
            } else {
                cat_results.iter().map(|r| r.duration_ms).sum::<f64>() / cat_results.len() as f64
            };
            CategoryReport {
                category,
                total: cat_total,
                verified: cat_verified,
                false_positives: cat_fp,
                success_rate: sanitize_f64(cat_success_rate),
                avg_response_time_ms: sanitize_f64(avg_ms),
            }
        })
        .collect();

    Some(ReachabilityReport {
        success_rate: sanitize_f64(success_rate),
        false_positive_rate: sanitize_f64(false_positive_rate),
        categories,
        total,
        verified,
        false_positives,
    })
}

/// Print a reachability report table to **stderr**.
///
/// Printing to stderr keeps stdout clean for callers that pipe JSON output.
pub fn print_reachability_report(report: &ReachabilityReport) {
    eprintln!("=== Reachability Report ===");
    eprintln!(
        "{:<20} {:>5}  {:>10}  {:>7}  {:>10}",
        "Category", "Total", "Verified", "FP Rate", "Avg Time"
    );
    eprintln!("{}", "-".repeat(58));

    let mut sorted_cats = report.categories.clone();
    sorted_cats.sort_by(|a, b| a.category.cmp(&b.category));

    for cat in &sorted_cats {
        eprintln!(
            "{:<20} {:>5}  {:>5}/{:<4}  {:>6.1}%  {:>8.0}ms",
            cat.category,
            cat.total,
            cat.verified,
            cat.total,
            cat.false_positives as f64 / cat.total.max(1) as f64 * 100.0,
            cat.avg_response_time_ms,
        );
    }

    eprintln!("{}", "-".repeat(58));
    eprintln!(
        "{:<20} {:>5}  {:>5}/{:<4}  {:>6.1}%",
        "Aggregate:",
        report.total,
        report.verified,
        report.total,
        report.false_positive_rate * 100.0,
    );
    eprintln!("==========================");
}
