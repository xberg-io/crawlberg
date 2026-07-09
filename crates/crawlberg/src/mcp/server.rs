//! Crawlberg MCP server implementation.
//!
//! This module provides the main MCP server struct and startup functions.

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
    transport::stdio,
};

use crate::engine::CrawlEngineBuilder;
use crate::types::CrawlConfig;

/// Validate that a URL is non-empty and uses http(s) scheme.
fn validate_url(url: &str) -> Result<(), rmcp::ErrorData> {
    if url.is_empty() {
        return Err(rmcp::ErrorData::invalid_params("url is required", None));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(rmcp::ErrorData::invalid_params(
            "url must start with http:// or https://",
            None,
        ));
    }
    Ok(())
}

/// Parse the optional format parameter, defaulting to "markdown".
fn parse_format(format: &Option<String>) -> &str {
    match format.as_deref() {
        Some(f) if f.eq_ignore_ascii_case("json") => "json",
        _ => "markdown",
    }
}

/// Crawlberg MCP server.
///
/// Provides web scraping, crawling, and mapping capabilities via MCP tools.
///
/// The server holds a default `CrawlConfig` that is used as the base for
/// all tool calls. Per-request parameters override specific fields.
pub struct CrawlbergMcp {
    tool_router: ToolRouter<CrawlbergMcp>,
    /// Default crawl configuration
    config: CrawlConfig,
}

impl Clone for CrawlbergMcp {
    fn clone(&self) -> Self {
        Self {
            tool_router: self.tool_router.clone(),
            config: self.config.clone(),
        }
    }
}

#[tool_router]
impl CrawlbergMcp {
    /// Create a new Crawlberg MCP server instance with default config.
    pub fn new() -> Self {
        Self::with_config(CrawlConfig::default())
    }

    /// Create a new Crawlberg MCP server instance with explicit config.
    ///
    /// # Arguments
    ///
    /// * `config` - Default crawl configuration for all tool calls
    pub fn with_config(config: CrawlConfig) -> Self {
        Self {
            tool_router: Self::tool_router(),
            config,
        }
    }

    // ~keep Build engines per request because each tool call can apply different crawl config overrides.

    /// Build a CrawlEngine from the stored config with parameter overrides applied.
    fn build_engine(&self, config: CrawlConfig) -> Result<crate::engine::CrawlEngine, rmcp::ErrorData> {
        CrawlEngineBuilder::new()
            .config(config)
            .build()
            .map_err(|e| rmcp::ErrorData::internal_error(format!("Failed to build engine: {e}"), None))
    }

    /// Scrape a single URL and extract content as markdown or JSON.
    ///
    /// Returns the page content, metadata, links, images, and feeds.
    /// Use `format: "json"` for structured output or `format: "markdown"` (default)
    /// for human-readable content.
    #[tool(
        description = "Scrape a single URL and extract content as markdown or JSON. Returns page content, metadata, links, and images.",
        annotations(
            title = "Scrape URL",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn scrape(
        &self,
        Parameters(params): Parameters<super::params::ScrapeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::errors::map_crawl_error;
        use super::format::{format_as_json, format_as_markdown};

        validate_url(&params.url)?;

        let config = self.config.clone();

        #[cfg(feature = "browser")]
        let config = {
            let mut config = config;
            if let Some(true) = params.use_browser {
                config.browser.mode = crate::types::BrowserMode::Always;
            }
            config
        };

        let engine = self.build_engine(config)?;
        let result = engine.scrape(&params.url).await.map_err(map_crawl_error)?;

        let response = if parse_format(&params.format) == "json" {
            format_as_json(&result)
        } else {
            format_as_markdown(&result)
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(response)]))
    }

    /// Crawl a website following links up to a configured depth.
    ///
    /// Starts from the given URL and follows links, returning content for all
    /// discovered pages. Use `stay_on_domain: true` to restrict to the same domain.
    #[tool(
        description = "Crawl a website following links. Returns content for all discovered pages up to max_depth/max_pages.",
        annotations(title = "Crawl Website", read_only_hint = true, open_world_hint = true)
    )]
    async fn crawl(
        &self,
        Parameters(params): Parameters<super::params::CrawlParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::errors::map_crawl_error;
        use super::format::{format_crawl_as_json, format_crawl_as_markdown};

        validate_url(&params.url)?;

        if let Some(depth) = params.max_depth
            && depth > 100
        {
            return Err(rmcp::ErrorData::invalid_params("max_depth must be <= 100", None));
        }
        if let Some(pages) = params.max_pages
            && (pages == 0 || pages > 100_000)
        {
            return Err(rmcp::ErrorData::invalid_params("max_pages must be 1..=100000", None));
        }

        let mut config = self.config.clone();
        if let Some(depth) = params.max_depth {
            config.max_depth = Some(depth);
        }
        if let Some(pages) = params.max_pages {
            config.max_pages = Some(pages);
        }
        if let Some(stay) = params.stay_on_domain {
            config.stay_on_domain = stay;
        }

        let engine = self.build_engine(config)?;
        let result = engine.crawl(&params.url).await.map_err(map_crawl_error)?;

        let response = if parse_format(&params.format) == "json" {
            format_crawl_as_json(&result)
        } else {
            format_crawl_as_markdown(&result)
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(response)]))
    }

    /// Discover all pages on a website via links and sitemaps.
    ///
    /// Returns a list of all discovered URLs with their metadata (last modified,
    /// change frequency, priority). Use `search` to filter URLs by substring.
    #[tool(
        description = "Discover all pages on a website via links and sitemaps. Returns a list of discovered URLs.",
        annotations(
            title = "Map Website",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn map(
        &self,
        Parameters(params): Parameters<super::params::MapParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::errors::map_crawl_error;
        use super::format::format_map_result;

        validate_url(&params.url)?;

        let mut config = self.config.clone();
        if let Some(limit) = params.limit {
            config.map_limit = Some(limit);
        }
        if let Some(ref search) = params.search {
            config.map_search = Some(search.clone());
        }
        if let Some(robots) = params.respect_robots_txt {
            config.respect_robots_txt = robots;
        }

        let engine = self.build_engine(config)?;
        let result = engine.map(&params.url).await.map_err(map_crawl_error)?;

        let response = format_map_result(&result);
        Ok(CallToolResult::success(vec![ContentBlock::text(response)]))
    }

    /// Scrape multiple URLs concurrently.
    ///
    /// Efficiently processes multiple URLs in parallel and returns combined results.
    #[tool(
        description = "Scrape multiple URLs concurrently. Returns results for all URLs.",
        annotations(title = "Batch Scrape", read_only_hint = true, open_world_hint = true)
    )]
    async fn batch_scrape(
        &self,
        Parameters(params): Parameters<super::params::BatchScrapeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::format::{format_as_json, format_as_markdown};

        if params.urls.is_empty() {
            return Err(rmcp::ErrorData::invalid_params("urls array must not be empty", None));
        }

        for url in &params.urls {
            validate_url(url)?;
        }

        let mut config = self.config.clone();
        if let Some(concurrency) = params.concurrency {
            config.max_concurrent = Some(concurrency);
        }

        let engine = self.build_engine(config)?;
        let url_refs: Vec<&str> = params.urls.iter().map(|s| s.as_str()).collect();
        let results = engine.batch_scrape(&url_refs).await;

        let is_json = parse_format(&params.format) == "json";

        let mut response = String::new();
        for (url, result) in &results {
            match result {
                Ok(scrape_result) => {
                    if is_json {
                        response.push_str(&format_as_json(scrape_result));
                    } else {
                        response.push_str(&format!("## {url}\n\n"));
                        response.push_str(&format_as_markdown(scrape_result));
                    }
                    response.push_str("\n\n---\n\n");
                }
                Err(e) => {
                    response.push_str(&format!("## {url}\n\n**Error:** {e}\n\n---\n\n"));
                }
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(response)]))
    }

    /// Crawl multiple seed URLs concurrently.
    ///
    /// Efficiently crawls multiple websites in parallel and returns combined results.
    #[tool(
        description = "Crawl multiple seed URLs concurrently. Returns crawl results for all seeds.",
        annotations(title = "Batch Crawl", read_only_hint = true, open_world_hint = true)
    )]
    async fn batch_crawl(
        &self,
        Parameters(params): Parameters<super::params::BatchCrawlParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::format::{format_crawl_as_json, format_crawl_as_markdown};

        if params.urls.is_empty() {
            return Err(rmcp::ErrorData::invalid_params("urls array must not be empty", None));
        }
        for url in &params.urls {
            validate_url(url)?;
        }
        if let Some(depth) = params.max_depth
            && depth > 100
        {
            return Err(rmcp::ErrorData::invalid_params("max_depth must be <= 100", None));
        }
        if let Some(pages) = params.max_pages
            && (pages == 0 || pages > 100_000)
        {
            return Err(rmcp::ErrorData::invalid_params("max_pages must be 1..=100000", None));
        }

        let mut config = self.config.clone();
        if let Some(depth) = params.max_depth {
            config.max_depth = Some(depth);
        }
        if let Some(pages) = params.max_pages {
            config.max_pages = Some(pages);
        }
        if let Some(stay) = params.stay_on_domain {
            config.stay_on_domain = stay;
        }
        if let Some(concurrency) = params.concurrency {
            config.max_concurrent = Some(concurrency);
        }

        let engine = self.build_engine(config)?;
        let url_refs: Vec<&str> = params.urls.iter().map(|s| s.as_str()).collect();
        let results = engine.batch_crawl(&url_refs).await;

        let is_json = parse_format(&params.format) == "json";

        let mut response = String::new();
        for (url, result) in &results {
            match result {
                Ok(crawl_result) => {
                    if is_json {
                        response.push_str(&format_crawl_as_json(crawl_result));
                    } else {
                        response.push_str(&format!("## {url}\n\n"));
                        response.push_str(&format_crawl_as_markdown(crawl_result));
                    }
                    response.push_str("\n\n---\n\n");
                }
                Err(e) => {
                    response.push_str(&format!("## {url}\n\n**Error:** {e}\n\n---\n\n"));
                }
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(response)]))
    }

    /// Download a document from a URL.
    ///
    /// Downloads the raw document bytes and returns metadata about the downloaded file
    /// including its MIME type, size, and content hash.
    #[tool(
        description = "Download a document from a URL. Returns metadata about the downloaded file.",
        annotations(title = "Download Document", read_only_hint = true, open_world_hint = true)
    )]
    async fn download(
        &self,
        Parameters(params): Parameters<super::params::DownloadParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::errors::map_crawl_error;

        validate_url(&params.url)?;

        let mut config = self.config.clone();
        config.download_documents = true;
        if let Some(max_size) = params.max_size {
            config.document_max_size = Some(max_size);
        }

        let engine = self.build_engine(config)?;
        let result = engine.scrape(&params.url).await.map_err(map_crawl_error)?;

        let response = if let Some(ref doc) = result.downloaded_document {
            serde_json::json!({
                "url": doc.url,
                "mime_type": doc.mime_type,
                "size": doc.size,
                "filename": doc.filename,
                "content_hash": doc.content_hash,
            })
            .to_string()
        } else {
            serde_json::json!({
                "url": params.url,
                "content_type": result.content_type,
                "status_code": result.status_code,
                "body_size": result.body_size,
                "note": "URL returned HTML content, not a downloadable document"
            })
            .to_string()
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(response)]))
    }

    /// Execute browser actions on a page.
    #[tool(
        description = "Execute browser actions on a page. Actions may mutate page or application state.",
        annotations(
            title = "Interact",
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn interact(
        &self,
        Parameters(params): Parameters<super::params::InteractParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::errors::map_crawl_error;

        validate_url(&params.url)?;
        let actions = params
            .actions
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<crate::PageAction>, _>>()
            .map_err(|e| rmcp::ErrorData::invalid_params(format!("invalid action payload: {e}"), None))?;

        let engine = self.build_engine(self.config.clone())?;
        let result = engine.interact(&params.url, &actions).await.map_err(map_crawl_error)?;
        let response = serde_json::to_string_pretty(&result).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Failed to serialize interaction result: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::text(response)]))
    }

    /// Convert markdown links into numbered citations.
    ///
    /// Rewrites inline `[text](url)` links to `text[N]` markers and appends a
    /// numbered reference list. Useful for producing LLM-friendly citations from
    /// scraped markdown.
    #[tool(
        description = "Convert markdown links into numbered citations with an appended reference list.",
        annotations(
            title = "Generate Citations",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn generate_citations(
        &self,
        Parameters(params): Parameters<super::params::GenerateCitationsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let result = crate::citations::generate_citations(&params.markdown);
        let response = serde_json::to_string_pretty(&result)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("Failed to serialize citations: {e}"), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(response)]))
    }

    /// Get the current crawlberg version.
    #[tool(
        description = "Get the current crawlberg library version.",
        annotations(
            title = "Get Version",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn get_version(
        &self,
        Parameters(_): Parameters<super::params::EmptyParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let response = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&response)
                .unwrap_or_else(|e| format!("{{\"error\": \"Failed to serialize version: {e}\"}}")),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for CrawlbergMcp {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability::default());

        let server_info = Implementation::new("crawlberg-mcp", env!("CARGO_PKG_VERSION"))
            .with_title("Crawlberg Web Crawling MCP Server")
            .with_description(
                "Web crawling and scraping library for extracting content from websites. \
                 Supports single-page scraping, multi-page crawling, site mapping, and batch operations.",
            )
            .with_website_url("https://github.com/xberg-io/crawlberg");

        InitializeResult::new(capabilities)
            .with_server_info(server_info)
            .with_instructions(
                "Scrape, crawl, and map websites. Use 'scrape' for single pages, 'crawl' for \
                 following links across a site, 'map' for discovering all URLs, and 'batch_scrape'/\
                 'batch_crawl' for processing multiple URLs concurrently. Use format: 'json' for \
                 structured output or 'markdown' (default) for human-readable content.",
            )
    }
}

impl Default for CrawlbergMcp {
    fn default() -> Self {
        Self::new()
    }
}

/// Start the Crawlberg MCP server with default configuration.
///
/// This function initializes and runs the MCP server using stdio transport.
/// It will block until the server is shut down.
///
/// # Errors
///
/// Returns an error if the server fails to start or encounters a fatal error.
///
/// # Example
///
/// ```rust,no_run
/// use crawlberg::start_mcp_server;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
///     start_mcp_server().await?;
///     Ok(())
/// }
/// ```
pub async fn start_mcp_server() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    start_mcp_server_with_config(CrawlConfig::default()).await
}

/// Start MCP server with custom crawl configuration.
///
/// This variant allows specifying a custom crawl configuration
/// instead of using defaults.
pub async fn start_mcp_server_with_config(config: CrawlConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = CrawlbergMcp::with_config(config).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Concrete type of the Streamable HTTP MCP service produced by
/// [`streamable_http_service`].
pub type CrawlbergHttpMcpService = rmcp::transport::streamable_http_server::StreamableHttpService<
    CrawlbergMcp,
    rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
>;

/// Build a Streamable HTTP MCP service that can be mounted onto an axum/tower
/// router (for example at `/mcp`).
///
/// The returned value is a [`tower::Service`](rmcp::transport::streamable_http_server::StreamableHttpService)
/// and exposes the same nine tools as the stdio server. Each HTTP session gets a
/// fresh [`CrawlbergMcp`] backed by `config`, with sessions tracked in-memory
/// via rmcp's `LocalSessionManager`.
pub fn streamable_http_service(config: CrawlConfig) -> CrawlbergHttpMcpService {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    StreamableHttpService::new(
        move || Ok::<_, std::io::Error>(CrawlbergMcp::with_config(config.clone())),
        std::sync::Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// (name, read_only_hint, destructive_hint, open_world_hint) per tool.
    type ToolHintExpectation = (&'static str, Option<bool>, Option<bool>, Option<bool>);

    /// Every tool must advertise rmcp annotation hints so MCP clients can reason
    /// about safety: `open_world_hint` for web-touching tools, `destructive_hint`
    /// for the one tool that mutates remote state.
    #[test]
    fn all_tools_declare_expected_annotation_hints() {
        let tools = CrawlbergMcp::new().tool_router.list_all();
        let by_name: HashMap<&str, &Tool> = tools.iter().map(|t| (t.name.as_ref(), t)).collect();

        let expected: &[ToolHintExpectation] = &[
            ("scrape", Some(true), None, Some(true)),
            ("crawl", Some(true), None, Some(true)),
            ("map", Some(true), None, Some(true)),
            ("batch_scrape", Some(true), None, Some(true)),
            ("batch_crawl", Some(true), None, Some(true)),
            ("download", Some(true), None, Some(true)),
            ("interact", Some(false), Some(true), Some(true)),
            ("generate_citations", Some(true), None, Some(false)),
            ("get_version", Some(true), None, Some(false)),
        ];

        assert_eq!(
            tools.len(),
            expected.len(),
            "tool count drifted from annotation expectations"
        );

        for (name, read_only, destructive, open_world) in expected {
            let tool = by_name
                .get(name)
                .unwrap_or_else(|| panic!("tool `{name}` missing from router"));
            let ann = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("tool `{name}` has no annotations"));
            assert_eq!(ann.read_only_hint, *read_only, "read_only_hint mismatch for `{name}`");
            assert_eq!(
                ann.destructive_hint, *destructive,
                "destructive_hint mismatch for `{name}`"
            );
            assert_eq!(
                ann.open_world_hint, *open_world,
                "open_world_hint mismatch for `{name}`"
            );
            assert!(ann.title.is_some(), "tool `{name}` is missing a title annotation");
        }
    }
}
