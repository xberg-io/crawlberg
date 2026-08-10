use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use crawlberg::{
    BatchCrawlResults, BatchScrapeResults, BrowserConfig, BrowserMode, CrawlConfig, PageAction, ProxyConfig,
    batch_crawl, batch_scrape, crawl, create_engine, generate_citations, interact, map_urls, scrape,
};

mod telemetry;
use telemetry::{LogConfig, LogFormat};

/// Log level selectable via `--log-level`, independent of `-v`/`-q`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliLogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl CliLogLevel {
    fn as_directive(self) -> &'static str {
        match self {
            CliLogLevel::Off => "off",
            CliLogLevel::Error => "error",
            CliLogLevel::Warn => "warn",
            CliLogLevel::Info => "info",
            CliLogLevel::Debug => "debug",
            CliLogLevel::Trace => "trace",
        }
    }
}

/// Log output format selectable via `--log-format`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CliLogFormat {
    #[default]
    Pretty,
    Compact,
    Json,
}

impl From<CliLogFormat> for LogFormat {
    fn from(value: CliLogFormat) -> Self {
        match value {
            CliLogFormat::Pretty => LogFormat::Pretty,
            CliLogFormat::Compact => LogFormat::Compact,
            CliLogFormat::Json => LogFormat::Json,
        }
    }
}

/// Derive the `tracing` `EnvFilter` directives string from `--log-level`, the
/// `-v`/`--verbose` count, and `-q`/`--quiet`.
///
/// `--log-level` takes precedence when given. Otherwise `--quiet` forces
/// `error`, and `--verbose` escalates from the default `warn` through `info`,
/// `debug`, and `trace`. The default `warn` level also enables `info` for the
/// CLI's own lifecycle/progress messages, since the library itself stays at
/// `warn` by default.
fn log_directives(log_level: Option<CliLogLevel>, verbose: u8, quiet: bool) -> String {
    let base = if let Some(level) = log_level {
        level.as_directive()
    } else if quiet {
        "error"
    } else {
        match verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };

    if base == "warn" {
        "warn,crawlberg_cli=info".to_owned()
    } else {
        base.to_owned()
    }
}

/// Write a line of program output to stdout — the CLI's data channel. Diagnostics
/// go through `tracing` (stderr); results go here. Exempt from `print_stdout`.
#[expect(clippy::print_stdout, reason = "stdout is the CLI's data output channel")]
fn emit(text: &str) {
    println!("{text}");
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliBrowserMode {
    Auto,
    Always,
    Never,
}

impl From<CliBrowserMode> for BrowserMode {
    fn from(value: CliBrowserMode) -> Self {
        match value {
            CliBrowserMode::Auto => BrowserMode::Auto,
            CliBrowserMode::Always => BrowserMode::Always,
            CliBrowserMode::Never => BrowserMode::Never,
        }
    }
}

/// Validate that a `--browser-endpoint` value is a WebSocket URL (`ws://` or `wss://`).
fn parse_browser_endpoint(value: &str) -> Result<String, String> {
    if value.starts_with("ws://") || value.starts_with("wss://") {
        Ok(value.to_owned())
    } else {
        Err(format!(
            "browser endpoint must be a WebSocket URL starting with ws:// or wss://, got: {value:?}"
        ))
    }
}

fn build_browser_config(
    browser_mode: CliBrowserMode,
    browser_endpoint: Option<String>,
    timeout: Duration,
) -> BrowserConfig {
    BrowserConfig {
        mode: browser_mode.into(),
        endpoint: browser_endpoint,
        timeout,
        ..Default::default()
    }
}

/// Merge a JSON config string (or @file.json reference) into a CrawlConfig.
/// JSON values override defaults but do not override CLI flags that were explicitly set.
fn merge_json_config(config: &mut CrawlConfig, config_str: &str) -> Result<(), Box<dyn std::error::Error>> {
    let json_text = if let Some(path) = config_str.strip_prefix('@') {
        std::fs::read_to_string(path)?
    } else {
        config_str.to_string()
    };

    let json: serde_json::Value = serde_json::from_str(&json_text)?;

    let partial: CrawlConfig = serde_json::from_value(json)?;

    let mut config_json = serde_json::to_value(config.clone())?;
    let partial_json = serde_json::to_value(partial)?;

    if let (serde_json::Value::Object(config_map), serde_json::Value::Object(partial_map)) =
        (&mut config_json, partial_json)
    {
        for (k, v) in partial_map {
            if !v.is_null() {
                config_map.insert(k, v);
            }
        }
    }

    *config = serde_json::from_value(config_json)?;
    Ok(())
}

/// Print a batch scrape as markdown, or the full `BatchScrapeResults` object as JSON.
///
/// The JSON form serializes the aggregate result (`results`, `total_count`,
/// `completed_count`, `failed_count`) so callers match the binding/MCP shape.
fn print_batch_scrape(results: &BatchScrapeResults, format: &str) {
    if format == "markdown" {
        for entry in &results.results {
            if let Some(ref r) = entry.result
                && let Some(ref md) = r.markdown
            {
                emit(&format!("---\nURL: {}\n---\n{}\n", entry.url, md.content));
            }
            if let Some(ref e) = entry.error {
                tracing::error!(url = %entry.url, error = %e, "scrape failed");
            }
        }
    } else {
        emit(&serde_json::to_string_pretty(results).expect("results are serializable"));
    }
}

/// Print a batch crawl as markdown, or the full `BatchCrawlResults` object as JSON.
///
/// The JSON form serializes the aggregate result (`results`, `total_count`,
/// `completed_count`, `failed_count`) so callers match the binding/MCP shape.
fn print_batch_crawl(results: &BatchCrawlResults, format: &str) {
    if format == "markdown" {
        for entry in &results.results {
            if let Some(ref r) = entry.result {
                for page in &r.pages {
                    if let Some(ref md) = page.markdown {
                        emit(&format!(
                            "---\nSeed: {}\nURL: {}\n---\n{}\n",
                            entry.url, page.url, md.content
                        ));
                    }
                }
            }
            if let Some(ref e) = entry.error {
                tracing::error!(url = %entry.url, error = %e, "crawl failed");
            }
        }
    } else {
        emit(&serde_json::to_string_pretty(results).expect("results are serializable"));
    }
}

#[derive(Parser)]
#[command(name = "crawlberg", about = "High-performance web crawler and scraper", version)]
struct Cli {
    /// Log level: off, error, warn, info, debug, or trace (overrides -v/-q)
    #[arg(long, global = true, value_enum)]
    log_level: Option<CliLogLevel>,
    /// Log output format
    #[arg(long, global = true, value_enum, default_value_t = CliLogFormat::Pretty)]
    log_format: CliLogFormat,
    /// Increase log verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Suppress diagnostics below error level
    #[arg(short = 'q', long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scrape a single URL and extract metadata
    Scrape {
        /// URL to scrape
        url: String,
        /// Output format: json or markdown
        #[arg(long, default_value = "json")]
        format: String,
        /// Proxy URL
        #[arg(long)]
        proxy: Option<String>,
        /// Custom user agent
        #[arg(long)]
        user_agent: Option<String>,
        /// Request timeout in milliseconds
        #[arg(long, default_value = "30000")]
        timeout: u64,
        /// Respect robots.txt
        #[arg(long)]
        respect_robots_txt: bool,
        /// When to use the browser: auto, always, or never
        #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
        browser_mode: CliBrowserMode,
        /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
        #[arg(long, value_parser = parse_browser_endpoint)]
        browser_endpoint: Option<String>,
        /// Configuration as JSON string or @file.json
        #[arg(long, value_name = "JSON")]
        config: Option<String>,
    },
    /// Crawl a website following links
    Crawl {
        /// Seed URL(s) to crawl
        #[arg(required = true)]
        urls: Vec<String>,
        /// Maximum crawl depth
        #[arg(long, short = 'd', default_value = "2")]
        depth: usize,
        /// Maximum pages to crawl
        #[arg(long, short = 'n')]
        max_pages: Option<usize>,
        /// Maximum concurrent requests
        #[arg(long, short = 'c', default_value = "10")]
        concurrent: usize,
        /// Rate limit delay in milliseconds
        #[arg(long, default_value = "200")]
        rate_limit: u64,
        /// Output format: json or markdown
        #[arg(long, default_value = "json")]
        format: String,
        /// Proxy URL
        #[arg(long)]
        proxy: Option<String>,
        /// Custom user agent
        #[arg(long)]
        user_agent: Option<String>,
        /// Request timeout in milliseconds
        #[arg(long, default_value = "30000")]
        timeout: u64,
        /// Respect robots.txt
        #[arg(long)]
        respect_robots_txt: bool,
        /// Stay on the same domain
        #[arg(long)]
        stay_on_domain: bool,
        /// When to use the browser: auto, always, or never
        #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
        browser_mode: CliBrowserMode,
        /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
        #[arg(long, value_parser = parse_browser_endpoint)]
        browser_endpoint: Option<String>,
        /// Configuration as JSON string or @file.json
        #[arg(long, value_name = "JSON")]
        config: Option<String>,
    },
    /// Discover all URLs on a website via sitemaps and link extraction
    Map {
        /// URL to map
        url: String,
        /// Maximum URLs to return
        #[arg(long)]
        limit: Option<usize>,
        /// Filter URLs by substring
        #[arg(long)]
        search: Option<String>,
        /// Respect robots.txt
        #[arg(long)]
        respect_robots_txt: bool,
        /// Output format: json or markdown
        #[arg(long, default_value = "json")]
        format: String,
        /// Request timeout in milliseconds
        #[arg(long, default_value = "30000")]
        timeout: u64,
        /// When to use the browser: auto, always, or never
        #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
        browser_mode: CliBrowserMode,
        /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
        #[arg(long, value_parser = parse_browser_endpoint)]
        browser_endpoint: Option<String>,
        /// Configuration as JSON string or @file.json
        #[arg(long, value_name = "JSON")]
        config: Option<String>,
    },
    /// Execute browser actions on a single page
    Interact {
        /// URL to interact with
        url: String,
        /// Actions as JSON array (e.g. '[{"type":"click","selector":"#submit"}]')
        #[arg(long, value_name = "JSON")]
        actions: String,
        /// Output format: json or markdown
        #[arg(long, default_value = "json")]
        format: String,
        /// Request timeout in milliseconds
        #[arg(long, default_value = "30000")]
        timeout: u64,
        /// When to use the browser: auto, always, or never
        #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
        browser_mode: CliBrowserMode,
        /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
        #[arg(long, value_parser = parse_browser_endpoint)]
        browser_endpoint: Option<String>,
        /// Configuration as JSON string or @file.json
        #[arg(long, value_name = "JSON")]
        config: Option<String>,
    },
    /// Scrape multiple URLs concurrently
    BatchScrape {
        /// URLs to scrape
        #[arg(required = true)]
        urls: Vec<String>,
        /// Maximum concurrent requests
        #[arg(long, short = 'c', default_value = "10")]
        concurrent: usize,
        /// Output format: json or markdown
        #[arg(long, default_value = "json")]
        format: String,
        /// Proxy URL
        #[arg(long)]
        proxy: Option<String>,
        /// Custom user agent
        #[arg(long)]
        user_agent: Option<String>,
        /// Request timeout in milliseconds
        #[arg(long, default_value = "30000")]
        timeout: u64,
        /// Respect robots.txt
        #[arg(long)]
        respect_robots_txt: bool,
        /// When to use the browser: auto, always, or never
        #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
        browser_mode: CliBrowserMode,
        /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
        #[arg(long, value_parser = parse_browser_endpoint)]
        browser_endpoint: Option<String>,
        /// Configuration as JSON string or @file.json
        #[arg(long, value_name = "JSON")]
        config: Option<String>,
    },
    /// Crawl multiple websites concurrently
    BatchCrawl {
        /// Seed URLs to crawl
        #[arg(required = true)]
        urls: Vec<String>,
        /// Maximum crawl depth
        #[arg(long, short = 'd', default_value = "2")]
        depth: usize,
        /// Maximum pages to crawl per seed
        #[arg(long, short = 'n')]
        max_pages: Option<usize>,
        /// Maximum concurrent requests
        #[arg(long, short = 'c', default_value = "10")]
        concurrent: usize,
        /// Rate limit delay in milliseconds
        #[arg(long, default_value = "200")]
        rate_limit: u64,
        /// Output format: json or markdown
        #[arg(long, default_value = "json")]
        format: String,
        /// Proxy URL
        #[arg(long)]
        proxy: Option<String>,
        /// Custom user agent
        #[arg(long)]
        user_agent: Option<String>,
        /// Request timeout in milliseconds
        #[arg(long, default_value = "30000")]
        timeout: u64,
        /// Respect robots.txt
        #[arg(long)]
        respect_robots_txt: bool,
        /// Stay on the same domain
        #[arg(long)]
        stay_on_domain: bool,
        /// When to use the browser: auto, always, or never
        #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
        browser_mode: CliBrowserMode,
        /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
        #[arg(long, value_parser = parse_browser_endpoint)]
        browser_endpoint: Option<String>,
        /// Configuration as JSON string or @file.json
        #[arg(long, value_name = "JSON")]
        config: Option<String>,
    },
    /// Download a document from a URL and report its metadata
    Download {
        /// URL to download
        url: String,
        /// Maximum document size in bytes
        #[arg(long)]
        max_size: Option<usize>,
        /// Request timeout in milliseconds
        #[arg(long, default_value = "30000")]
        timeout: u64,
        /// When to use the browser: auto, always, or never
        #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
        browser_mode: CliBrowserMode,
        /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
        #[arg(long, value_parser = parse_browser_endpoint)]
        browser_endpoint: Option<String>,
        /// Configuration as JSON string or @file.json
        #[arg(long, value_name = "JSON")]
        config: Option<String>,
    },
    /// Convert markdown links into numbered citations
    Citations {
        /// Markdown text, or @file.md to read from a file
        input: String,
    },
    /// Print the crawlberg version as JSON
    Version {},
    /// Start the REST API server.
    ///
    /// Unauthenticated by default. Set CRAWLBERG_API_TOKEN to require a bearer
    /// token on every request except /health. Binding a non-loopback --host
    /// (the "0.0.0.0" default included) without a token refuses to start;
    /// set CRAWLBERG_API_ALLOW_INSECURE=1 to explicitly opt out. See also
    /// CRAWLBERG_API_CORS_ORIGINS, CRAWLBERG_API_MAX_PAGES,
    /// CRAWLBERG_API_MAX_BATCH_URLS, and CRAWLBERG_API_MAX_CONCURRENT_JOBS.
    #[cfg(feature = "api")]
    Serve {
        /// Host address to bind to
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
        /// Port to listen on
        #[arg(long, default_value = "3000")]
        port: u16,
    },
    /// Start the MCP server (stdio transport by default).
    ///
    /// In --http mode, the same CRAWLBERG_API_TOKEN / CRAWLBERG_API_ALLOW_INSECURE
    /// env vars documented on `serve` apply: a non-loopback --host without a
    /// token refuses to start.
    #[cfg(feature = "mcp")]
    Mcp {
        /// Serve over Streamable HTTP at `/mcp` instead of stdio
        /// (requires a build with the `mcp-http` feature)
        #[arg(long)]
        http: bool,
        /// Host address to bind to in `--http` mode
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to listen on in `--http` mode
        #[arg(long, default_value = "3001")]
        port: u16,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let log_config = LogConfig {
        directives: log_directives(cli.log_level, cli.verbose, cli.quiet),
        format: cli.log_format.into(),
        env_override: true,
        ..Default::default()
    };
    // ~keep Hold the guard for the whole process so the OTLP providers flush on exit.
    let _telemetry_guard = telemetry::init(&log_config);

    match cli.command {
        Commands::Scrape {
            url,
            format,
            proxy,
            user_agent,
            timeout,
            respect_robots_txt,
            browser_mode,
            browser_endpoint,
            config: config_str,
        } => {
            let timeout_duration = Duration::from_millis(timeout);
            let mut config = CrawlConfig {
                user_agent,
                request_timeout: timeout_duration,
                respect_robots_txt,
                proxy: proxy.map(|url| ProxyConfig {
                    url,
                    username: None,
                    password: None,
                }),
                browser: build_browser_config(browser_mode, browser_endpoint, timeout_duration),
                ..Default::default()
            };

            if let Some(config_json) = config_str
                && let Err(e) = merge_json_config(&mut config, &config_json)
            {
                tracing::error!("invalid config: {e}");
                std::process::exit(1);
            }

            let handle = match create_engine(Some(config)) {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::error!("failed to create crawl engine: {e}");
                    std::process::exit(1);
                }
            };
            match scrape(&handle, &url).await {
                Ok(result) => {
                    if format == "markdown" {
                        if let Some(ref md) = result.markdown {
                            emit(&md.content);
                        } else {
                            tracing::warn!("no markdown content available");
                        }
                    } else {
                        emit(&serde_json::to_string_pretty(&result).expect("result is serializable"));
                    }
                }
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Crawl {
            urls,
            depth,
            max_pages,
            concurrent,
            rate_limit,
            format,
            proxy,
            user_agent,
            timeout,
            respect_robots_txt,
            stay_on_domain,
            browser_mode,
            browser_endpoint,
            config: config_str,
        } => {
            let timeout_duration = Duration::from_millis(timeout);
            let mut config = CrawlConfig {
                max_depth: Some(depth),
                max_pages,
                max_concurrent: Some(concurrent),
                rate_limit_ms: Some(rate_limit),
                user_agent,
                request_timeout: timeout_duration,
                respect_robots_txt,
                stay_on_domain,
                proxy: proxy.map(|url| ProxyConfig {
                    url,
                    username: None,
                    password: None,
                }),
                browser: build_browser_config(browser_mode, browser_endpoint, timeout_duration),
                ..Default::default()
            };

            if let Some(config_json) = config_str
                && let Err(e) = merge_json_config(&mut config, &config_json)
            {
                tracing::error!("invalid config: {e}");
                std::process::exit(1);
            }

            let handle = match create_engine(Some(config)) {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::error!("failed to create crawl engine: {e}");
                    std::process::exit(1);
                }
            };

            if urls.len() == 1 {
                match crawl(&handle, &urls[0]).await {
                    Ok(result) => {
                        if format == "markdown" {
                            for page in &result.pages {
                                if let Some(ref md) = page.markdown {
                                    emit(&format!("---\nURL: {}\n---\n{}\n", page.url, md.content));
                                }
                            }
                        } else {
                            emit(&serde_json::to_string_pretty(&result).expect("result is serializable"));
                        }
                    }
                    Err(e) => {
                        tracing::error!("{e}");
                        std::process::exit(1);
                    }
                }
            } else {
                match batch_crawl(&handle, urls).await {
                    Ok(results) => print_batch_crawl(&results, &format),
                    Err(e) => {
                        tracing::error!("{e}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Commands::Map {
            url,
            limit,
            search,
            respect_robots_txt,
            format,
            timeout,
            browser_mode,
            browser_endpoint,
            config: config_str,
        } => {
            let timeout_duration = Duration::from_millis(timeout);
            let mut config = CrawlConfig {
                respect_robots_txt,
                map_limit: limit,
                map_search: search,
                request_timeout: timeout_duration,
                browser: build_browser_config(browser_mode, browser_endpoint, timeout_duration),
                ..Default::default()
            };

            if let Some(config_json) = config_str
                && let Err(e) = merge_json_config(&mut config, &config_json)
            {
                tracing::error!("invalid config: {e}");
                std::process::exit(1);
            }

            let handle = match create_engine(Some(config)) {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::error!("failed to create crawl engine: {e}");
                    std::process::exit(1);
                }
            };
            match map_urls(&handle, &url).await {
                Ok(result) => {
                    if format == "markdown" {
                        for url_entry in &result.urls {
                            emit(&url_entry.url);
                        }
                    } else {
                        emit(&serde_json::to_string_pretty(&result).expect("result is serializable"));
                    }
                }
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Interact {
            url,
            actions,
            format,
            timeout,
            browser_mode,
            browser_endpoint,
            config: config_str,
        } => {
            let timeout_duration = Duration::from_millis(timeout);
            let mut config = CrawlConfig {
                request_timeout: timeout_duration,
                browser: build_browser_config(browser_mode, browser_endpoint, timeout_duration),
                ..Default::default()
            };

            if let Some(config_json) = config_str
                && let Err(e) = merge_json_config(&mut config, &config_json)
            {
                tracing::error!("invalid config: {e}");
                std::process::exit(1);
            }

            let parsed_actions: Vec<PageAction> = match serde_json::from_str(&actions) {
                Ok(value) => value,
                Err(e) => {
                    tracing::error!("invalid actions JSON: {e}");
                    std::process::exit(1);
                }
            };

            let handle = match create_engine(Some(config)) {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::error!("failed to create crawl engine: {e}");
                    std::process::exit(1);
                }
            };
            match interact(&handle, &url, parsed_actions).await {
                Ok(result) => {
                    if format == "markdown" {
                        emit(&result.final_html);
                    } else {
                        let wrapped = serde_json::json!({ "interaction": result });
                        emit(&serde_json::to_string_pretty(&wrapped).expect("result is serializable"));
                    }
                }
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::BatchScrape {
            urls,
            concurrent,
            format,
            proxy,
            user_agent,
            timeout,
            respect_robots_txt,
            browser_mode,
            browser_endpoint,
            config: config_str,
        } => {
            let timeout_duration = Duration::from_millis(timeout);
            let mut config = CrawlConfig {
                max_concurrent: Some(concurrent),
                user_agent,
                request_timeout: timeout_duration,
                respect_robots_txt,
                proxy: proxy.map(|url| ProxyConfig {
                    url,
                    username: None,
                    password: None,
                }),
                browser: build_browser_config(browser_mode, browser_endpoint, timeout_duration),
                ..Default::default()
            };

            if let Some(config_json) = config_str
                && let Err(e) = merge_json_config(&mut config, &config_json)
            {
                tracing::error!("invalid config: {e}");
                std::process::exit(1);
            }

            let handle = match create_engine(Some(config)) {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::error!("failed to create crawl engine: {e}");
                    std::process::exit(1);
                }
            };
            match batch_scrape(&handle, urls).await {
                Ok(results) => print_batch_scrape(&results, &format),
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::BatchCrawl {
            urls,
            depth,
            max_pages,
            concurrent,
            rate_limit,
            format,
            proxy,
            user_agent,
            timeout,
            respect_robots_txt,
            stay_on_domain,
            browser_mode,
            browser_endpoint,
            config: config_str,
        } => {
            let timeout_duration = Duration::from_millis(timeout);
            let mut config = CrawlConfig {
                max_depth: Some(depth),
                max_pages,
                max_concurrent: Some(concurrent),
                rate_limit_ms: Some(rate_limit),
                user_agent,
                request_timeout: timeout_duration,
                respect_robots_txt,
                stay_on_domain,
                proxy: proxy.map(|url| ProxyConfig {
                    url,
                    username: None,
                    password: None,
                }),
                browser: build_browser_config(browser_mode, browser_endpoint, timeout_duration),
                ..Default::default()
            };

            if let Some(config_json) = config_str
                && let Err(e) = merge_json_config(&mut config, &config_json)
            {
                tracing::error!("invalid config: {e}");
                std::process::exit(1);
            }

            let handle = match create_engine(Some(config)) {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::error!("failed to create crawl engine: {e}");
                    std::process::exit(1);
                }
            };
            match batch_crawl(&handle, urls).await {
                Ok(results) => print_batch_crawl(&results, &format),
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Download {
            url,
            max_size,
            timeout,
            browser_mode,
            browser_endpoint,
            config: config_str,
        } => {
            let timeout_duration = Duration::from_millis(timeout);
            let mut config = CrawlConfig {
                request_timeout: timeout_duration,
                download_documents: true,
                document_max_size: max_size,
                browser: build_browser_config(browser_mode, browser_endpoint, timeout_duration),
                ..Default::default()
            };

            if let Some(config_json) = config_str
                && let Err(e) = merge_json_config(&mut config, &config_json)
            {
                tracing::error!("invalid config: {e}");
                std::process::exit(1);
            }

            let handle = match create_engine(Some(config)) {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::error!("failed to create crawl engine: {e}");
                    std::process::exit(1);
                }
            };
            match scrape(&handle, &url).await {
                Ok(result) => {
                    let output = if let Some(ref doc) = result.downloaded_document {
                        serde_json::json!({
                            "url": doc.url,
                            "mime_type": doc.mime_type,
                            "size": doc.size,
                            "filename": doc.filename,
                            "content_hash": doc.content_hash,
                        })
                    } else {
                        serde_json::json!({
                            "url": url,
                            "content_type": result.content_type,
                            "status_code": result.status_code,
                            "body_size": result.body_size,
                            "note": "URL returned HTML content, not a downloadable document",
                        })
                    };
                    emit(&serde_json::to_string_pretty(&output).expect("output is serializable"));
                }
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Citations { input } => {
            let markdown = if let Some(path) = input.strip_prefix('@') {
                match std::fs::read_to_string(path) {
                    Ok(text) => text,
                    Err(e) => {
                        tracing::error!("cannot read {path}: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                input
            };
            let result = generate_citations(&markdown);
            emit(&serde_json::to_string_pretty(&result).expect("result is serializable"));
        }
        Commands::Version {} => {
            let response = serde_json::json!({ "version": env!("CARGO_PKG_VERSION") });
            emit(&serde_json::to_string_pretty(&response).expect("version is serializable"));
        }
        #[cfg(feature = "api")]
        Commands::Serve { host, port } => {
            tracing::info!("starting REST API server on {host}:{port}");
            #[cfg(feature = "mcp")]
            tracing::info!("MCP Streamable HTTP transport available at http://{host}:{port}/mcp");
            if let Err(e) = crawlberg::serve_api(&host, port, CrawlConfig::default()).await {
                tracing::error!("server error: {e}");
                std::process::exit(1);
            }
        }
        #[cfg(feature = "mcp")]
        Commands::Mcp { http, host, port } => {
            if http {
                #[cfg(feature = "mcp-http")]
                {
                    tracing::info!("starting MCP server (Streamable HTTP) at http://{host}:{port}/mcp");
                    if let Err(e) = crawlberg::start_mcp_http_server(&host, port, CrawlConfig::default()).await {
                        tracing::error!("MCP server error: {e}");
                        std::process::exit(1);
                    }
                }
                #[cfg(not(feature = "mcp-http"))]
                {
                    let _ = (host, port);
                    tracing::error!("--http requires a build with the `mcp-http` feature");
                    std::process::exit(1);
                }
            } else {
                tracing::info!("starting MCP server (stdio transport)");
                if let Err(e) = crawlberg::start_mcp_server().await {
                    tracing::error!("MCP server error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CliBrowserMode, build_browser_config, parse_browser_endpoint};
    use crawlberg::BrowserMode;

    const DEFAULT_TIMEOUT: Duration = Duration::from_millis(30_000);

    #[test]
    fn maps_cli_browser_mode_to_engine_mode() {
        assert_eq!(
            build_browser_config(CliBrowserMode::Auto, None, DEFAULT_TIMEOUT).mode,
            BrowserMode::Auto
        );
        assert_eq!(
            build_browser_config(CliBrowserMode::Always, None, DEFAULT_TIMEOUT).mode,
            BrowserMode::Always
        );
        assert_eq!(
            build_browser_config(CliBrowserMode::Never, None, DEFAULT_TIMEOUT).mode,
            BrowserMode::Never
        );
    }

    #[test]
    fn preserves_browser_endpoint() {
        let endpoint = Some("ws://127.0.0.1:9222/devtools/browser/test".to_string());
        let config = build_browser_config(CliBrowserMode::Auto, endpoint.clone(), DEFAULT_TIMEOUT);
        assert_eq!(config.endpoint, endpoint);
    }

    #[test]
    fn timeout_is_propagated_to_browser_config() {
        let timeout = Duration::from_millis(5_000);
        let config = build_browser_config(CliBrowserMode::Auto, None, timeout);
        assert_eq!(config.timeout, timeout);
    }

    #[test]
    fn parse_browser_endpoint_accepts_ws_urls() {
        assert!(parse_browser_endpoint("ws://127.0.0.1:9222/devtools/browser/abc").is_ok());
        assert!(parse_browser_endpoint("wss://remote.host/devtools/browser/abc").is_ok());
    }

    #[test]
    fn parse_browser_endpoint_rejects_non_ws_urls() {
        assert!(parse_browser_endpoint("http://127.0.0.1:9222").is_err());
        assert!(parse_browser_endpoint("https://remote.host").is_err());
        assert!(parse_browser_endpoint("127.0.0.1:9222").is_err());
    }

    use clap::Parser;

    use super::{Cli, Commands};

    #[test]
    fn parses_batch_scrape_subcommand() {
        let cli = Cli::try_parse_from(["crawlberg", "batch-scrape", "https://a.com", "https://b.com"]).unwrap();
        match cli.command {
            Commands::BatchScrape { urls, concurrent, .. } => {
                assert_eq!(urls, vec!["https://a.com".to_string(), "https://b.com".to_string()]);
                assert_eq!(concurrent, 10);
            }
            _ => panic!("expected BatchScrape"),
        }
    }

    #[test]
    fn parses_batch_crawl_subcommand_with_depth() {
        let cli = Cli::try_parse_from([
            "crawlberg",
            "batch-crawl",
            "https://a.com",
            "https://b.com",
            "--depth",
            "3",
        ])
        .unwrap();
        match cli.command {
            Commands::BatchCrawl { urls, depth, .. } => {
                assert_eq!(urls.len(), 2);
                assert_eq!(depth, 3);
            }
            _ => panic!("expected BatchCrawl"),
        }
    }

    #[test]
    fn parses_download_subcommand() {
        let cli =
            Cli::try_parse_from(["crawlberg", "download", "https://a.com/doc.pdf", "--max-size", "1024"]).unwrap();
        match cli.command {
            Commands::Download { url, max_size, .. } => {
                assert_eq!(url, "https://a.com/doc.pdf");
                assert_eq!(max_size, Some(1024));
            }
            _ => panic!("expected Download"),
        }
    }

    #[test]
    fn parses_citations_subcommand() {
        let cli = Cli::try_parse_from(["crawlberg", "citations", "@notes.md"]).unwrap();
        match cli.command {
            Commands::Citations { input } => assert_eq!(input, "@notes.md"),
            _ => panic!("expected Citations"),
        }
    }

    #[test]
    fn parses_version_subcommand() {
        let cli = Cli::try_parse_from(["crawlberg", "version"]).unwrap();
        assert!(matches!(cli.command, Commands::Version {}));
    }

    #[test]
    fn batch_scrape_requires_at_least_one_url() {
        assert!(Cli::try_parse_from(["crawlberg", "batch-scrape"]).is_err());
    }
}
