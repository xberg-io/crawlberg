//! Shared result/quality builders for the `output` module's submodule tests.

use crate::types::{
    ErrorKind, ExecutionMode, IterationResult, PerformanceMetrics, ScrapeBenchmarkResult, ScrapeQualityMetrics,
};

pub(crate) fn make_result(
    success: bool,
    duration_ms: f64,
    quality: Option<ScrapeQualityMetrics>,
) -> ScrapeBenchmarkResult {
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

pub(crate) fn make_quality(f1_text: f64, noise_penalty: f64, quality_score: f64) -> ScrapeQualityMetrics {
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

/// Build a result with an explicit fixture_id, framework, and memory.
pub(crate) fn make_result_named(
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
