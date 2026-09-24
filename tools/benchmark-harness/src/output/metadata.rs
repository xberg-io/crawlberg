//! Construction of [`BenchmarkMetadata`] from run configuration and results.

use chrono::Utc;

use crate::config::BenchmarkConfig;
use crate::types::{BenchmarkMetadata, ScrapeBenchmarkResult};

/// Build [`BenchmarkMetadata`] from config and results.
pub(crate) fn build_metadata(
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
