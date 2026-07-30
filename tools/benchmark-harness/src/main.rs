// See lib.rs: unpublished dev tool, stdout/stderr are its output channel.
#![allow(clippy::print_stdout, clippy::print_stderr)]
//! Benchmark harness CLI entry point.

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use benchmark_harness::config::BenchmarkConfig;
use benchmark_harness::types::ExecutionMode;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "benchmark-harness")]
#[command(about = "Benchmark and quality-evaluation harness for crawlberg")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Execution mode selectable from the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliExecutionMode {
    /// Fetch pages live from the network.
    Live,
    /// Serve pages from a pre-populated local HTML cache.
    Cached,
}

/// Browser backend selectable from the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliBrowserBackend {
    /// Use direct HTTP fetching without browser rendering.
    Http,
    /// Use the existing Chromium/CDP backend.
    Chromiumoxide,
    /// Use the native V8 browser backend.
    Native,
}

#[derive(Subcommand)]
enum Commands {
    /// Download a scrape-evals dataset to a local directory.
    Download {
        /// Dataset name or URL to download.
        #[arg(short, long)]
        dataset: String,

        /// Directory to store the downloaded fixtures.
        #[arg(short, long, default_value = "fixtures")]
        output: PathBuf,

        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },

    /// Run benchmarks against a set of fixtures.
    Run {
        /// Path to the fixtures file or directory.
        #[arg(short, long)]
        fixtures: PathBuf,

        /// Execution mode: live or cached.
        #[arg(short = 'm', long, default_value = "cached")]
        mode: CliExecutionMode,

        /// Browser backend to use for scraping.
        #[arg(long, default_value = "chromiumoxide")]
        browser_backend: CliBrowserBackend,

        /// Frameworks to benchmark (may be specified multiple times).
        #[arg(short = 'F', long, default_values = ["crawlberg-native"])]
        frameworks: Vec<String>,

        /// Directory to write result JSON files.
        #[arg(short, long, default_value = "results")]
        output: PathBuf,

        /// Maximum number of concurrent scrape workers.
        #[arg(long, default_value = "10")]
        max_concurrent: usize,

        /// Minimum gap between requests to the same host, in milliseconds.
        #[arg(long, default_value = "200")]
        rate_limit_ms: u64,

        /// Per-request timeout in seconds.
        #[arg(long, default_value = "30")]
        timeout: u64,

        /// Number of warmup iterations before recording measurements.
        #[arg(long, default_value = "1")]
        warmup: usize,

        /// Number of timed benchmark iterations per URL.
        #[arg(long, default_value = "3")]
        iterations: usize,

        /// Compute quality metrics against fixture ground truth.
        #[arg(long, default_value = "true")]
        measure_quality: bool,

        /// Persist fetched HTML pages to the cache directory.
        #[arg(long)]
        save_cache: bool,

        /// Directory used for the HTML page cache.
        #[arg(long, default_value = ".benchmark-cache")]
        cache_dir: PathBuf,

        /// Shard specification in the form `INDEX/TOTAL` (e.g. `0/4`).
        #[arg(long)]
        shard: Option<String>,

        /// Regex filter applied to fixture IDs.
        #[arg(long)]
        filter: Option<String>,

        /// Configuration preset: "default" (CrawlConfig defaults) or "quality" (optimized for extraction quality).
        #[arg(long, default_value = "default")]
        preset: String,
    },

    /// Profile CPU or memory usage during scraping.
    Profile {
        /// Path to the fixtures file or directory.
        #[arg(short, long)]
        fixtures: PathBuf,

        /// Directory to write flamegraph SVGs and profile data.
        #[arg(short, long, default_value = "profiles")]
        output: PathBuf,

        /// Execution mode: live or cached.
        #[arg(short = 'm', long, default_value = "cached")]
        mode: CliExecutionMode,

        /// Browser backend to use for scraping.
        #[arg(long, default_value = "chromiumoxide")]
        browser_backend: CliBrowserBackend,

        /// Number of URLs to scrape during the profiling session.
        #[arg(long, default_value = "50")]
        sample_size: usize,

        /// CPU sampling frequency in Hz.
        #[arg(long, default_value = "1000")]
        frequency: i32,
    },

    /// Generate a comparison report from multiple benchmark result files.
    Report {
        /// One or more result JSON files to include in the report.
        #[arg(required = true, num_args = 1..)]
        inputs: Vec<PathBuf>,

        /// Directory to write the generated report.
        #[arg(short, long, default_value = "report")]
        output: PathBuf,

        /// Framework name to treat as the baseline for comparisons.
        #[arg(short, long, default_value = "crawlberg-native")]
        baseline: String,
    },

    /// Validate that all fixtures in a file or directory are well-formed.
    Validate {
        /// Path to the fixtures file or directory.
        #[arg(short, long)]
        fixtures: PathBuf,
    },

    /// List cached pages stored in the HTML cache directory.
    ListCache {
        /// Cache directory to inspect.
        #[arg(long, default_value = ".benchmark-cache")]
        cache_dir: PathBuf,
    },
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Download { dataset, output, force } => cmd_download(dataset, output, force),
        Commands::Run {
            fixtures,
            mode,
            browser_backend,
            frameworks,
            output,
            max_concurrent,
            rate_limit_ms,
            timeout,
            warmup,
            iterations,
            measure_quality,
            save_cache,
            cache_dir,
            shard,
            filter,
            preset,
        } => cmd_run(
            fixtures,
            mode,
            browser_backend,
            frameworks,
            output,
            max_concurrent,
            rate_limit_ms,
            timeout,
            warmup,
            iterations,
            measure_quality,
            save_cache,
            cache_dir,
            shard,
            filter,
            preset,
        ),
        Commands::Profile {
            fixtures,
            output,
            mode,
            browser_backend,
            sample_size,
            frequency,
        } => cmd_profile(fixtures, output, mode, browser_backend, sample_size, frequency),
        Commands::Report {
            inputs,
            output,
            baseline,
        } => cmd_report(inputs, output, baseline),
        Commands::Validate { fixtures } => cmd_validate(fixtures),
        Commands::ListCache { cache_dir } => cmd_list_cache(cache_dir),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn quality_crawl_config() -> crawlberg::CrawlConfig {
    crawlberg::CrawlConfig {
        content: crawlberg::ContentConfig {
            output_format: "plain".to_owned(),
            preprocessing_preset: "aggressive".to_owned(),
            ..Default::default()
        },
        retry_count: 1,
        retry_codes: vec![429, 500, 502, 503, 504],
        cookies_enabled: true,
        browser: crawlberg::BrowserConfig {
            mode: crawlberg::BrowserMode::Auto,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn crawl_config_for_backend(
    preset: &str,
    browser_backend: CliBrowserBackend,
    max_concurrent: usize,
) -> benchmark_harness::Result<crawlberg::CrawlConfig> {
    let mut crawl_config = match preset {
        "quality" => quality_crawl_config(),
        "default" => crawlberg::CrawlConfig::default(),
        other => {
            return Err(benchmark_harness::Error::Config(format!(
                "unknown preset '{other}', expected 'default' or 'quality'"
            )));
        }
    };

    crawl_config.max_concurrent = Some(max_concurrent);

    match browser_backend {
        CliBrowserBackend::Chromiumoxide => {
            crawl_config.browser.backend = crawlberg::BrowserBackend::Chromiumoxide;
        }
        CliBrowserBackend::Native => {
            configure_native_browser_backend(&mut crawl_config)?;
        }
        CliBrowserBackend::Http => {
            crawl_config.browser.mode = crawlberg::BrowserMode::Never;
        }
    }

    Ok(crawl_config)
}

#[cfg(feature = "native-browser")]
fn configure_native_browser_backend(crawl_config: &mut crawlberg::CrawlConfig) -> benchmark_harness::Result<()> {
    crawl_config.browser.backend = crawlberg::BrowserBackend::Native;
    crawl_config.browser.mode = crawlberg::BrowserMode::Always;
    Ok(())
}

#[cfg(not(feature = "native-browser"))]
fn configure_native_browser_backend(_crawl_config: &mut crawlberg::CrawlConfig) -> benchmark_harness::Result<()> {
    Err(benchmark_harness::Error::Config(
        "browser-backend=native requires the benchmark-harness native-browser feature".to_string(),
    ))
}

fn cmd_download(_dataset: String, output: PathBuf, force: bool) -> benchmark_harness::Result<()> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| benchmark_harness::Error::Config(format!("failed to create tokio runtime: {e}")))?;
    let count = rt.block_on(benchmark_harness::dataset::download_scrape_evals(&output, force))?;
    eprintln!("Downloaded {count} fixtures to {}", output.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_run(
    fixtures: PathBuf,
    mode: CliExecutionMode,
    browser_backend: CliBrowserBackend,
    frameworks: Vec<String>,
    output: PathBuf,
    max_concurrent: usize,
    rate_limit_ms: u64,
    timeout: u64,
    warmup: usize,
    iterations: usize,
    measure_quality: bool,
    save_cache: bool,
    cache_dir: PathBuf,
    shard: Option<String>,
    filter: Option<String>,
    preset: String,
) -> benchmark_harness::Result<()> {
    let execution_mode = match mode {
        CliExecutionMode::Live => ExecutionMode::Live,
        CliExecutionMode::Cached => ExecutionMode::Cached,
    };

    for framework in &frameworks {
        if framework != "crawlberg-native" {
            tracing::warn!(
                framework = %framework,
                "unknown framework; only 'crawlberg-native' is supported — ignoring"
            );
        }
    }

    let parsed_shard = shard.as_deref().map(parse_shard).transpose()?;

    let dataset_name = fixtures.file_stem().and_then(|s| s.to_str()).map(str::to_owned);

    let config = BenchmarkConfig {
        output_dir: output.clone(),
        execution_mode,
        max_concurrent,
        rate_limit_ms,
        timeout: Duration::from_secs(timeout),
        warmup_iterations: warmup,
        benchmark_iterations: iterations,
        measure_quality,
        save_cache,
        cache_dir: cache_dir.clone(),
        shard: parsed_shard,
        filter: filter.clone(),
        dataset_name,
    };

    let mut fm = benchmark_harness::fixture::FixtureManager::new();
    if fixtures.is_dir() {
        fm.load_directory(&fixtures)?;
    } else {
        fm.load(&fixtures)?;
    }

    if let Some(ref f) = filter {
        fm.filter_url(f)?;
    }
    if let Some((idx, total)) = parsed_shard {
        fm.apply_shard(idx, total);
    }

    if fm.is_empty() {
        return Err(benchmark_harness::Error::Fixture(
            "no fixtures loaded after filtering".to_string(),
        ));
    }

    eprintln!("Loaded {} fixtures", fm.len());

    let cache = if execution_mode == ExecutionMode::Cached || save_cache {
        Some(benchmark_harness::cache::HtmlCache::open(&cache_dir)?)
    } else {
        None
    };

    let crawl_config = crawl_config_for_backend(&preset, browser_backend, max_concurrent)?;
    let adapter: Arc<dyn benchmark_harness::adapter::ScrapeAdapter> = Arc::new(
        benchmark_harness::adapters::native::NativeAdapter::with_config(crawl_config)?,
    );

    let fixture_entries = fm.entries().to_vec();
    let mut runner = benchmark_harness::runner::BenchmarkRunner::new(
        config.clone(),
        adapter.clone(),
        fixture_entries.clone(),
        cache,
    );

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| benchmark_harness::Error::Config(format!("failed to create tokio runtime: {e}")))?;
    let (results, fixture_outputs) = rt.block_on(runner.run())?;

    let output_data = benchmark_harness::output::aggregate_results(&results, &fixture_entries, &config, adapter.name());
    benchmark_harness::output::write_results(&output, &output_data)?;
    benchmark_harness::output::write_fixture_outputs(&output, &results, &fixture_outputs, &fixture_entries)?;
    benchmark_harness::output::print_summary(&output_data);

    eprintln!("Results written to {}", output.join("results.json").display());
    eprintln!("Fixture outputs written to {}", output.join("fixtures").display());
    Ok(())
}

fn cmd_profile(
    fixtures: PathBuf,
    output: PathBuf,
    mode: CliExecutionMode,
    browser_backend: CliBrowserBackend,
    sample_size: usize,
    frequency: i32,
) -> benchmark_harness::Result<()> {
    let execution_mode = match mode {
        CliExecutionMode::Live => ExecutionMode::Live,
        CliExecutionMode::Cached => ExecutionMode::Cached,
    };

    let config = BenchmarkConfig {
        output_dir: output.clone(),
        execution_mode,
        max_concurrent: 1,
        warmup_iterations: 0,
        benchmark_iterations: 1,
        measure_quality: false,
        ..BenchmarkConfig::default()
    };

    let mut fm = benchmark_harness::fixture::FixtureManager::new();
    if fixtures.is_dir() {
        fm.load_directory(&fixtures)?;
    } else {
        fm.load(&fixtures)?;
    }

    let fixture_entries: Vec<_> = fm.entries().iter().take(sample_size).cloned().collect();
    if fixture_entries.is_empty() {
        return Err(benchmark_harness::Error::Fixture(
            "no fixtures loaded for profiling".to_string(),
        ));
    }

    eprintln!("Profiling {} fixtures at {} Hz", fixture_entries.len(), frequency);

    let crawl_config = crawl_config_for_backend("default", browser_backend, config.max_concurrent)?;
    let adapter: Arc<dyn benchmark_harness::adapter::ScrapeAdapter> = Arc::new(
        benchmark_harness::adapters::native::NativeAdapter::with_config(crawl_config)?,
    );

    std::fs::create_dir_all(&output)?;
    let flamegraph_path = output.join("flamegraph.svg");
    let guard = benchmark_harness::profiling::ProfileGuard::start(frequency, &flamegraph_path)?;

    let mut runner = benchmark_harness::runner::BenchmarkRunner::new(config, adapter, fixture_entries, None);

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| benchmark_harness::Error::Config(format!("failed to create tokio runtime: {e}")))?;
    let (_results, _fixture_outputs) = rt.block_on(runner.run())?;

    guard.finish()?;

    eprintln!("Profile written to {}", flamegraph_path.display());
    Ok(())
}

fn cmd_report(inputs: Vec<PathBuf>, output: PathBuf, baseline: String) -> benchmark_harness::Result<()> {
    eprintln!("Loading results from {} file(s)...", inputs.len());

    let mut outputs: Vec<benchmark_harness::BenchmarkOutput> = Vec::with_capacity(inputs.len());
    for path in &inputs {
        let content = std::fs::read_to_string(path)?;
        let data: benchmark_harness::BenchmarkOutput = serde_json::from_str(&content)?;
        outputs.push(data);
    }

    std::fs::create_dir_all(&output)?;

    let all_results: Vec<_> = outputs.iter().flat_map(|o| o.results.clone()).collect();
    let config = BenchmarkConfig {
        output_dir: output.clone(),
        ..BenchmarkConfig::default()
    };
    let merged = benchmark_harness::output::aggregate_results(&all_results, &[], &config, "merged");
    benchmark_harness::output::write_results(&output, &merged)?;
    benchmark_harness::output::print_summary(&merged);

    if outputs.len() >= 2 {
        let baseline_output = outputs.iter().find(|o| o.metadata.framework == baseline);

        match baseline_output {
            None => {
                tracing::warn!(
                    baseline = %baseline,
                    "no input file matches baseline framework name; skipping comparisons"
                );
            }
            Some(base) => {
                for candidate in outputs.iter().filter(|o| o.metadata.framework != baseline) {
                    let report = benchmark_harness::output::compare_results(
                        &base.results,
                        &candidate.results,
                        &base.metadata.framework,
                        &candidate.metadata.framework,
                    );
                    benchmark_harness::output::print_comparison(&report);

                    let cand_name = candidate.metadata.framework.replace(['/', '\\', ' '], "_");
                    let comparison_path = output.join(format!("comparison_{cand_name}.json"));
                    let json = serde_json::to_string_pretty(&report)?;
                    std::fs::write(&comparison_path, json)?;
                    eprintln!("Comparison written to {}", comparison_path.display());
                }
            }
        }
    }

    eprintln!("Report written to {}", output.join("results.json").display());
    Ok(())
}

fn cmd_validate(fixtures: PathBuf) -> benchmark_harness::Result<()> {
    let mut fm = benchmark_harness::fixture::FixtureManager::new();
    let count = if fixtures.is_dir() {
        fm.load_directory(&fixtures)?
    } else {
        fm.load(&fixtures)?
    };

    let with_truth = fm.entries().iter().filter(|f| f.truth_text.is_some()).count();
    let with_lie = fm.entries().iter().filter(|f| f.lie_text.is_some()).count();
    let with_error = fm.entries().iter().filter(|f| f.error.is_some()).count();

    eprintln!("Validated {count} fixtures");
    eprintln!("  with truth_text: {with_truth}");
    eprintln!("  with lie_text:   {with_lie}");
    eprintln!("  with error:      {with_error}");
    Ok(())
}

fn cmd_list_cache(cache_dir: PathBuf) -> benchmark_harness::Result<()> {
    let cache = benchmark_harness::cache::HtmlCache::open(&cache_dir)?;
    eprintln!("Cache at: {}", cache_dir.display());
    eprintln!("Entries:  {}", cache.len());
    Ok(())
}

/// Parse a shard specification like `0/4` into (index, total).
fn parse_shard(s: &str) -> benchmark_harness::Result<(usize, usize)> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        return Err(benchmark_harness::Error::Config(format!(
            "invalid shard format '{s}', expected INDEX/TOTAL"
        )));
    }
    let index: usize = parts[0]
        .parse()
        .map_err(|_| benchmark_harness::Error::Config(format!("invalid shard index '{}'", parts[0])))?;
    let total: usize = parts[1]
        .parse()
        .map_err(|_| benchmark_harness::Error::Config(format!("invalid shard total '{}'", parts[1])))?;
    if index >= total {
        return Err(benchmark_harness::Error::Config(format!(
            "shard index {index} must be less than total {total}"
        )));
    }
    Ok((index, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "native-browser")]
    #[test]
    fn native_browser_backend_uses_native_always_and_concurrency() {
        let config = crawl_config_for_backend("default", CliBrowserBackend::Native, 7).unwrap();

        assert_eq!(config.browser.backend, crawlberg::BrowserBackend::Native);
        assert_eq!(config.browser.mode, crawlberg::BrowserMode::Always);
        assert_eq!(config.max_concurrent, Some(7));
    }

    #[cfg(not(feature = "native-browser"))]
    #[test]
    fn native_browser_backend_requires_native_browser_feature() {
        let error = crawl_config_for_backend("default", CliBrowserBackend::Native, 7)
            .expect_err("native backend should require the native-browser feature");

        assert!(
            error.to_string().contains("native-browser"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn http_browser_backend_disables_browser() {
        let config = crawl_config_for_backend("default", CliBrowserBackend::Http, 3).unwrap();

        assert_eq!(config.browser.mode, crawlberg::BrowserMode::Never);
        assert_eq!(config.max_concurrent, Some(3));
    }

    #[test]
    fn chromiumoxide_browser_backend_keeps_existing_mode() {
        let config = crawl_config_for_backend("quality", CliBrowserBackend::Chromiumoxide, 5).unwrap();

        assert_eq!(config.browser.backend, crawlberg::BrowserBackend::Chromiumoxide);
        assert_eq!(config.browser.mode, crawlberg::BrowserMode::Auto);
        assert_eq!(config.max_concurrent, Some(5));
    }
}
