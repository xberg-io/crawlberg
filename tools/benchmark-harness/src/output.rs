//! Result serialization and human-readable reporting.
//!
//! Writes [`BenchmarkOutput`] to JSON files and prints summary tables to stderr.

use std::path::Path;

use chrono::Utc;

use ahash::AHashMap;

use crate::adapter::ScrapeOutput;
use crate::config::BenchmarkConfig;
use crate::stats::{percentile_r7, sanitize_f64};

use crate::Result;
use crate::types::{
    BenchmarkMetadata, BenchmarkOutput, CategoryReport, ComparisonReport, DatasetPerformanceReport,
    DatasetQualityReport, ExecutionMode, FixtureComparison, ReachabilityReport, ScrapeBenchmarkResult, ScrapeFixture,
};

/// Write the full benchmark output to `{output_dir}/results.json` as
/// pretty-printed JSON.
///
/// Creates `output_dir` if it does not exist.
///
/// # Errors
///
/// Returns [`crate::Error`] if the directory cannot be created or the file
/// cannot be written.
pub fn write_results(output_dir: &Path, output: &BenchmarkOutput) -> Result<()> {
    std::fs::create_dir_all(output_dir)?;
    let path = output_dir.join("results.json");
    let json = serde_json::to_string_pretty(output)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Sanitize a fixture ID for use as a filename component.
///
/// Replaces characters that are unsafe in filesystem paths with `_`.
fn sanitize_fixture_id(id: &str) -> String {
    id.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Write per-fixture extraction outputs to `{output_dir}/fixtures/`.
///
/// For each fixture that has a successful [`ScrapeOutput`], creates:
/// - `{fixture_id}.md` — extracted markdown (or a note when unavailable)
/// - `{fixture_id}.html` — raw HTML response
/// - `{fixture_id}.meta.json` — metadata (URL, status, quality scores, token
///   analysis, duration)
///
/// Fixtures whose output is `None` (failed scrapes) are skipped entirely.
///
/// This enables manual inspection of extracted content vs ground truth to
/// support debugging quality gaps and reaching 100% extraction coverage.
///
/// # Errors
///
/// Returns [`crate::Error`] if the directory cannot be created or any file
/// cannot be written.
pub fn write_fixture_outputs(
    output_dir: &Path,
    results: &[ScrapeBenchmarkResult],
    outputs: &[(String, Option<ScrapeOutput>)],
    fixtures: &[ScrapeFixture],
) -> Result<()> {
    let fixtures_dir = output_dir.join("fixtures");
    std::fs::create_dir_all(&fixtures_dir)?;

    let result_map: AHashMap<&str, &ScrapeBenchmarkResult> =
        results.iter().map(|r| (r.fixture_id.as_str(), r)).collect();
    let fixture_map: AHashMap<&str, &ScrapeFixture> = fixtures.iter().map(|f| (f.id.as_str(), f)).collect();

    for (fixture_id, maybe_output) in outputs {
        let Some(output) = maybe_output else {
            continue;
        };

        let safe_id = sanitize_fixture_id(fixture_id);
        let result = result_map.get(fixture_id.as_str()).copied();
        let fixture = fixture_map.get(fixture_id.as_str()).copied();

        let content_path = fixtures_dir.join(format!("{safe_id}.md"));
        let content_str = output
            .content
            .as_deref()
            .unwrap_or("<!-- content extraction was not available for this fixture -->\n");
        std::fs::write(&content_path, content_str)?;

        let html_path = fixtures_dir.join(format!("{safe_id}.html"));
        std::fs::write(&html_path, &output.html)?;

        let url = result.map(|r| r.url.as_str()).unwrap_or("");
        let status_code = output.status_code;
        let browser_used = output.browser_used;
        let content_size = output.content_size;
        let duration_ms = result.map(|r| r.duration_ms).unwrap_or(0.0);
        let truth_text = fixture.and_then(|f| f.truth_text.as_deref()).unwrap_or("");
        let lie_text = fixture.and_then(|f| f.lie_text.as_deref()).unwrap_or("");

        let quality_json = result
            .and_then(|r| r.quality.as_ref())
            .map(|q| {
                serde_json::json!({
                    "f1_text": q.f1_text,
                    "f1_numeric": q.f1_numeric,
                    "quality_score": q.quality_score,
                    "precision": q.precision,
                    "recall": q.recall,
                    "noise_penalty": q.noise_penalty,
                    "correct": q.correct,
                    "missing_tokens": q.missing_tokens,
                    "extra_tokens": q.extra_tokens,
                })
            })
            .unwrap_or(serde_json::Value::Null);

        let meta = serde_json::json!({
            "fixture_id": fixture_id,
            "url": url,
            "status_code": status_code,
            "browser_used": browser_used,
            "content_size": content_size,
            "truth_text": truth_text,
            "lie_text": lie_text,
            "quality": quality_json,
            "duration_ms": duration_ms,
        });

        let meta_path = fixtures_dir.join(format!("{safe_id}.meta.json"));
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
    }

    Ok(())
}

/// Print a human-readable summary of `output` to **stderr**.
///
/// Printing to stderr keeps stdout clean for callers that pipe JSON output.
pub fn print_summary(output: &BenchmarkOutput) {
    let meta = &output.metadata;
    let perf = &output.performance_report;

    let mode_str = match meta.execution_mode {
        ExecutionMode::Live => "live",
        ExecutionMode::Cached => "cached",
    };

    let successful = output.results.iter().filter(|r| r.success).count();
    let total = output.results.len();

    eprintln!("=== Benchmark Summary ===");
    eprintln!("Framework   : {}", meta.framework);
    eprintln!("Mode        : {mode_str}");
    eprintln!("Dataset     : {}", meta.dataset);
    eprintln!("Fixtures    : {total}");
    eprintln!("Successful  : {successful} / {total}");
    eprintln!("Iterations  : {}", meta.iterations);
    eprintln!("Concurrency : {}", meta.max_concurrent);
    eprintln!("Timestamp   : {}", meta.timestamp);
    eprintln!("---");
    eprintln!("Latency p50 : {:.1} ms", perf.latency_p50_ms);
    eprintln!("Latency p95 : {:.1} ms", perf.latency_p95_ms);
    eprintln!("Latency p99 : {:.1} ms", perf.latency_p99_ms);
    eprintln!("Throughput  : {:.2} pages/sec", perf.throughput_pages_per_sec);
    eprintln!("Peak memory : {:.1} MB", perf.peak_memory_bytes as f64 / 1_048_576.0);

    if let Some(ref quality) = output.quality_report {
        eprintln!("---");
        eprintln!(
            "Coverage    : {:.1}% ({} / {} fixtures scored)",
            quality.coverage * 100.0,
            quality.scored_urls,
            quality.total_urls
        );
        eprintln!("F1 text     : {:.3}", quality.mean_f1_text);
        eprintln!("F1 numeric  : {:.3}", quality.mean_f1_numeric);
        eprintln!("Quality     : {:.3}", quality.mean_quality_score);
        eprintln!("Precision   : {:.3}", quality.mean_precision);
        eprintln!("Recall      : {:.3}", quality.mean_recall);
        eprintln!("Noise pen.  : {:.3}", quality.mean_noise_penalty);
    }

    eprintln!("=========================");
}

/// Aggregate individual fixture results into a [`BenchmarkOutput`].
///
/// Computes:
/// - [`DatasetQualityReport`] from results that have quality metrics, filtered
///   to exclude expected failures when `fixtures` is non-empty
/// - [`DatasetPerformanceReport`] from per-result `duration_ms` and metrics
/// - [`BenchmarkMetadata`] from the config and adapter name
///
/// When `fixtures` is non-empty, quality scoring skips fixtures that have an
/// expected error, a non-2xx status code, or zero content size, so that
/// expected failures don't penalise coverage metrics.
pub fn aggregate_results(
    results: &[ScrapeBenchmarkResult],
    fixtures: &[ScrapeFixture],
    config: &BenchmarkConfig,
    adapter_name: &str,
) -> BenchmarkOutput {
    let metadata = build_metadata(results, config, adapter_name);
    let performance_report = build_performance_report(results);
    let quality_report = if config.measure_quality {
        Some(build_quality_report(results, fixtures))
    } else {
        None
    };
    let reachability_report = build_reachability_report(results, fixtures);

    BenchmarkOutput {
        metadata,
        quality_report,
        performance_report,
        reachability_report,
        results: results.to_vec(),
    }
}

/// Build [`BenchmarkMetadata`] from config and results.
fn build_metadata(
    results: &[ScrapeBenchmarkResult],
    config: &BenchmarkConfig,
    adapter_name: &str,
) -> BenchmarkMetadata {
    let dataset = config
        .dataset_name
        .clone()
        .or_else(|| config.cache_dir.file_name().and_then(|n| n.to_str()).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());

    BenchmarkMetadata {
        timestamp: Utc::now().to_rfc3339(),
        harness_version: env!("CARGO_PKG_VERSION").to_owned(),
        execution_mode: config.execution_mode,
        dataset,
        fixture_count: results.len(),
        framework: adapter_name.to_owned(),
        iterations: config.benchmark_iterations,
        max_concurrent: config.max_concurrent,
    }
}

/// Aggregate duration and memory observations into a [`DatasetPerformanceReport`].
fn build_performance_report(results: &[ScrapeBenchmarkResult]) -> DatasetPerformanceReport {
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

/// Aggregate quality metrics from results that have them into a [`DatasetQualityReport`].
///
/// When `fixtures` is non-empty, fixtures that represent expected failures
/// (those with `error.is_some()`) are excluded from the scoreable pool so they
/// don't penalise coverage. Results with a non-2xx status code or zero content
/// size are also excluded from scoring.
fn build_quality_report(results: &[ScrapeBenchmarkResult], fixtures: &[ScrapeFixture]) -> DatasetQualityReport {
    let fixture_map: AHashMap<&str, &ScrapeFixture> = fixtures.iter().map(|f| (f.id.as_str(), f)).collect();

    let expected_failure_count = if fixture_map.is_empty() {
        0
    } else {
        results
            .iter()
            .filter(|r| {
                fixture_map
                    .get(r.fixture_id.as_str())
                    .is_some_and(|f| f.error.is_some())
            })
            .count()
    };

    let total_urls = results.len();
    let scoreable_urls = total_urls.saturating_sub(expected_failure_count);
    let successful_urls = results.iter().filter(|r| r.success).count();

    let scored: Vec<&crate::types::ScrapeQualityMetrics> = results
        .iter()
        .filter(|r| {
            if !fixture_map.is_empty()
                && fixture_map
                    .get(r.fixture_id.as_str())
                    .is_some_and(|f| f.error.is_some())
            {
                return false;
            }
            let is_success_status = r.status_code.is_some_and(|code| (200..=299).contains(&code));
            if !is_success_status || r.content_size == 0 {
                return false;
            }
            true
        })
        .filter_map(|r| r.quality.as_ref())
        .collect();

    let scored_urls = scored.len();

    let coverage = if scoreable_urls == 0 {
        0.0
    } else {
        scored_urls as f64 / scoreable_urls as f64
    };

    let (mean_f1_text, mean_f1_numeric, mean_quality_score, mean_precision, mean_recall, mean_noise_penalty) =
        if scored_urls == 0 {
            (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        } else {
            let n = scored_urls as f64;
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
        };

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

/// Aggregate reachability results from all fixtures into a [`ReachabilityReport`].
///
/// Returns `None` when no fixture in the run defines any verification rules,
/// so callers can omit the section entirely for non-reachability runs.
fn build_reachability_report(
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

    let latency_delta_pct = percentile_r7(&mut latency_deltas, 0.50).unwrap_or(0.0);
    let latency_delta_pct = sanitize_f64(latency_delta_pct);

    let quality_delta = if !baseline_quality_scores.is_empty() && !candidate_quality_scores.is_empty() {
        let mean_baseline: f64 = baseline_quality_scores.iter().sum::<f64>() / baseline_quality_scores.len() as f64;
        let mean_candidate: f64 = candidate_quality_scores.iter().sum::<f64>() / candidate_quality_scores.len() as f64;
        Some(sanitize_f64(mean_candidate - mean_baseline))
    } else {
        None
    };

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

/// Print a comparison report to **stderr**.
///
/// Shows aggregate deltas and the top-5 regressions and top-5 improvements
/// by per-fixture latency delta.
pub fn print_comparison(report: &ComparisonReport) {
    eprintln!("=== Comparison: {} vs {} ===", report.candidate, report.baseline);
    eprintln!("Matched fixtures: {}", report.fixture_comparisons.len());
    eprintln!("---");

    let latency_label = if report.latency_delta_pct < 0.0 {
        "faster"
    } else if report.latency_delta_pct > 0.0 {
        "slower"
    } else {
        "unchanged"
    };
    eprintln!("Latency    : {:+.1}% ({})", report.latency_delta_pct, latency_label);

    let throughput_label = if report.throughput_delta_pct > 0.0 {
        "better"
    } else if report.throughput_delta_pct < 0.0 {
        "worse"
    } else {
        "unchanged"
    };
    eprintln!(
        "Throughput : {:+.1}% ({})",
        report.throughput_delta_pct, throughput_label
    );

    let memory_label = if report.memory_delta_pct < 0.0 {
        "less"
    } else if report.memory_delta_pct > 0.0 {
        "more"
    } else {
        "unchanged"
    };
    eprintln!("Memory     : {:+.1}% ({})", report.memory_delta_pct, memory_label);

    if let Some(qd) = report.quality_delta {
        let quality_label = if qd > 0.0 {
            "better"
        } else if qd < 0.0 {
            "worse"
        } else {
            "unchanged"
        };
        eprintln!("Quality    : {:+.4} ({})", qd, quality_label);
    }

    let mut sorted: Vec<&FixtureComparison> = report.fixture_comparisons.iter().collect();
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

    eprintln!("================================");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ErrorKind, ExecutionMode, IterationResult, PerformanceMetrics, ScrapeQualityMetrics};

    fn make_result(success: bool, duration_ms: f64, quality: Option<ScrapeQualityMetrics>) -> ScrapeBenchmarkResult {
        ScrapeBenchmarkResult {
            framework: "test".to_owned(),
            url: "https://example.com".to_owned(),
            fixture_id: "f1".to_owned(),
            success,
            error_message: None,
            error_kind: ErrorKind::None,
            duration_ms,
            metrics: PerformanceMetrics {
                peak_memory_bytes: 1024 * 1024 * 100,
                avg_cpu_percent: 0.0,
                throughput_pages_per_sec: if duration_ms > 0.0 { 1_000.0 / duration_ms } else { 0.0 },
                p50_memory_bytes: 0,
                p95_memory_bytes: 0,
                p99_memory_bytes: 0,
            },
            quality,
            status_code: Some(200),
            browser_used: false,
            js_render_hint: false,
            content_size: 512,
            iterations: vec![IterationResult {
                iteration: 0,
                duration_ms,
                success,
                error: None,
                memory_bytes: 0,
            }],
            statistics: None,
            execution_mode: ExecutionMode::Cached,
            reachability: None,
        }
    }

    fn make_quality(f1_text: f64, noise_penalty: f64, quality_score: f64) -> ScrapeQualityMetrics {
        ScrapeQualityMetrics {
            f1_text,
            f1_numeric: 0.0,
            quality_score,
            precision: f1_text,
            recall: f1_text,
            noise_penalty,
            missing_tokens: vec![],
            extra_tokens: vec![],
            correct: quality_score >= 0.95,
        }
    }

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

    #[test]
    fn test_write_results_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = BenchmarkConfig::default();
        let results: Vec<ScrapeBenchmarkResult> = vec![];
        let output = aggregate_results(&results, &[], &config, "test-adapter");
        write_results(dir.path(), &output).unwrap();
        assert!(dir.path().join("results.json").exists());
    }

    #[test]
    fn test_write_results_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let config = BenchmarkConfig::default();
        let results = vec![make_result(true, 150.0, None)];
        let output = aggregate_results(&results, &[], &config, "test-adapter");
        write_results(dir.path(), &output).unwrap();

        let raw = std::fs::read_to_string(dir.path().join("results.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(parsed.get("results").is_some());
        assert!(parsed.get("metadata").is_some());
        assert!(parsed.get("performance_report").is_some());
    }

    #[test]
    fn test_aggregate_results_quality_present_when_enabled() {
        let config = BenchmarkConfig {
            measure_quality: true,
            ..Default::default()
        };
        let results = vec![make_result(true, 100.0, Some(make_quality(0.9, 0.8, 0.847)))];
        let output = aggregate_results(&results, &[], &config, "test");
        assert!(output.quality_report.is_some());
    }

    #[test]
    fn test_aggregate_results_quality_absent_when_disabled() {
        let config = BenchmarkConfig {
            measure_quality: false,
            ..Default::default()
        };
        let results = vec![make_result(true, 100.0, Some(make_quality(0.9, 0.8, 0.847)))];
        let output = aggregate_results(&results, &[], &config, "test");
        assert!(output.quality_report.is_none());
    }

    /// Build a result with an explicit fixture_id, framework, and memory.
    fn make_result_named(
        fixture_id: &str,
        framework: &str,
        duration_ms: f64,
        quality: Option<ScrapeQualityMetrics>,
        peak_memory_bytes: u64,
    ) -> ScrapeBenchmarkResult {
        let mut r = make_result(true, duration_ms, quality);
        r.fixture_id = fixture_id.to_owned();
        r.framework = framework.to_owned();
        r.metrics.peak_memory_bytes = peak_memory_bytes;
        r
    }

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
