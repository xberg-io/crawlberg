//! Transport wiring: the stdio server entry points and the Streamable HTTP service.

use rmcp::ServiceExt;
use rmcp::task_manager::TaskManager;
use rmcp::transport::stdio;

use super::server::CrawlbergMcp;
use crate::types::CrawlConfig;

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
