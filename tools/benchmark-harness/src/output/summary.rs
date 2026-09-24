//! Human-readable summary printing.

use crate::types::{BenchmarkOutput, ExecutionMode};

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
