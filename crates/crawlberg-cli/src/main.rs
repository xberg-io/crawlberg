use std::time::Duration;

use clap::Parser;
use crawlberg::{
    BatchCrawlResults, BatchScrapeResults, BrowserConfig, CrawlConfig, CrawlEngineHandle, CrawlPageResult, CrawlResult,
    PageAction, ProxyConfig, batch_crawl, batch_scrape, crawl, create_engine, generate_citations, interact, map_urls,
    scrape,
};

mod cli;
mod telemetry;

use cli::{
    BatchCrawlArgs, BatchScrapeArgs, CitationsArgs, Cli, CliBrowserMode, Commands, CrawlArgs, DownloadArgs,
    InteractArgs, MapArgs, ScrapeArgs, log_directives,
};
use telemetry::LogConfig;

/// Write a line of program output to stdout — the CLI's data channel. Diagnostics
/// go through `tracing` (stderr); results go here. Exempt from `print_stdout`.
#[expect(clippy::print_stdout, reason = "stdout is the CLI's data output channel")]
fn emit(text: &str) {
    println!("{text}");
}

/// Log `error` and terminate with the CLI's failure exit code.
fn exit_with_error(error: &dyn std::fmt::Display) -> ! {
    tracing::error!("{error}");
    std::process::exit(1);
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

/// Apply an optional `--config` JSON overlay and build the engine, exiting on failure.
fn prepare_engine(mut config: CrawlConfig, config_str: Option<String>) -> CrawlEngineHandle {
    if let Some(config_json) = config_str
        && let Err(e) = merge_json_config(&mut config, &config_json)
    {
        tracing::error!("invalid config: {e}");
        std::process::exit(1);
    }

    match create_engine(Some(config)) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!("failed to create crawl engine: {e}");
            std::process::exit(1);
        }
    }
}

fn proxy_config(proxy: Option<String>) -> Option<ProxyConfig> {
    proxy.map(|url| ProxyConfig {
        url,
        username: None,
        password: None,
    })
}

/// Emit each crawled page's markdown, prefixed with its own URL.
fn emit_crawl_pages(pages: &[CrawlPageResult]) {
    for page in pages {
        if let Some(ref md) = page.markdown {
            emit(&format!("---\nURL: {}\n---\n{}\n", page.url, md.content));
        }
    }
}

/// Emit each crawled page's markdown, prefixed with the seed URL it came from.
fn emit_batch_crawl_pages(seed_url: &str, pages: &[CrawlPageResult]) {
    for page in pages {
        if let Some(ref md) = page.markdown {
            emit(&format!(
                "---\nSeed: {}\nURL: {}\n---\n{}\n",
                seed_url, page.url, md.content
            ));
        }
    }
}

/// Print a single crawl as markdown, or the full `CrawlResult` object as JSON.
fn print_crawl(result: &CrawlResult, format: &str) {
    if format == "markdown" {
        emit_crawl_pages(&result.pages);
    } else {
        emit(&serde_json::to_string_pretty(result).expect("result is serializable"));
    }
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
                emit_batch_crawl_pages(&entry.url, &r.pages);
            }
            if let Some(ref e) = entry.error {
                tracing::error!(url = %entry.url, error = %e, "crawl failed");
            }
        }
    } else {
        emit(&serde_json::to_string_pretty(results).expect("results are serializable"));
    }
}

async fn run_scrape(args: ScrapeArgs) {
    let timeout_duration = Duration::from_millis(args.timeout);
    let config = CrawlConfig {
        user_agent: args.user_agent,
        request_timeout: timeout_duration,
        respect_robots_txt: args.respect_robots_txt,
        proxy: proxy_config(args.proxy),
        browser: build_browser_config(args.browser_mode, args.browser_endpoint, timeout_duration),
        ..Default::default()
    };

    let handle = prepare_engine(config, args.config);

    match scrape(&handle, &args.url).await {
        Ok(result) => {
            if args.format == "markdown" {
                if let Some(ref md) = result.markdown {
                    emit(&md.content);
                } else {
                    tracing::warn!("no markdown content available");
                }
            } else {
                emit(&serde_json::to_string_pretty(&result).expect("result is serializable"));
            }
        }
        Err(e) => exit_with_error(&e),
    }
}

async fn run_crawl(args: CrawlArgs) {
    let timeout_duration = Duration::from_millis(args.timeout);
    let config = CrawlConfig {
        max_depth: Some(args.depth),
        max_pages: args.max_pages,
        max_concurrent: Some(args.concurrent),
        rate_limit_ms: Some(args.rate_limit),
        user_agent: args.user_agent,
        request_timeout: timeout_duration,
        respect_robots_txt: args.respect_robots_txt,
        stay_on_domain: args.stay_on_domain,
        proxy: proxy_config(args.proxy),
        browser: build_browser_config(args.browser_mode, args.browser_endpoint, timeout_duration),
        ..Default::default()
    };

    let handle = prepare_engine(config, args.config);

    if args.urls.len() == 1 {
        match crawl(&handle, &args.urls[0]).await {
            Ok(result) => print_crawl(&result, &args.format),
            Err(e) => exit_with_error(&e),
        }
    } else {
        match batch_crawl(&handle, args.urls).await {
            Ok(results) => print_batch_crawl(&results, &args.format),
            Err(e) => exit_with_error(&e),
        }
    }
}

async fn run_map(args: MapArgs) {
    let timeout_duration = Duration::from_millis(args.timeout);
    let config = CrawlConfig {
        respect_robots_txt: args.respect_robots_txt,
        map_limit: args.limit,
        map_search: args.search,
        request_timeout: timeout_duration,
        browser: build_browser_config(args.browser_mode, args.browser_endpoint, timeout_duration),
        ..Default::default()
    };

    let handle = prepare_engine(config, args.config);

    match map_urls(&handle, &args.url).await {
        Ok(result) => {
            if args.format == "markdown" {
                for url_entry in &result.urls {
                    emit(&url_entry.url);
                }
            } else {
                emit(&serde_json::to_string_pretty(&result).expect("result is serializable"));
            }
        }
        Err(e) => exit_with_error(&e),
    }
}

async fn run_interact(args: InteractArgs) {
    let timeout_duration = Duration::from_millis(args.timeout);
    // ~keep This arm does not use `prepare_engine`: the actions JSON must be parsed
    // ~keep between the config merge and engine creation, so a malformed `--actions`
    // ~keep value fails before any browser is launched.
    let mut config = CrawlConfig {
        request_timeout: timeout_duration,
        browser: build_browser_config(args.browser_mode, args.browser_endpoint, timeout_duration),
        ..Default::default()
    };

    if let Some(config_json) = args.config
        && let Err(e) = merge_json_config(&mut config, &config_json)
    {
        tracing::error!("invalid config: {e}");
        std::process::exit(1);
    }

    let parsed_actions: Vec<PageAction> = match serde_json::from_str(&args.actions) {
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

    match interact(&handle, &args.url, parsed_actions).await {
        Ok(result) => {
            if args.format == "markdown" {
                emit(&result.final_html);
            } else {
                let wrapped = serde_json::json!({ "interaction": result });
                emit(&serde_json::to_string_pretty(&wrapped).expect("result is serializable"));
            }
        }
        Err(e) => exit_with_error(&e),
    }
}

async fn run_batch_scrape(args: BatchScrapeArgs) {
    let timeout_duration = Duration::from_millis(args.timeout);
    let config = CrawlConfig {
        max_concurrent: Some(args.concurrent),
        user_agent: args.user_agent,
        request_timeout: timeout_duration,
        respect_robots_txt: args.respect_robots_txt,
        proxy: proxy_config(args.proxy),
        browser: build_browser_config(args.browser_mode, args.browser_endpoint, timeout_duration),
        ..Default::default()
    };

    let handle = prepare_engine(config, args.config);

    match batch_scrape(&handle, args.urls).await {
        Ok(results) => print_batch_scrape(&results, &args.format),
        Err(e) => exit_with_error(&e),
    }
}

async fn run_batch_crawl(args: BatchCrawlArgs) {
    let timeout_duration = Duration::from_millis(args.timeout);
    let config = CrawlConfig {
        max_depth: Some(args.depth),
        max_pages: args.max_pages,
        max_concurrent: Some(args.concurrent),
        rate_limit_ms: Some(args.rate_limit),
        user_agent: args.user_agent,
        request_timeout: timeout_duration,
        respect_robots_txt: args.respect_robots_txt,
        stay_on_domain: args.stay_on_domain,
        proxy: proxy_config(args.proxy),
        browser: build_browser_config(args.browser_mode, args.browser_endpoint, timeout_duration),
        ..Default::default()
    };

    let handle = prepare_engine(config, args.config);

    match batch_crawl(&handle, args.urls).await {
        Ok(results) => print_batch_crawl(&results, &args.format),
        Err(e) => exit_with_error(&e),
    }
}

async fn run_download(args: DownloadArgs) {
    let timeout_duration = Duration::from_millis(args.timeout);
    let config = CrawlConfig {
        request_timeout: timeout_duration,
        download_documents: true,
        document_max_size: args.max_size,
        browser: build_browser_config(args.browser_mode, args.browser_endpoint, timeout_duration),
        ..Default::default()
    };

    let handle = prepare_engine(config, args.config);

    match scrape(&handle, &args.url).await {
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
                    "url": args.url,
                    "content_type": result.content_type,
                    "status_code": result.status_code,
                    "body_size": result.body_size,
                    "note": "URL returned HTML content, not a downloadable document",
                })
            };
            emit(&serde_json::to_string_pretty(&output).expect("output is serializable"));
        }
        Err(e) => exit_with_error(&e),
    }
}

fn run_citations(args: CitationsArgs) {
    let markdown = if let Some(path) = args.input.strip_prefix('@') {
        match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                tracing::error!("cannot read {path}: {e}");
                std::process::exit(1);
            }
        }
    } else {
        args.input
    };
    let result = generate_citations(&markdown);
    emit(&serde_json::to_string_pretty(&result).expect("result is serializable"));
}

fn run_version() {
    let response = serde_json::json!({ "version": env!("CARGO_PKG_VERSION") });
    emit(&serde_json::to_string_pretty(&response).expect("version is serializable"));
}

#[cfg(feature = "api")]
async fn run_serve(args: cli::ServeArgs) {
    let cli::ServeArgs { host, port } = args;
    tracing::info!("starting REST API server on {host}:{port}");
    #[cfg(feature = "mcp")]
    tracing::info!("MCP Streamable HTTP transport available at http://{host}:{port}/mcp");
    if let Err(e) = crawlberg::serve_api(&host, port, CrawlConfig::default()).await {
        tracing::error!("server error: {e}");
        std::process::exit(1);
    }
}

#[cfg(feature = "mcp")]
async fn run_mcp(args: cli::McpArgs) {
    let cli::McpArgs { http, host, port } = args;
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
        Commands::Scrape(args) => run_scrape(args).await,
        Commands::Crawl(args) => run_crawl(args).await,
        Commands::Map(args) => run_map(args).await,
        Commands::Interact(args) => run_interact(args).await,
        Commands::BatchScrape(args) => run_batch_scrape(args).await,
        Commands::BatchCrawl(args) => run_batch_crawl(args).await,
        Commands::Download(args) => run_download(args).await,
        Commands::Citations(args) => run_citations(args),
        Commands::Version {} => run_version(),
        #[cfg(feature = "api")]
        Commands::Serve(args) => run_serve(args).await,
        #[cfg(feature = "mcp")]
        Commands::Mcp(args) => run_mcp(args).await,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CliBrowserMode, build_browser_config, merge_json_config, proxy_config};
    use crawlberg::{BrowserMode, CrawlConfig};

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
    fn proxy_config_carries_the_url_without_credentials() {
        let proxy = proxy_config(Some("http://proxy.test:8080".to_string())).expect("proxy should be set");
        assert_eq!(proxy.url, "http://proxy.test:8080");
        assert_eq!(proxy.username, None);
        assert_eq!(proxy.password, None);

        assert!(proxy_config(None).is_none());
    }

    #[test]
    fn merge_json_config_overrides_named_fields() {
        let mut config = CrawlConfig {
            max_depth: Some(2),
            ..Default::default()
        };
        merge_json_config(&mut config, r#"{"max_depth": 7, "stay_on_domain": true}"#).expect("valid config");
        assert_eq!(config.max_depth, Some(7));
        assert!(config.stay_on_domain);
    }

    #[test]
    fn merge_json_config_leaves_unmentioned_fields_alone() {
        let mut config = CrawlConfig {
            max_concurrent: Some(4),
            ..Default::default()
        };
        merge_json_config(&mut config, r#"{"max_depth": 1}"#).expect("valid config");
        assert_eq!(config.max_concurrent, Some(4));
        assert_eq!(config.max_depth, Some(1));
    }

    #[test]
    fn merge_json_config_rejects_malformed_json() {
        let mut config = CrawlConfig::default();
        assert!(merge_json_config(&mut config, "{not json").is_err());
    }

    #[test]
    fn merge_json_config_reports_a_missing_at_file() {
        let mut config = CrawlConfig::default();
        assert!(merge_json_config(&mut config, "@/definitely/not/a/real/config.json").is_err());
    }
}
