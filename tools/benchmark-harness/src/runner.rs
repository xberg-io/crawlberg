//! Benchmark runner — drives scrape adapters across fixture sets and collects results.
//!
//! The runner performs warmup iterations (discarded) followed by timed benchmark
//! iterations, collecting per-fixture timing records and aggregating them into
//! [`ScrapeBenchmarkResult`] values with full [`DurationStatistics`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use ahash::AHashMap;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::Result;
use crate::adapter::ScrapeAdapter;
use crate::cache::HtmlCache;
use crate::config::BenchmarkConfig;
use crate::monitoring::{ResourceMetrics, ResourceMonitor, snapshot_resources};
use crate::quality::compute_scrape_quality;
use crate::stats::{calculate_variance, percentile_r7, sanitize_f64};
use crate::types::{
    DurationStatistics, ErrorKind, ExecutionMode, IterationResult, PerformanceMetrics, ReachabilityResult,
    ScrapeBenchmarkResult, ScrapeFixture,
};

/// Orchestrates warmup and benchmark iterations for a set of fixtures.
///
/// Call [`BenchmarkRunner::run`] to execute the full benchmark and collect
/// per-fixture [`ScrapeBenchmarkResult`] values.
pub struct BenchmarkRunner {
    config: BenchmarkConfig,
    adapter: Arc<dyn ScrapeAdapter>,
    fixtures: Vec<ScrapeFixture>,
    cache: Option<HtmlCache>,
}

impl BenchmarkRunner {
    /// Create a new runner.
    ///
    /// # Arguments
    ///
    /// * `config` — runtime configuration (concurrency, iterations, quality flags, etc.)
    /// * `adapter` — scraping backend to benchmark
    /// * `fixtures` — fixture set to run against
    /// * `cache` — optional HTML cache for cached-mode runs; also written to when
    ///   `config.save_cache` is `true`
    pub fn new(
        config: BenchmarkConfig,
        adapter: Arc<dyn ScrapeAdapter>,
        fixtures: Vec<ScrapeFixture>,
        cache: Option<HtmlCache>,
    ) -> Self {
        Self {
            config,
            adapter,
            fixtures,
            cache,
        }
    }

    /// Run the full benchmark: warmup iterations followed by timed iterations.
    ///
    /// Returns one [`ScrapeBenchmarkResult`] per fixture alongside the last
    /// successful [`crate::adapter::ScrapeOutput`] for each fixture (used for
    /// per-fixture output inspection).  Individual scrape failures do not abort
    /// the run — they are recorded with the appropriate [`ErrorKind`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] if adapter setup or teardown fails.
    pub async fn run(
        &mut self,
    ) -> Result<(
        Vec<ScrapeBenchmarkResult>,
        Vec<(String, Option<crate::adapter::ScrapeOutput>)>,
    )> {
        self.adapter.setup().await?;

        let total_iterations = self.config.warmup_iterations + self.config.benchmark_iterations;
        let fixture_count = self.fixtures.len();

        info!(
            adapter = self.adapter.name(),
            fixtures = fixture_count,
            warmup = self.config.warmup_iterations,
            benchmark = self.config.benchmark_iterations,
            "starting benchmark run"
        );

        let mut monitor = ResourceMonitor::new();
        monitor.start(Duration::from_millis(10)).await;

        let mut iteration_records: Vec<Vec<IterationResult>> =
            vec![Vec::with_capacity(total_iterations); fixture_count];

        let mut last_outputs: Vec<Option<crate::adapter::ScrapeOutput>> = vec![None; fixture_count];

        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent));

        let cached_html_map: AHashMap<String, String> = self
            .fixtures
            .iter()
            .filter_map(|f| {
                self.cache.as_ref().and_then(|c| match c.get(&f.url) {
                    Ok(Some(resp)) => Some((f.url.clone(), resp.body.clone())),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::warn!(url = %f.url, error = %e, "failed to read cached response");
                        None
                    }
                })
            })
            .collect();

        if self.config.execution_mode == ExecutionMode::Cached {
            let cached_count = cached_html_map.len();
            let total = self.fixtures.len();
            if cached_count < total {
                tracing::warn!(
                    cached = cached_count,
                    total = total,
                    "cached mode: {}/{} fixtures have cached HTML; remaining will be fetched live",
                    cached_count,
                    total
                );
            }
        }

        let cached_html_map = Arc::new(cached_html_map);

        let run_start = Instant::now();

        for iteration in 0..total_iterations {
            let is_warmup = iteration < self.config.warmup_iterations;
            debug!(
                iteration,
                is_warmup,
                "running iteration {}/{}",
                iteration + 1,
                total_iterations
            );

            let work: Vec<(usize, ScrapeFixture)> =
                self.fixtures.iter().enumerate().map(|(i, f)| (i, f.clone())).collect();

            let results = self
                .run_iteration(work, Arc::clone(&semaphore), Arc::clone(&cached_html_map), iteration)
                .await;

            for (fixture_idx, iteration_result, maybe_output) in results {
                if !is_warmup {
                    if iteration_result.success
                        && let Some(output) = maybe_output
                    {
                        last_outputs[fixture_idx] = Some(output);
                    }
                    iteration_records[fixture_idx].push(iteration_result);
                }
            }

            if self.config.execution_mode == ExecutionMode::Live
                && self.config.rate_limit_ms > 0
                && iteration + 1 < total_iterations
            {
                tokio::time::sleep(Duration::from_millis(self.config.rate_limit_ms)).await;
            }
        }

        monitor.stop();
        let resource_metrics = monitor.metrics().await;

        info!("collecting results");

        let mut results = Vec::with_capacity(fixture_count);

        for (fixture_idx, fixture) in self.fixtures.iter().enumerate() {
            let records = &iteration_records[fixture_idx];
            let output = last_outputs[fixture_idx].as_ref();

            let result = build_result(
                fixture,
                self.adapter.name(),
                records,
                output,
                &resource_metrics,
                self.config.execution_mode,
                self.config.measure_quality,
            );
            results.push(result);
        }

        if self.config.save_cache
            && let Some(ref mut cache) = self.cache
        {
            save_responses_to_cache(cache, &self.fixtures, &last_outputs);
        }

        self.adapter.teardown().await?;

        let elapsed_secs = run_start.elapsed().as_secs_f64();
        let throughput = if elapsed_secs > 0.0 {
            fixture_count as f64 / elapsed_secs
        } else {
            0.0
        };

        info!(
            fixtures = fixture_count,
            throughput_pages_per_sec = throughput,
            "benchmark run complete"
        );

        let fixture_outputs: Vec<(String, Option<crate::adapter::ScrapeOutput>)> = self
            .fixtures
            .iter()
            .zip(last_outputs)
            .map(|(f, o)| (f.id.clone(), o))
            .collect();

        Ok((results, fixture_outputs))
    }

    /// Run one iteration across all fixtures concurrently (limited by the shared semaphore).
    ///
    /// * `semaphore` — shared across all calls so concurrency stays within `max_concurrent`.
    /// * `cached_html_map` — pre-loaded cache keyed by URL; avoids blocking I/O in async tasks.
    /// * `iteration_number` — the outer iteration index stored in each [`IterationResult`].
    ///
    /// Returns `(fixture_index, IterationResult, Option<ScrapeOutput>)` for each fixture.
    async fn run_iteration(
        &self,
        work: Vec<(usize, ScrapeFixture)>,
        semaphore: Arc<Semaphore>,
        cached_html_map: Arc<AHashMap<String, String>>,
        iteration_number: usize,
    ) -> Vec<(usize, IterationResult, Option<crate::adapter::ScrapeOutput>)> {
        let mut join_set = JoinSet::new();

        for (fixture_idx, fixture) in work {
            let permit = semaphore.clone().acquire_owned().await.ok();

            let adapter = Arc::clone(&self.adapter);
            let timeout = self.config.timeout;
            let rate_limit_ms = if self.config.execution_mode == ExecutionMode::Live {
                self.config.rate_limit_ms
            } else {
                0
            };
            let cached_html = cached_html_map.get(&fixture.url).cloned();

            let iteration_num = iteration_number;

            join_set.spawn(async move {
                if rate_limit_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(rate_limit_ms)).await;
                }

                let pre_snapshot = snapshot_resources();
                let start = Instant::now();
                let scrape_result = adapter.scrape(&fixture.url, cached_html.as_deref(), timeout).await;
                let duration_ms = start.elapsed().as_secs_f64() * 1_000.0;
                let post_snapshot = snapshot_resources();

                drop(permit);

                let _ = pre_snapshot;
                let memory_bytes = post_snapshot.memory_bytes;

                let (success, error_msg, maybe_output) = match scrape_result {
                    Ok(output) => (true, None, Some(output)),
                    Err(e) => (false, Some(e.to_string()), None),
                };

                let iteration_result = IterationResult {
                    iteration: iteration_num,
                    duration_ms: sanitize_f64(duration_ms),
                    success,
                    error: error_msg,
                    memory_bytes,
                };

                (fixture_idx, iteration_result, maybe_output)
            });
        }

        let mut results = Vec::new();
        while let Some(task_result) = join_set.join_next().await {
            match task_result {
                Ok(item) => results.push(item),
                Err(join_error) => {
                    warn!(error = %join_error, "task panicked during iteration");
                }
            }
        }

        results
    }
}

/// Aggregate per-iteration records into a [`ScrapeBenchmarkResult`].
fn build_result(
    fixture: &ScrapeFixture,
    framework: &str,
    records: &[IterationResult],
    last_output: Option<&crate::adapter::ScrapeOutput>,
    resource_metrics: &ResourceMetrics,
    execution_mode: ExecutionMode,
    measure_quality: bool,
) -> ScrapeBenchmarkResult {
    let success = records.iter().any(|r| r.success);
    let error_message = records
        .iter()
        .filter_map(|r| r.error.as_deref())
        .next_back()
        .map(str::to_owned);
    let error_kind = classify_error(error_message.as_deref());

    let mut durations: Vec<f64> = records.iter().filter(|r| r.success).map(|r| r.duration_ms).collect();

    let mean_duration_ms = if durations.is_empty() {
        0.0
    } else {
        durations.iter().sum::<f64>() / durations.len() as f64
    };

    let statistics = if durations.len() >= 2 {
        Some(compute_duration_statistics(&mut durations))
    } else if durations.len() == 1 {
        let single = durations[0];
        Some(DurationStatistics {
            mean_ms: single,
            median_ms: single,
            std_dev_ms: 0.0,
            min_ms: single,
            max_ms: single,
            p95_ms: single,
            p99_ms: single,
            sample_count: 1,
        })
    } else {
        None
    };

    let (status_code, browser_used, js_render_hint, content_size, quality, reachability) =
        if let Some(output) = last_output {
            let quality = if measure_quality {
                if let Some(content) = output.content.as_deref() {
                    compute_scrape_quality(content, fixture.truth_text.as_deref(), fixture.lie_text.as_deref())
                } else {
                    None
                }
            } else {
                None
            };

            let reachability = if !fixture.verify_selectors.is_empty() || !fixture.verify_text.is_empty() {
                Some(crate::verify::verify_content(fixture, output))
            } else {
                None
            };

            (
                Some(output.status_code),
                output.browser_used,
                output.js_render_hint,
                output.content_size,
                quality,
                reachability,
            )
        } else {
            let reachability = if !fixture.verify_selectors.is_empty() || !fixture.verify_text.is_empty() {
                Some(ReachabilityResult {
                    verified: false,
                    selectors_found: 0,
                    selectors_total: fixture.verify_selectors.len(),
                    text_found: 0,
                    text_total: fixture.verify_text.len(),
                    is_false_positive: false,
                })
            } else {
                None
            };
            (None, false, false, 0, None, reachability)
        };

    let throughput = if mean_duration_ms > 0.0 {
        1_000.0 / mean_duration_ms
    } else {
        0.0
    };

    let peak_memory_bytes = records.iter().map(|r| r.memory_bytes).max().unwrap_or(0);
    let (p50_memory_bytes, p95_memory_bytes, p99_memory_bytes) = if records.is_empty() {
        (0, 0, 0)
    } else {
        let mut memory_samples: Vec<f64> = records.iter().map(|r| r.memory_bytes as f64).collect();
        let p50 = crate::stats::percentile_r7(&mut memory_samples, 0.5).unwrap_or(0.0) as u64;
        let p95 = crate::stats::percentile_r7(&mut memory_samples, 0.95).unwrap_or(0.0) as u64;
        let p99 = crate::stats::percentile_r7(&mut memory_samples, 0.99).unwrap_or(0.0) as u64;
        (p50, p95, p99)
    };

    let metrics = PerformanceMetrics {
        peak_memory_bytes,
        avg_cpu_percent: resource_metrics.avg_cpu_percent,
        throughput_pages_per_sec: sanitize_f64(throughput),
        p50_memory_bytes,
        p95_memory_bytes,
        p99_memory_bytes,
    };

    ScrapeBenchmarkResult {
        framework: framework.to_owned(),
        url: fixture.url.clone(),
        fixture_id: fixture.id.clone(),
        success,
        error_message,
        error_kind,
        duration_ms: sanitize_f64(mean_duration_ms),
        metrics,
        quality,
        status_code,
        browser_used,
        js_render_hint,
        content_size,
        iterations: records.to_vec(),
        statistics,
        execution_mode,
        reachability,
    }
}

/// Compute full [`DurationStatistics`] from a non-empty, mutable slice.
///
/// The slice will be sorted in place as a side effect of percentile computation.
fn compute_duration_statistics(durations: &mut [f64]) -> DurationStatistics {
    let n = durations.len();
    let mean_ms = durations.iter().sum::<f64>() / n as f64;
    let variance = calculate_variance(durations);
    let std_dev_ms = variance.sqrt();
    let min_ms = durations.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_ms = durations.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    let median_ms = percentile_r7(durations, 0.5).unwrap_or(mean_ms);
    let p95_ms = percentile_r7(durations, 0.95).unwrap_or(max_ms);
    let p99_ms = percentile_r7(durations, 0.99).unwrap_or(max_ms);

    DurationStatistics {
        mean_ms: sanitize_f64(mean_ms),
        median_ms: sanitize_f64(median_ms),
        std_dev_ms: sanitize_f64(std_dev_ms),
        min_ms: sanitize_f64(min_ms),
        max_ms: sanitize_f64(max_ms),
        p95_ms: sanitize_f64(p95_ms),
        p99_ms: sanitize_f64(p99_ms),
        sample_count: n,
    }
}

/// Map an error message string to the appropriate [`ErrorKind`].
fn classify_error(message: Option<&str>) -> ErrorKind {
    let Some(msg) = message else {
        return ErrorKind::None;
    };
    let lower = msg.to_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        ErrorKind::Timeout
    } else if lower.contains("blocked") || lower.contains("waf") {
        ErrorKind::Blocked
    } else if lower.contains("empty") {
        ErrorKind::EmptyContent
    } else {
        ErrorKind::FrameworkError
    }
}

/// Persist the final scrape outputs for each fixture into the HTML cache.
///
/// Failures per entry are logged as warnings rather than propagated, because a
/// cache write failure should not invalidate benchmark results that have already
/// been collected.
fn save_responses_to_cache(
    cache: &mut HtmlCache,
    fixtures: &[ScrapeFixture],
    outputs: &[Option<crate::adapter::ScrapeOutput>],
) {
    for (fixture, maybe_output) in fixtures.iter().zip(outputs.iter()) {
        let Some(output) = maybe_output else {
            continue;
        };

        let response = crate::cache::CachedResponse {
            url: fixture.url.clone(),
            status_code: output.status_code,
            content_type: "text/html".to_owned(),
            headers: std::collections::HashMap::new(),
            body: output.html.clone(),
        };

        if let Err(error) = cache.insert(&response) {
            warn!(
                url = %fixture.url,
                error = %error,
                "failed to save response to cache"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitoring::ResourceMetrics;

    fn zeroed_resource_metrics() -> ResourceMetrics {
        ResourceMetrics {
            peak_memory_bytes: 0,
            p50_memory_bytes: 0,
            p95_memory_bytes: 0,
            p99_memory_bytes: 0,
            avg_cpu_percent: 0.0,
            peak_cpu_percent: 0.0,
            sample_count: 0,
            baseline_memory_bytes: 0,
        }
    }

    #[test]
    fn test_classify_none_when_no_message() {
        assert_eq!(classify_error(None), ErrorKind::None);
    }

    #[test]
    fn test_classify_timeout() {
        assert_eq!(classify_error(Some("request timed out")), ErrorKind::Timeout);
        assert_eq!(classify_error(Some("timeout after 30s")), ErrorKind::Timeout);
    }

    #[test]
    fn test_classify_blocked() {
        assert_eq!(classify_error(Some("request blocked by WAF")), ErrorKind::Blocked);
        assert_eq!(classify_error(Some("BLOCKED by Cloudflare")), ErrorKind::Blocked);
    }

    #[test]
    fn test_classify_empty_content() {
        assert_eq!(classify_error(Some("returned empty content")), ErrorKind::EmptyContent);
    }

    #[test]
    fn test_classify_framework_error_fallthrough() {
        assert_eq!(
            classify_error(Some("some unexpected adapter failure")),
            ErrorKind::FrameworkError
        );
    }

    #[test]
    fn test_duration_stats_known_values() {
        let mut durations = vec![100.0_f64, 200.0, 300.0, 400.0, 500.0];
        let stats = compute_duration_statistics(&mut durations);

        assert!((stats.mean_ms - 300.0).abs() < 1e-9);
        assert!((stats.median_ms - 300.0).abs() < 1e-9);
        assert!((stats.min_ms - 100.0).abs() < 1e-9);
        assert!((stats.max_ms - 500.0).abs() < 1e-9);
        assert_eq!(stats.sample_count, 5);
    }

    #[test]
    fn test_duration_stats_std_dev() {
        let mut durations = vec![42.0_f64; 5];
        let stats = compute_duration_statistics(&mut durations);
        assert!(stats.std_dev_ms.abs() < 1e-9);
    }

    #[test]
    fn test_build_result_no_records_is_not_successful() {
        let fixture = ScrapeFixture {
            id: "f1".to_owned(),
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
        };

        let result = build_result(
            &fixture,
            "test-adapter",
            &[],
            None,
            &zeroed_resource_metrics(),
            ExecutionMode::Cached,
            false,
        );

        assert!(!result.success);
        assert_eq!(result.error_kind, ErrorKind::None);
        assert!(result.statistics.is_none());
    }

    #[test]
    fn test_build_result_single_success() {
        let fixture = ScrapeFixture {
            id: "f2".to_owned(),
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
        };

        let records = vec![IterationResult {
            iteration: 0,
            duration_ms: 123.0,
            success: true,
            error: None,
            memory_bytes: 0,
        }];

        let result = build_result(
            &fixture,
            "test-adapter",
            &records,
            None,
            &zeroed_resource_metrics(),
            ExecutionMode::Cached,
            false,
        );

        assert!(result.success);
        assert!((result.duration_ms - 123.0).abs() < 1e-9);
        assert!(result.statistics.is_some());
        let stats = result.statistics.unwrap();
        assert_eq!(stats.sample_count, 1);
        assert!((stats.mean_ms - 123.0).abs() < 1e-9);
    }

    #[test]
    fn test_build_result_error_classification() {
        let fixture = ScrapeFixture {
            id: "f3".to_owned(),
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
        };

        let records = vec![IterationResult {
            iteration: 0,
            duration_ms: 30_000.0,
            success: false,
            error: Some("scrape timed out after 30s".to_owned()),
            memory_bytes: 0,
        }];

        let result = build_result(
            &fixture,
            "test-adapter",
            &records,
            None,
            &zeroed_resource_metrics(),
            ExecutionMode::Live,
            false,
        );

        assert!(!result.success);
        assert_eq!(result.error_kind, ErrorKind::Timeout);
    }
}
