//! Result serialization and human-readable reporting.
//!
//! Writes [`BenchmarkOutput`] to JSON files and prints summary tables to stderr.
//!
//! The implementation is split across private submodules; every public path this module
//! ever exposed is re-exported here unchanged.

mod comparison;
mod metadata;
mod performance;
mod quality;
mod reachability;
mod results;
mod summary;
#[cfg(test)]
pub(crate) mod test_support;

use crate::config::BenchmarkConfig;
use crate::types::{BenchmarkOutput, ScrapeBenchmarkResult, ScrapeFixture};

pub use comparison::{compare_results, print_comparison};
pub use reachability::print_reachability_report;
pub use results::{write_fixture_outputs, write_results};
pub use summary::print_summary;

/// Aggregate individual fixture results into a [`BenchmarkOutput`].
///
/// Computes:
/// - [`crate::types::DatasetQualityReport`] from results that have quality metrics, filtered
///   to exclude expected failures when `fixtures` is non-empty
/// - [`crate::types::DatasetPerformanceReport`] from per-result `duration_ms` and metrics
/// - [`crate::types::BenchmarkMetadata`] from the config and adapter name
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
    let metadata = metadata::build_metadata(results, config, adapter_name);
    let performance_report = performance::build_performance_report(results);
    let quality_report = if config.measure_quality {
        Some(quality::build_quality_report(results, fixtures))
    } else {
        None
    };
    let reachability_report = reachability::build_reachability_report(results, fixtures);

    BenchmarkOutput {
        metadata,
        quality_report,
        performance_report,
        reachability_report,
        results: results.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::{make_quality, make_result};

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
}
