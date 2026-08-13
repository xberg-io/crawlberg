//! API server startup functions.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use crate::engine::CrawlEngine;
use crate::error::CrawlError;
use crate::types::CrawlConfig;

use super::router::create_router_with_security;
use super::state::{ALLOW_INSECURE_BIND_ENV, ApiSecurityConfig};

/// Start the API server with a pre-built engine.
///
/// # Arguments
///
/// * `host` - IP address to bind to (e.g. `"127.0.0.1"` or `"0.0.0.0"`)
/// * `port` - Port number to bind to
/// * `engine` - A shared [`CrawlEngine`] that powers all operations
///
/// # Errors
///
/// Returns a [`CrawlError`] if the address is invalid, the bind address requires
/// authentication that isn't configured (see [`ensure_bind_is_safe`]), or the
/// server fails to start.
pub async fn serve(host: &str, port: u16, engine: Arc<CrawlEngine>) -> Result<(), CrawlError> {
    let ip: IpAddr = host
        .parse()
        .map_err(|e| CrawlError::invalid_config(format!("invalid host address: {e}")))?;

    let security = ApiSecurityConfig::from_env();
    ensure_bind_is_safe(&ip, &security)?;

    let addr = SocketAddr::new(ip, port);
    let app = create_router_with_security(engine, security);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| CrawlError::other(format!("failed to bind {addr}: {e}")))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| CrawlError::other(format!("server error: {e}")))?;

    Ok(())
}

/// Refuse to bind a non-loopback address without authentication configured.
///
/// ~keep Deliberate safe default: an unauthenticated crawler reachable from the
/// ~keep network is an abuse/SSRF-as-a-service vector. Loopback binds
/// ~keep (127.0.0.1/::1) stay open by default for local development, matching the
/// ~keep historical behavior for that case. Any other bind address requires either
/// ~keep `CRAWLBERG_API_TOKEN` or an explicit `CRAWLBERG_API_ALLOW_INSECURE=1`
/// ~keep opt-out, so misconfiguration (e.g. `--host 0.0.0.0` with no token) fails
/// ~keep fast with a clear error instead of silently exposing the API.
fn ensure_bind_is_safe(ip: &IpAddr, security: &ApiSecurityConfig) -> Result<(), CrawlError> {
    if bind_is_safe(ip, security.auth_token.is_some(), allow_insecure_bind_opt_in()) {
        return Ok(());
    }

    Err(CrawlError::invalid_config(format!(
        "refusing to bind {ip} without authentication: set {token_env}, bind to a loopback address \
         (127.0.0.1/::1), or set {allow_env}=1 to explicitly opt out",
        token_env = super::state::AUTH_TOKEN_ENV,
        allow_env = ALLOW_INSECURE_BIND_ENV,
    )))
}

/// Pure decision logic for [`ensure_bind_is_safe`], factored out so tests can
/// exercise it deterministically instead of mutating process environment variables.
fn bind_is_safe(ip: &IpAddr, has_auth_token: bool, allow_insecure_opt_in: bool) -> bool {
    has_auth_token || ip.is_loopback() || allow_insecure_opt_in
}

/// Read the [`ALLOW_INSECURE_BIND_ENV`] opt-out. Accepts `1` or `true` (case-insensitive).
fn allow_insecure_bind_opt_in() -> bool {
    std::env::var(ALLOW_INSECURE_BIND_ENV)
        .map(|value| value.eq_ignore_ascii_case("1") || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Start the API server with a [`CrawlConfig`], building the engine internally.
///
/// Convenience wrapper that constructs a default [`CrawlEngine`] from the given config.
///
/// # Arguments
///
/// * `host` - IP address to bind to
/// * `port` - Port number to bind to
/// * `config` - Crawl configuration for the engine
///
/// # Errors
///
/// Returns a [`CrawlError`] if the config is invalid, the address cannot be
/// parsed, or the server fails to start.
pub async fn serve_with_config(host: &str, port: u16, config: CrawlConfig) -> Result<(), CrawlError> {
    let engine = CrawlEngine::builder().config(config).build()?;
    serve(host, port, Arc::new(engine)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_bind_is_safe_without_a_token() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(
            bind_is_safe(&loopback, false, false),
            "loopback binds must stay open by default"
        );
    }

    #[test]
    fn non_loopback_bind_requires_a_token_or_explicit_opt_out() {
        let any: IpAddr = "0.0.0.0".parse().unwrap();
        assert!(
            !bind_is_safe(&any, false, false),
            "a non-loopback bind with no token and no opt-out must be refused"
        );
        assert!(
            bind_is_safe(&any, true, false),
            "a configured token makes a non-loopback bind safe"
        );
        assert!(
            bind_is_safe(&any, false, true),
            "the explicit CRAWLBERG_API_ALLOW_INSECURE opt-out makes a non-loopback bind safe"
        );
    }

    #[test]
    fn ensure_bind_is_safe_returns_a_clear_error_message() {
        let any: IpAddr = "0.0.0.0".parse().unwrap();
        let err = ensure_bind_is_safe(&any, &ApiSecurityConfig::default())
            .expect_err("must refuse an unauthenticated non-loopback bind");
        let message = err.to_string();
        assert!(
            message.contains("CRAWLBERG_API_TOKEN"),
            "error must name the token env var: {message}"
        );
        assert!(
            message.contains("CRAWLBERG_API_ALLOW_INSECURE"),
            "error must name the opt-out env var: {message}"
        );
    }

    #[test]
    fn ensure_bind_is_safe_allows_loopback_by_default() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(ensure_bind_is_safe(&loopback, &ApiSecurityConfig::default()).is_ok());
    }
}
