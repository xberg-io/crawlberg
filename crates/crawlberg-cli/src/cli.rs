//! Command-line surface: argument parsing, value enums, and log-filter derivation.
//!
//! The flag names, defaults, help text and subcommand names defined here are the
//! CLI's public contract.

use clap::{Args, Parser, Subcommand, ValueEnum};
use crawlberg::BrowserMode;

use crate::telemetry::LogFormat;

/// Log level selectable via `--log-level`, independent of `-v`/`-q`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliLogLevel {
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
pub enum CliLogFormat {
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
pub fn log_directives(log_level: Option<CliLogLevel>, verbose: u8, quiet: bool) -> String {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliBrowserMode {
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
pub fn parse_browser_endpoint(value: &str) -> Result<String, String> {
    if value.starts_with("ws://") || value.starts_with("wss://") {
        Ok(value.to_owned())
    } else {
        Err(format!(
            "browser endpoint must be a WebSocket URL starting with ws:// or wss://, got: {value:?}"
        ))
    }
}

#[derive(Parser)]
#[command(name = "crawlberg", about = "High-performance web crawler and scraper", version)]
pub struct Cli {
    /// Log level: off, error, warn, info, debug, or trace (overrides -v/-q)
    #[arg(long, global = true, value_enum)]
    pub log_level: Option<CliLogLevel>,
    /// Log output format
    #[arg(long, global = true, value_enum, default_value_t = CliLogFormat::Pretty)]
    pub log_format: CliLogFormat,
    /// Increase log verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Suppress diagnostics below error level
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Scrape a single URL and extract metadata
    Scrape(ScrapeArgs),
    /// Crawl a website following links
    Crawl(CrawlArgs),
    /// Discover all URLs on a website via sitemaps and link extraction
    Map(MapArgs),
    /// Execute browser actions on a single page
    Interact(InteractArgs),
    /// Scrape multiple URLs concurrently
    BatchScrape(BatchScrapeArgs),
    /// Crawl multiple websites concurrently
    BatchCrawl(BatchCrawlArgs),
    /// Download a document from a URL and report its metadata
    Download(DownloadArgs),
    /// Convert markdown links into numbered citations
    Citations(CitationsArgs),
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
    Serve(ServeArgs),
    /// Start the MCP server (stdio transport by default).
    ///
    /// In --http mode, the same CRAWLBERG_API_TOKEN / CRAWLBERG_API_ALLOW_INSECURE
    /// env vars documented on `serve` apply: a non-loopback --host without a
    /// token refuses to start.
    #[cfg(feature = "mcp")]
    Mcp(McpArgs),
}

#[derive(Args)]
pub struct ScrapeArgs {
    /// URL to scrape
    pub url: String,
    /// Output format: json or markdown
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Proxy URL
    #[arg(long)]
    pub proxy: Option<String>,
    /// Custom user agent
    #[arg(long)]
    pub user_agent: Option<String>,
    /// Request timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout: u64,
    /// Respect robots.txt
    #[arg(long)]
    pub respect_robots_txt: bool,
    /// When to use the browser: auto, always, or never
    #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
    pub browser_mode: CliBrowserMode,
    /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
    #[arg(long, value_parser = parse_browser_endpoint)]
    pub browser_endpoint: Option<String>,
    /// Configuration as JSON string or @file.json
    #[arg(long, value_name = "JSON")]
    pub config: Option<String>,
}

#[derive(Args)]
pub struct CrawlArgs {
    /// Seed URL(s) to crawl
    #[arg(required = true)]
    pub urls: Vec<String>,
    /// Maximum crawl depth
    #[arg(long, short = 'd', default_value = "2")]
    pub depth: usize,
    /// Maximum pages to crawl
    #[arg(long, short = 'n')]
    pub max_pages: Option<usize>,
    /// Maximum concurrent requests
    #[arg(long, short = 'c', default_value = "10")]
    pub concurrent: usize,
    /// Rate limit delay in milliseconds
    #[arg(long, default_value = "200")]
    pub rate_limit: u64,
    /// Output format: json or markdown
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Proxy URL
    #[arg(long)]
    pub proxy: Option<String>,
    /// Custom user agent
    #[arg(long)]
    pub user_agent: Option<String>,
    /// Request timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout: u64,
    /// Respect robots.txt
    #[arg(long)]
    pub respect_robots_txt: bool,
    /// Stay on the same domain
    #[arg(long)]
    pub stay_on_domain: bool,
    /// When to use the browser: auto, always, or never
    #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
    pub browser_mode: CliBrowserMode,
    /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
    #[arg(long, value_parser = parse_browser_endpoint)]
    pub browser_endpoint: Option<String>,
    /// Configuration as JSON string or @file.json
    #[arg(long, value_name = "JSON")]
    pub config: Option<String>,
}

#[derive(Args)]
pub struct MapArgs {
    /// URL to map
    pub url: String,
    /// Maximum URLs to return
    #[arg(long)]
    pub limit: Option<usize>,
    /// Filter URLs by substring
    #[arg(long)]
    pub search: Option<String>,
    /// Respect robots.txt
    #[arg(long)]
    pub respect_robots_txt: bool,
    /// Output format: json or markdown
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Request timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout: u64,
    /// When to use the browser: auto, always, or never
    #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
    pub browser_mode: CliBrowserMode,
    /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
    #[arg(long, value_parser = parse_browser_endpoint)]
    pub browser_endpoint: Option<String>,
    /// Configuration as JSON string or @file.json
    #[arg(long, value_name = "JSON")]
    pub config: Option<String>,
}

#[derive(Args)]
pub struct InteractArgs {
    /// URL to interact with
    pub url: String,
    /// Actions as JSON array (e.g. '[{"type":"click","selector":"#submit"}]')
    #[arg(long, value_name = "JSON")]
    pub actions: String,
    /// Output format: json or markdown
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Request timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout: u64,
    /// When to use the browser: auto, always, or never
    #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
    pub browser_mode: CliBrowserMode,
    /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
    #[arg(long, value_parser = parse_browser_endpoint)]
    pub browser_endpoint: Option<String>,
    /// Configuration as JSON string or @file.json
    #[arg(long, value_name = "JSON")]
    pub config: Option<String>,
}

#[derive(Args)]
pub struct BatchScrapeArgs {
    /// URLs to scrape
    // ~keep Deliberately not `required = true`. Empty input must reach `batch_scrape`
    // ~keep so the library owns the check and every language reports the same
    // ~keep `invalid_config: batch_urls must not be empty`. Under clap's guard the CLI
    // ~keep instead died at parse time with `<URLS>...`, which shares no text with the
    // ~keep library message and made the CLI the one binding that failed the shared
    // ~keep `batch_scrape_empty_urls_error` fixture.
    pub urls: Vec<String>,
    /// Maximum concurrent requests
    #[arg(long, short = 'c', default_value = "10")]
    pub concurrent: usize,
    /// Output format: json or markdown
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Proxy URL
    #[arg(long)]
    pub proxy: Option<String>,
    /// Custom user agent
    #[arg(long)]
    pub user_agent: Option<String>,
    /// Request timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout: u64,
    /// Respect robots.txt
    #[arg(long)]
    pub respect_robots_txt: bool,
    /// When to use the browser: auto, always, or never
    #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
    pub browser_mode: CliBrowserMode,
    /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
    #[arg(long, value_parser = parse_browser_endpoint)]
    pub browser_endpoint: Option<String>,
    /// Configuration as JSON string or @file.json
    #[arg(long, value_name = "JSON")]
    pub config: Option<String>,
}

#[derive(Args)]
pub struct BatchCrawlArgs {
    /// Seed URLs to crawl
    #[arg(required = true)]
    pub urls: Vec<String>,
    /// Maximum crawl depth
    #[arg(long, short = 'd', default_value = "2")]
    pub depth: usize,
    /// Maximum pages to crawl per seed
    #[arg(long, short = 'n')]
    pub max_pages: Option<usize>,
    /// Maximum concurrent requests
    #[arg(long, short = 'c', default_value = "10")]
    pub concurrent: usize,
    /// Rate limit delay in milliseconds
    #[arg(long, default_value = "200")]
    pub rate_limit: u64,
    /// Output format: json or markdown
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Proxy URL
    #[arg(long)]
    pub proxy: Option<String>,
    /// Custom user agent
    #[arg(long)]
    pub user_agent: Option<String>,
    /// Request timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout: u64,
    /// Respect robots.txt
    #[arg(long)]
    pub respect_robots_txt: bool,
    /// Stay on the same domain
    #[arg(long)]
    pub stay_on_domain: bool,
    /// When to use the browser: auto, always, or never
    #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
    pub browser_mode: CliBrowserMode,
    /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
    #[arg(long, value_parser = parse_browser_endpoint)]
    pub browser_endpoint: Option<String>,
    /// Configuration as JSON string or @file.json
    #[arg(long, value_name = "JSON")]
    pub config: Option<String>,
}

#[derive(Args)]
pub struct DownloadArgs {
    /// URL to download
    pub url: String,
    /// Maximum document size in bytes
    #[arg(long)]
    pub max_size: Option<usize>,
    /// Request timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout: u64,
    /// When to use the browser: auto, always, or never
    #[arg(long, value_enum, default_value_t = CliBrowserMode::Auto)]
    pub browser_mode: CliBrowserMode,
    /// CDP WebSocket endpoint for an external browser (must start with ws:// or wss://)
    #[arg(long, value_parser = parse_browser_endpoint)]
    pub browser_endpoint: Option<String>,
    /// Configuration as JSON string or @file.json
    #[arg(long, value_name = "JSON")]
    pub config: Option<String>,
}

#[derive(Args)]
pub struct CitationsArgs {
    /// Markdown text, or @file.md to read from a file
    pub input: String,
}

#[cfg(feature = "api")]
#[derive(Args)]
pub struct ServeArgs {
    /// Host address to bind to
    #[arg(long, default_value = "0.0.0.0")]
    pub host: String,
    /// Port to listen on
    #[arg(long, default_value = "3000")]
    pub port: u16,
}

#[cfg(feature = "mcp")]
#[derive(Args)]
pub struct McpArgs {
    /// Serve over Streamable HTTP at `/mcp` instead of stdio
    /// (requires a build with the `mcp-http` feature)
    #[arg(long)]
    pub http: bool,
    /// Host address to bind to in `--http` mode
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Port to listen on in `--http` mode
    #[arg(long, default_value = "3001")]
    pub port: u16,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, CliLogLevel, Commands, log_directives, parse_browser_endpoint};

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

    #[test]
    fn log_level_flag_overrides_verbose_and_quiet() {
        assert_eq!(log_directives(Some(CliLogLevel::Trace), 0, true), "trace");
        assert_eq!(log_directives(Some(CliLogLevel::Off), 3, false), "off");
    }

    #[test]
    fn default_log_directives_raise_the_cli_to_info() {
        assert_eq!(log_directives(None, 0, false), "warn,crawlberg_cli=info");
        assert_eq!(log_directives(None, 1, false), "info");
        assert_eq!(log_directives(None, 2, false), "debug");
        assert_eq!(log_directives(None, 9, false), "trace");
        assert_eq!(log_directives(None, 0, true), "error");
    }

    #[test]
    fn parses_batch_scrape_subcommand() {
        let cli = Cli::try_parse_from(["crawlberg", "batch-scrape", "https://a.com", "https://b.com"]).unwrap();
        match cli.command {
            Commands::BatchScrape(args) => {
                assert_eq!(
                    args.urls,
                    vec!["https://a.com".to_string(), "https://b.com".to_string()]
                );
                assert_eq!(args.concurrent, 10);
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
            Commands::BatchCrawl(args) => {
                assert_eq!(args.urls.len(), 2);
                assert_eq!(args.depth, 3);
            }
            _ => panic!("expected BatchCrawl"),
        }
    }

    #[test]
    fn parses_download_subcommand() {
        let cli =
            Cli::try_parse_from(["crawlberg", "download", "https://a.com/doc.pdf", "--max-size", "1024"]).unwrap();
        match cli.command {
            Commands::Download(args) => {
                assert_eq!(args.url, "https://a.com/doc.pdf");
                assert_eq!(args.max_size, Some(1024));
            }
            _ => panic!("expected Download"),
        }
    }

    #[test]
    fn parses_citations_subcommand() {
        let cli = Cli::try_parse_from(["crawlberg", "citations", "@notes.md"]).unwrap();
        match cli.command {
            Commands::Citations(args) => assert_eq!(args.input, "@notes.md"),
            _ => panic!("expected Citations"),
        }
    }

    #[test]
    fn parses_version_subcommand() {
        let cli = Cli::try_parse_from(["crawlberg", "version"]).unwrap();
        assert!(matches!(cli.command, Commands::Version {}));
    }

    #[test]
    fn batch_scrape_defers_empty_urls_to_library_validation() {
        let cli = Cli::try_parse_from(["crawlberg", "batch-scrape"]).expect("empty urls must parse");
        match cli.command {
            Commands::BatchScrape(args) => assert!(args.urls.is_empty(), "urls should reach the library empty"),
            _ => panic!("expected BatchScrape"),
        }
    }

    #[test]
    fn scrape_defaults_match_the_documented_flag_defaults() {
        let cli = Cli::try_parse_from(["crawlberg", "scrape", "https://a.com"]).unwrap();
        match cli.command {
            Commands::Scrape(args) => {
                assert_eq!(args.url, "https://a.com");
                assert_eq!(args.format, "json");
                assert_eq!(args.timeout, 30_000);
                assert!(!args.respect_robots_txt);
                assert_eq!(args.proxy, None);
                assert_eq!(args.config, None);
            }
            _ => panic!("expected Scrape"),
        }
    }

    #[test]
    fn crawl_defaults_match_the_documented_flag_defaults() {
        let cli = Cli::try_parse_from(["crawlberg", "crawl", "https://a.com"]).unwrap();
        match cli.command {
            Commands::Crawl(args) => {
                assert_eq!(args.depth, 2);
                assert_eq!(args.concurrent, 10);
                assert_eq!(args.rate_limit, 200);
                assert_eq!(args.timeout, 30_000);
                assert_eq!(args.max_pages, None);
                assert!(!args.stay_on_domain);
            }
            _ => panic!("expected Crawl"),
        }
    }

    #[test]
    fn map_accepts_limit_and_search_filters() {
        let cli =
            Cli::try_parse_from(["crawlberg", "map", "https://a.com", "--limit", "5", "--search", "docs"]).unwrap();
        match cli.command {
            Commands::Map(args) => {
                assert_eq!(args.limit, Some(5));
                assert_eq!(args.search, Some("docs".to_string()));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn interact_requires_the_actions_flag() {
        assert!(Cli::try_parse_from(["crawlberg", "interact", "https://a.com"]).is_err());

        let cli = Cli::try_parse_from(["crawlberg", "interact", "https://a.com", "--actions", "[]"]).unwrap();
        match cli.command {
            Commands::Interact(args) => assert_eq!(args.actions, "[]"),
            _ => panic!("expected Interact"),
        }
    }

    #[test]
    fn crawl_rejects_a_non_websocket_browser_endpoint() {
        assert!(
            Cli::try_parse_from([
                "crawlberg",
                "crawl",
                "https://a.com",
                "--browser-endpoint",
                "http://127.0.0.1:9222",
            ])
            .is_err()
        );
    }
}
