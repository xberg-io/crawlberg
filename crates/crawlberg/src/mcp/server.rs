//! Crawlberg MCP server implementation.
//!
//! This module provides the main MCP server struct and startup functions.

use rmcp::{
    RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext, wrapper::Parameters},
    model::*,
    service::RequestContext,
    task_manager::{TaskExit, TaskManager, TaskOptions},
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

/// Build a tool result carrying both a human-readable text block and the
/// machine-readable `structuredContent` (SEP-2106).
///
/// `structured` is the JSON value clients can consume programmatically; `text`
/// is the same information rendered for humans (markdown or pretty JSON). The
/// `format` parameter selects the text representation only — `structuredContent`
/// is always populated so schema-aware clients get typed output regardless.
fn structured_success(structured: serde_json::Value, text: String) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(structured);
    result
}

/// Serialize a value into `structuredContent`, mapping failures to an MCP error.
fn to_structured<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, rmcp::ErrorData> {
    serde_json::to_value(value)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("failed to serialize structured content: {e}"), None))
}

/// Crawlberg MCP server.
///
/// Provides web scraping, crawling, and mapping capabilities via MCP tools.
///
/// The server holds a default `CrawlConfig` that is used as the base for
/// all tool calls. Per-request parameters override specific fields.
pub struct CrawlbergMcp {
    tool_router: ToolRouter<CrawlbergMcp>,
    /// SEP-2663 Tasks extension runtime. `Arc`-backed and cheaply cloneable;
    /// all clones share the same task store so a task created on one request
    /// remains observable via `tasks/get` on later (possibly stateless-HTTP)
    /// requests served by a different `CrawlbergMcp` instance.
    task_manager: TaskManager,
    /// Default crawl configuration
    config: CrawlConfig,
}

impl Clone for CrawlbergMcp {
    fn clone(&self) -> Self {
        Self {
            tool_router: self.tool_router.clone(),
            task_manager: self.task_manager.clone(),
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
    /// Each instance gets its own [`TaskManager`], which is correct for the
    /// single long-lived stdio server. HTTP callers that materialize a new
    /// instance per request should use [`Self::with_config_and_tasks`] to share
    /// one task store across instances.
    ///
    /// # Arguments
    ///
    /// * `config` - Default crawl configuration for all tool calls
    pub fn with_config(config: CrawlConfig) -> Self {
        Self::with_config_and_tasks(config, TaskManager::new())
    }

    /// Create a new Crawlberg MCP server instance sharing an existing
    /// [`TaskManager`].
    ///
    /// Use this when several instances must observe the same SEP-2663 task
    /// store — notably the stateless Streamable HTTP transport, which builds a
    /// fresh instance per request. Passing a shared, `Arc`-backed manager keeps
    /// a task created on the originating `tools/call` observable on the later
    /// `tasks/get` / `tasks/update` / `tasks/cancel` requests.
    pub fn with_config_and_tasks(config: CrawlConfig, task_manager: TaskManager) -> Self {
        Self {
            tool_router: Self::tool_router(),
            task_manager,
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
        output_schema = rmcp::handler::server::common::schema_for_output::<crate::types::ScrapeResult>(),
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
        use super::errors::crawl_error_to_tool_result;
        use super::format::{format_as_json, format_as_markdown};

        validate_url(&params.url)?;

        let mut config = self.config.clone();
        if let Some(true) = params.only_main_content {
            config.content.preprocessing_preset = "aggressive".to_owned();
        }

        #[cfg(feature = "browser")]
        let config = {
            let mut config = config;
            if let Some(true) = params.use_browser {
                config.browser.mode = crate::types::BrowserMode::Always;
            }
            config
        };

        let engine = self.build_engine(config)?;
        let result = match engine.scrape(&params.url).await {
            Ok(result) => result,
            Err(error) => return crawl_error_to_tool_result(error),
        };

        let text = if parse_format(&params.format) == "json" {
            format_as_json(&result)
        } else {
            format_as_markdown(&result)
        };

        Ok(structured_success(to_structured(&result)?, text))
    }

    /// Crawl a website following links up to a configured depth.
    ///
    /// Starts from the given URL and follows links, returning content for all
    /// discovered pages. Use `stay_on_domain: true` to restrict to the same domain.
    #[tool(
        description = "Crawl a website following links. Returns content for all discovered pages up to max_depth/max_pages.",
        output_schema = rmcp::handler::server::common::schema_for_output::<crate::types::CrawlResult>(),
        annotations(title = "Crawl Website", read_only_hint = true, open_world_hint = true)
    )]
    async fn crawl(
        &self,
        Parameters(params): Parameters<super::params::CrawlParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::errors::crawl_error_to_tool_result;
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
        let result = match engine.crawl(&params.url).await {
            Ok(result) => result,
            Err(error) => return crawl_error_to_tool_result(error),
        };

        let text = if parse_format(&params.format) == "json" {
            format_crawl_as_json(&result)
        } else {
            format_crawl_as_markdown(&result)
        };

        Ok(structured_success(to_structured(&result)?, text))
    }

    /// Discover all pages on a website via links and sitemaps.
    ///
    /// Returns a list of all discovered URLs with their metadata (last modified,
    /// change frequency, priority). Use `search` to filter URLs by substring.
    #[tool(
        description = "Discover all pages on a website via links and sitemaps. Returns a list of discovered URLs.",
        output_schema = rmcp::handler::server::common::schema_for_output::<crate::types::MapResult>(),
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
        use super::errors::crawl_error_to_tool_result;
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
        let result = match engine.map(&params.url).await {
            Ok(result) => result,
            Err(error) => return crawl_error_to_tool_result(error),
        };

        let text = format_map_result(&result);
        Ok(structured_success(to_structured(&result)?, text))
    }

    /// Scrape multiple URLs concurrently.
    ///
    /// Efficiently processes multiple URLs in parallel and returns combined results.
    #[tool(
        description = "Scrape multiple URLs concurrently. Returns results for all URLs.",
        output_schema = rmcp::handler::server::common::schema_for_output::<Vec<super::outputs::BatchScrapeItem>>(),
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
        let mut items: Vec<super::outputs::BatchScrapeItem> = Vec::with_capacity(results.len());
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
                    items.push(super::outputs::BatchScrapeItem {
                        url: url.to_string(),
                        ok: true,
                        result: Some(scrape_result.clone()),
                        error: None,
                    });
                }
                Err(e) => {
                    response.push_str(&format!("## {url}\n\n**Error:** {e}\n\n---\n\n"));
                    items.push(super::outputs::BatchScrapeItem {
                        url: url.to_string(),
                        ok: false,
                        result: None,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(structured_success(to_structured(&items)?, response))
    }

    /// Crawl multiple seed URLs concurrently.
    ///
    /// Efficiently crawls multiple websites in parallel and returns combined results.
    #[tool(
        description = "Crawl multiple seed URLs concurrently. Returns crawl results for all seeds.",
        output_schema = rmcp::handler::server::common::schema_for_output::<Vec<super::outputs::BatchCrawlItem>>(),
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
        let mut items: Vec<super::outputs::BatchCrawlItem> = Vec::with_capacity(results.len());
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
                    items.push(super::outputs::BatchCrawlItem {
                        url: url.to_string(),
                        ok: true,
                        result: Some(crawl_result.clone()),
                        error: None,
                    });
                }
                Err(e) => {
                    response.push_str(&format!("## {url}\n\n**Error:** {e}\n\n---\n\n"));
                    items.push(super::outputs::BatchCrawlItem {
                        url: url.to_string(),
                        ok: false,
                        result: None,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(structured_success(to_structured(&items)?, response))
    }

    /// Download a document from a URL.
    ///
    /// Downloads the raw document bytes and returns metadata about the downloaded file
    /// including its MIME type, size, and content hash.
    #[tool(
        description = "Download a document from a URL. Returns metadata about the downloaded file.",
        output_schema = rmcp::handler::server::common::schema_for_output::<super::outputs::DownloadOutput>(),
        annotations(title = "Download Document", read_only_hint = true, open_world_hint = true)
    )]
    async fn download(
        &self,
        Parameters(params): Parameters<super::params::DownloadParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use super::errors::crawl_error_to_tool_result;

        validate_url(&params.url)?;

        let mut config = self.config.clone();
        config.download_documents = true;
        if let Some(max_size) = params.max_size {
            config.document_max_size = Some(max_size);
        }

        let engine = self.build_engine(config)?;
        let result = match engine.scrape(&params.url).await {
            Ok(result) => result,
            Err(error) => return crawl_error_to_tool_result(error),
        };

        let output = if let Some(ref doc) = result.downloaded_document {
            super::outputs::DownloadOutput {
                url: doc.url.clone(),
                mime_type: Some(doc.mime_type.clone().into_owned()),
                size: Some(doc.size),
                filename: doc.filename.as_ref().map(|filename| filename.to_string()),
                content_hash: Some(doc.content_hash.to_string()),
                ..Default::default()
            }
        } else {
            super::outputs::DownloadOutput {
                url: params.url.clone(),
                content_type: Some(result.content_type.clone()),
                status_code: Some(result.status_code),
                body_size: Some(result.body_size),
                note: Some("URL returned HTML content, not a downloadable document".to_string()),
                ..Default::default()
            }
        };

        let structured = to_structured(&output)?;
        let text = serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string());
        Ok(structured_success(structured, text))
    }

    /// Execute browser actions on a page.
    #[tool(
        description = "Execute browser actions on a page. Actions may mutate page or application state.",
        output_schema = rmcp::handler::server::common::schema_for_output::<crate::types::InteractionResult>(),
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
        use super::errors::crawl_error_to_tool_result;

        validate_url(&params.url)?;
        let actions = params
            .actions
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<crate::PageAction>, _>>()
            .map_err(|e| rmcp::ErrorData::invalid_params(format!("invalid action payload: {e}"), None))?;

        let engine = self.build_engine(self.config.clone())?;
        let result = match engine.interact(&params.url, &actions).await {
            Ok(result) => result,
            Err(error) => return crawl_error_to_tool_result(error),
        };
        let structured = to_structured(&result)?;
        let text = serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string());

        Ok(structured_success(structured, text))
    }

    /// Convert markdown links into numbered citations.
    ///
    /// Rewrites inline `[text](url)` links to `text[N]` markers and appends a
    /// numbered reference list. Useful for producing LLM-friendly citations from
    /// scraped markdown.
    #[tool(
        description = "Convert markdown links into numbered citations with an appended reference list.",
        output_schema = rmcp::handler::server::common::schema_for_output::<crate::citations::CitationResult>(),
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
        let structured = to_structured(&result)?;
        let text = serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string());
        Ok(structured_success(structured, text))
    }

    /// Get the current crawlberg version.
    #[tool(
        description = "Get the current crawlberg library version.",
        output_schema = rmcp::handler::server::common::schema_for_output::<super::outputs::VersionOutput>(),
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
        let output = super::outputs::VersionOutput {
            version: env!("CARGO_PKG_VERSION").to_string(),
        };

        let structured = to_structured(&output)?;
        let text = serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string());
        Ok(structured_success(structured, text))
    }
}

/// Read the SEP-2663 task augmentation from a request's `_meta`.
///
/// The service layer moves a request's `_meta` into
/// [`RequestContext::meta`](rmcp::service::RequestContext), so the augmentation
/// is read from there rather than from the (now-empty) request params.
///
/// Returns `Some(ttl_ms)` when the client asked for task-based execution (the
/// `io.modelcontextprotocol/tasks` extension key is present); the inner value
/// is the client-requested TTL in milliseconds, or `None` for "unspecified"
/// (the manager default applies). Returns `None` when no task was requested.
fn requested_task_ttl(meta: &rmcp::model::RequestMetaObject) -> Option<Option<u64>> {
    let augmentation = meta.get(rmcp::model::TASKS_EXTENSION_ID)?;
    let ttl_ms = augmentation.get("ttl").and_then(serde_json::Value::as_u64);
    Some(ttl_ms)
}

#[tool_handler]
impl ServerHandler for CrawlbergMcp {
    /// Dispatch a `tools/call`, honoring SEP-2663 task augmentation.
    ///
    /// When the client both declares the tasks capability and augments this
    /// request with the tasks extension, the tool runs as a background task and
    /// we return a task handle immediately; the client then polls `tasks/get`.
    /// Otherwise the tool runs inline and returns its result directly (the
    /// behavior the `#[tool_handler]`-generated `call_tool` would provide).
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        request_context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let client_supports_tasks = request_context
            .client_capabilities()
            .is_some_and(|capabilities| capabilities.supports_tasks());
        let task_ttl = requested_task_ttl(&request_context.meta);

        // Run inline unless the client both supports and requested a task.
        let Some(ttl_ms) = task_ttl.filter(|_| client_supports_tasks) else {
            let context = ToolCallContext::new(self, request, request_context);
            return self.tool_router.call(context).await;
        };

        let options = match ttl_ms {
            Some(ttl_ms) => TaskOptions::new().with_ttl_ms(ttl_ms),
            None => TaskOptions::new(),
        };
        // Own a clone so the spawned operation is `'static`: the tool router is
        // driven against the moved `server`, borrowed only within the future.
        let server = self.clone();
        let task = self.task_manager.spawn(options, move |_task_context| {
            Box::pin(async move {
                let context = ToolCallContext::new(&server, request, request_context);
                match server.tool_router.call(context).await {
                    Ok(CallToolResponse::Complete(result)) => Ok(result),
                    Ok(_) => Err(TaskExit::Error(rmcp::ErrorData::internal_error(
                        "tool produced a non-final response inside a task",
                        None,
                    ))),
                    Err(error) => Err(TaskExit::Error(error)),
                }
            })
        });
        Ok(CallToolResponse::Task(CreateTaskResult::new(task)))
    }

    /// SEP-2663 `tasks/get`: report a task's current state.
    async fn get_task(
        &self,
        request: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, rmcp::ErrorData> {
        self.task_manager.get_task(&request.task_id).map(GetTaskResult::new)
    }

    /// SEP-2663 `tasks/update`: deliver client responses to in-task input requests.
    async fn update_task(
        &self,
        request: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        self.task_manager.update_task(&request.task_id, request.input_responses)
    }

    /// SEP-2663 `tasks/cancel`: request cooperative cancellation of a task.
    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        self.task_manager.cancel_task(&request.task_id)
    }

    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder().enable_tools().enable_tasks().build();

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

/// Start a dedicated MCP server over the Streamable HTTP transport, exposing
/// the MCP endpoint at `/mcp`.
///
/// This serves *only* the MCP transport — unlike [`crate::serve_api`], it mounts
/// no REST API routes. It backs `crawlberg mcp --http`. The transport is
/// stateless (SEP-2567) and supports the SEP-2663 Tasks extension across
/// requests via a shared task store. Blocks until the server shuts down.
///
/// # Errors
///
/// Returns an error if `host` is not a valid IP address, the address cannot be
/// bound, requires authentication that isn't configured (see
/// [`mcp_http_allow_insecure_bind`]), or the server encounters a fatal error
/// while running.
#[cfg(feature = "mcp-http")]
pub async fn start_mcp_http_server(
    host: &str,
    port: u16,
    config: CrawlConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::net::{IpAddr, SocketAddr};

    let ip: IpAddr = host
        .parse()
        .map_err(|e| format!("invalid host address '{host}': {e}"))?;

    // ~keep This is a second, independent HTTP listener alongside the REST API
    // ~keep (see `api::startup::ensure_bind_is_safe` for the same policy there):
    // ~keep an unauthenticated crawler reachable from the network is an abuse
    // ~keep vector, so a non-loopback bind requires a token or an explicit opt-out.
    let auth_token = mcp_http_auth_token();
    if auth_token.is_none() && !ip.is_loopback() && !mcp_http_allow_insecure_bind() {
        return Err(format!(
            "refusing to bind {ip} without authentication: set {token_env}, bind to a loopback \
             address (127.0.0.1/::1), or set {allow_env}=1 to explicitly opt out",
            token_env = crate::api::AUTH_TOKEN_ENV,
            allow_env = crate::api::ALLOW_INSECURE_BIND_ENV,
        )
        .into());
    }

    let addr = SocketAddr::new(ip, port);
    let mut app = axum::Router::new().nest_service("/mcp", streamable_http_service(config));
    if let Some(token) = auth_token {
        app = app.layer(axum::middleware::from_fn(move |req, next| {
            let token = token.clone();
            require_mcp_bearer_token(token, req, next)
        }));
    }

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Read [`crate::api::AUTH_TOKEN_ENV`] for the standalone MCP HTTP transport.
/// Shares the same env var (and hence the same credential) as the REST API's
/// auth, since both are HTTP surfaces exposing the same crawl engine.
#[cfg(feature = "mcp-http")]
fn mcp_http_auth_token() -> Option<std::sync::Arc<str>> {
    std::env::var(crate::api::AUTH_TOKEN_ENV)
        .ok()
        .filter(|token| !token.is_empty())
        .map(std::sync::Arc::from)
}

/// Read [`crate::api::ALLOW_INSECURE_BIND_ENV`]. Accepts `1` or `true` (case-insensitive).
#[cfg(feature = "mcp-http")]
fn mcp_http_allow_insecure_bind() -> bool {
    std::env::var(crate::api::ALLOW_INSECURE_BIND_ENV)
        .map(|value| value.eq_ignore_ascii_case("1") || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Enforce the configured bearer token on every request to the standalone MCP
/// HTTP transport (`crawlberg mcp --http`), mirroring the REST API's check.
#[cfg(feature = "mcp-http")]
async fn require_mcp_bearer_token(
    token: std::sync::Arc<str>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let provided = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    match provided {
        Some(candidate) if candidate.as_bytes() == token.as_bytes() => next.run(req).await,
        _ => (axum::http::StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response(),
    }
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

    // Share one task store across the per-request instances the service
    // materializes, so SEP-2663 tasks survive across a client's `tools/call` →
    // `tasks/get` request sequence even without a session.
    let task_manager = TaskManager::new();

    // SEP-2567: serve statelessly. Modern (2026-07-28+) clients are always
    // stateless; disabling legacy session mode drops per-session state for
    // older clients too. `json_response` returns plain `application/json` for
    // request/response tools and transparently falls back to SSE when a handler
    // needs to stream, so it stays compatible with tasks and progress.
    let mut http_config = StreamableHttpServerConfig::default();
    http_config.legacy_session_mode = false;
    http_config.json_response = true;

    StreamableHttpService::new(
        move || {
            Ok::<_, std::io::Error>(CrawlbergMcp::with_config_and_tasks(
                config.clone(),
                task_manager.clone(),
            ))
        },
        std::sync::Arc::new(LocalSessionManager::default()),
        http_config,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// (name, read_only_hint, destructive_hint, open_world_hint) per tool.
    type ToolHintExpectation = (&'static str, Option<bool>, Option<bool>, Option<bool>);

    /// The server must advertise both the tools capability and the SEP-2663
    /// tasks extension so clients know they may request task-based execution.
    #[test]
    fn get_info_advertises_tools_and_tasks_capabilities() {
        let capabilities = CrawlbergMcp::new().get_info().capabilities;
        assert!(capabilities.tools.is_some(), "tools capability must be advertised");
        assert!(
            capabilities.supports_tasks(),
            "SEP-2663 tasks extension capability must be advertised"
        );
    }

    /// The SEP-2663 task augmentation is read from the request `_meta`: present
    /// key means "run as task" (with an optional TTL), absent means inline.
    #[test]
    fn requested_task_ttl_reads_augmentation_from_meta() {
        use rmcp::model::RequestMetaObject;

        let empty = RequestMetaObject::new();
        assert_eq!(
            requested_task_ttl(&empty),
            None,
            "no augmentation means inline execution"
        );

        let mut with_ttl = RequestMetaObject::new();
        with_ttl.insert(
            rmcp::model::TASKS_EXTENSION_ID.to_string(),
            serde_json::json!({ "ttl": 30_000 }),
        );
        assert_eq!(
            requested_task_ttl(&with_ttl),
            Some(Some(30_000)),
            "tasks extension key with ttl requests a task with that TTL"
        );

        let mut without_ttl = RequestMetaObject::new();
        without_ttl.insert(rmcp::model::TASKS_EXTENSION_ID.to_string(), serde_json::json!({}));
        assert_eq!(
            requested_task_ttl(&without_ttl),
            Some(None),
            "tasks extension key without ttl requests a task with default TTL"
        );
    }

    /// SEP-2106: every tool must advertise an `outputSchema` so schema-aware
    /// clients get typed results.
    #[test]
    fn every_tool_advertises_an_output_schema() {
        let tools = CrawlbergMcp::new().tool_router.list_all();
        for tool in &tools {
            assert!(
                tool.output_schema.is_some(),
                "tool `{}` must advertise an outputSchema (SEP-2106)",
                tool.name
            );
        }
    }

    /// The advertised `outputSchema` for each tool must not drift from the
    /// `structuredContent` the tool actually emits: every serialized field must
    /// be a declared schema property, and every required property must appear in
    /// the serialized output. Because both the schema and the structured content
    /// derive from the same types (schemars + serde), this catches any rename or
    /// skip mismatch between the two derivations.
    #[test]
    fn output_schemas_do_not_drift_from_structured_content() {
        let tools = CrawlbergMcp::new().tool_router.list_all();

        let object_cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "scrape",
                serde_json::to_value(crate::types::ScrapeResult::default()).unwrap(),
            ),
            (
                "crawl",
                serde_json::to_value(crate::types::CrawlResult::default()).unwrap(),
            ),
            ("map", serde_json::to_value(crate::types::MapResult::default()).unwrap()),
            (
                "interact",
                serde_json::to_value(crate::types::InteractionResult::default()).unwrap(),
            ),
            (
                "generate_citations",
                serde_json::to_value(crate::citations::CitationResult {
                    content: "text[1]".to_string(),
                    references: vec![crate::citations::CitationReference {
                        index: 1,
                        url: "https://example.com".to_string(),
                        text: "example".to_string(),
                    }],
                })
                .unwrap(),
            ),
            (
                "get_version",
                serde_json::to_value(crate::mcp::outputs::VersionOutput {
                    version: "1.0.0".to_string(),
                })
                .unwrap(),
            ),
            (
                "download",
                serde_json::to_value(crate::mcp::outputs::DownloadOutput {
                    url: "https://example.com/doc.pdf".to_string(),
                    mime_type: Some("application/pdf".to_string()),
                    size: Some(1024),
                    filename: Some("doc.pdf".to_string()),
                    content_hash: Some("abc123".to_string()),
                    content_type: Some("text/html".to_string()),
                    status_code: Some(200),
                    body_size: Some(2048),
                    note: Some("note".to_string()),
                })
                .unwrap(),
            ),
        ];
        for (name, sample) in &object_cases {
            assert_serialized_matches_schema(name, &output_schema_of(&tools, name), sample);
        }

        let batch_scrape_sample = serde_json::to_value(vec![
            crate::mcp::outputs::BatchScrapeItem {
                url: "https://a.com".to_string(),
                ok: true,
                result: Some(crate::types::ScrapeResult::default()),
                error: None,
            },
            crate::mcp::outputs::BatchScrapeItem {
                url: "https://b.com".to_string(),
                ok: false,
                result: None,
                error: Some("boom".to_string()),
            },
        ])
        .unwrap();
        let scrape_item_schema = resolve_item_schema(&output_schema_of(&tools, "batch_scrape"));
        for element in batch_scrape_sample.as_array().unwrap() {
            assert_serialized_matches_schema("batch_scrape", &scrape_item_schema, element);
        }

        let batch_crawl_sample = serde_json::to_value(vec![
            crate::mcp::outputs::BatchCrawlItem {
                url: "https://a.com".to_string(),
                ok: true,
                result: Some(crate::types::CrawlResult::default()),
                error: None,
            },
            crate::mcp::outputs::BatchCrawlItem {
                url: "https://b.com".to_string(),
                ok: false,
                result: None,
                error: Some("boom".to_string()),
            },
        ])
        .unwrap();
        let crawl_item_schema = resolve_item_schema(&output_schema_of(&tools, "batch_crawl"));
        for element in batch_crawl_sample.as_array().unwrap() {
            assert_serialized_matches_schema("batch_crawl", &crawl_item_schema, element);
        }
    }

    /// The advertised `outputSchema` JSON object for a tool by name.
    fn output_schema_of(tools: &[Tool], name: &str) -> serde_json::Map<String, serde_json::Value> {
        let tool = tools
            .iter()
            .find(|tool| tool.name.as_ref() == name)
            .unwrap_or_else(|| panic!("tool `{name}` missing from router"));
        let schema = tool
            .output_schema
            .as_ref()
            .unwrap_or_else(|| panic!("tool `{name}` advertises no outputSchema"));
        (**schema).clone()
    }

    /// Resolve the element schema of an array-typed `outputSchema`, following a
    /// top-level `items` `$ref` into `$defs`.
    fn resolve_item_schema(
        array_schema: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let items = array_schema
            .get("items")
            .expect("array outputSchema must declare `items`");
        if let Some(reference) = items.get("$ref").and_then(serde_json::Value::as_str) {
            let name = reference
                .strip_prefix("#/$defs/")
                .unwrap_or_else(|| panic!("unexpected $ref form: {reference}"));
            array_schema
                .get("$defs")
                .and_then(|defs| defs.get(name))
                .and_then(serde_json::Value::as_object)
                .cloned()
                .unwrap_or_else(|| panic!("$defs is missing `{name}`"))
        } else {
            items
                .as_object()
                .cloned()
                .unwrap_or_else(|| panic!("array `items` is neither a $ref nor an inline object schema"))
        }
    }

    /// Assert a serialized object conforms to an object schema: no undeclared
    /// fields, and all required properties present.
    fn assert_serialized_matches_schema(
        name: &str,
        object_schema: &serde_json::Map<String, serde_json::Value>,
        sample: &serde_json::Value,
    ) {
        let properties: std::collections::BTreeSet<&str> = object_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .map(|props| props.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let object = sample
            .as_object()
            .unwrap_or_else(|| panic!("tool `{name}`: sample structuredContent is not a JSON object"));

        for key in object.keys() {
            assert!(
                properties.contains(key.as_str()),
                "tool `{name}`: serialized field `{key}` is not declared in outputSchema.properties (schema/output drift)"
            );
        }
        if let Some(required) = object_schema.get("required").and_then(serde_json::Value::as_array) {
            for req in required.iter().filter_map(serde_json::Value::as_str) {
                assert!(
                    object.contains_key(req),
                    "tool `{name}`: outputSchema requires `{req}` but it is absent from serialized output (drift)"
                );
            }
        }
    }

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
