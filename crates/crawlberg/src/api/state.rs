//! Shared API state.

use std::sync::Arc;
use std::time::Duration;

use crate::engine::CrawlEngine;

use super::jobs::JobRegistry;

/// Default time-to-live for jobs (1 hour).
const DEFAULT_JOB_MAX_AGE: Duration = Duration::from_secs(3600);

/// Environment variable holding the bearer token required to call the API.
/// Unset disables authentication; [`crate::api::startup`] additionally refuses
/// to bind a non-loopback address in that configuration unless
/// [`ALLOW_INSECURE_BIND_ENV`] is set, so "no token configured" cannot
/// silently expose an unauthenticated crawler to the network.
pub(crate) const AUTH_TOKEN_ENV: &str = "CRAWLBERG_API_TOKEN";

/// Environment variable that explicitly opts into starting the API without
/// authentication on a non-loopback bind address. Accepts `1` or `true`
/// (case-insensitive); any other value (including unset) keeps the refusal.
pub(crate) const ALLOW_INSECURE_BIND_ENV: &str = "CRAWLBERG_API_ALLOW_INSECURE";

/// Environment variable listing allowed CORS origins, comma-separated (e.g.
/// `https://a.example.com,https://b.example.com`), or the literal `*` to
/// explicitly opt into allowing any origin. Unset means no
/// `Access-Control-Allow-Origin` header is ever sent, so browsers enforce
/// same-origin by default.
pub(crate) const CORS_ORIGINS_ENV: &str = "CRAWLBERG_API_CORS_ORIGINS";

/// Environment variable overriding the per-request `max_pages` ceiling.
pub(crate) const MAX_PAGES_ENV: &str = "CRAWLBERG_API_MAX_PAGES";

/// Environment variable overriding the per-request batch URL count ceiling.
pub(crate) const MAX_BATCH_URLS_ENV: &str = "CRAWLBERG_API_MAX_BATCH_URLS";

/// Environment variable overriding the concurrently-active job ceiling.
pub(crate) const MAX_CONCURRENT_JOBS_ENV: &str = "CRAWLBERG_API_MAX_CONCURRENT_JOBS";

/// Default ceiling on `max_pages` per crawl/batch-crawl request. Matches the
/// MCP server's hard cap so REST and MCP enforce the same limit unless an
/// operator overrides it via [`MAX_PAGES_ENV`].
const DEFAULT_MAX_PAGES: usize = 100_000;

/// Default ceiling on the number of URLs accepted in a single batch request.
const DEFAULT_MAX_BATCH_URLS: usize = 500;

/// Default ceiling on concurrently active (pending/in-progress) jobs.
const DEFAULT_MAX_CONCURRENT_JOBS: usize = 50;

/// Server-side authentication and resource-limit configuration for the REST API.
///
/// Resolved from environment variables at startup via [`ApiSecurityConfig::from_env`].
/// Tests construct this directly instead of mutating process environment, which keeps
/// them independent and safe to run in parallel.
#[derive(Clone, Debug)]
pub struct ApiSecurityConfig {
    /// Bearer token required in `Authorization: Bearer <token>`. `None` disables auth.
    pub auth_token: Option<Arc<str>>,
    /// Hard ceiling on `max_pages` accepted from a crawl/batch-crawl request.
    pub max_pages_ceiling: usize,
    /// Hard ceiling on the number of URLs accepted in a batch request.
    pub max_batch_urls: usize,
    /// Hard ceiling on concurrently active (pending/in-progress) jobs.
    pub max_concurrent_jobs: usize,
}

impl ApiSecurityConfig {
    /// Resolve configuration from environment variables, falling back to safe defaults.
    pub fn from_env() -> Self {
        Self {
            auth_token: std::env::var(AUTH_TOKEN_ENV)
                .ok()
                .filter(|token| !token.is_empty())
                .map(Arc::from),
            max_pages_ceiling: env_usize(MAX_PAGES_ENV, DEFAULT_MAX_PAGES),
            max_batch_urls: env_usize(MAX_BATCH_URLS_ENV, DEFAULT_MAX_BATCH_URLS),
            max_concurrent_jobs: env_usize(MAX_CONCURRENT_JOBS_ENV, DEFAULT_MAX_CONCURRENT_JOBS),
        }
    }
}

impl Default for ApiSecurityConfig {
    fn default() -> Self {
        Self {
            auth_token: None,
            max_pages_ceiling: DEFAULT_MAX_PAGES,
            max_batch_urls: DEFAULT_MAX_BATCH_URLS,
            max_concurrent_jobs: DEFAULT_MAX_CONCURRENT_JOBS,
        }
    }
}

/// Parse a positive `usize` from an environment variable, falling back to `default`
/// when the variable is unset, not a valid `usize`, or zero.
fn env_usize(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

/// Shared state for all API handlers.
#[derive(Clone)]
pub struct ApiState {
    /// The crawl engine used to execute scrape, crawl, and map operations.
    pub engine: Arc<CrawlEngine>,
    /// Registry of asynchronous crawl and batch scrape jobs.
    pub jobs: Arc<JobRegistry>,
    /// Authentication and resource-limit configuration for this server instance.
    pub security: ApiSecurityConfig,
}

impl ApiState {
    /// Create a new `ApiState` wrapping the given engine with explicit security
    /// configuration.
    ///
    /// This also spawns a background task that periodically evicts expired jobs
    /// from the registry (default TTL: 1 hour). Callers resolve
    /// [`ApiSecurityConfig`] once up front — from the environment in production
    /// (see [`ApiSecurityConfig::from_env`], used by `startup::serve`) or
    /// explicitly in tests.
    pub fn with_security(engine: Arc<CrawlEngine>, security: ApiSecurityConfig) -> Self {
        let jobs = Arc::new(JobRegistry::new());
        jobs.spawn_eviction_task(DEFAULT_JOB_MAX_AGE);
        Self { engine, jobs, security }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_usize_falls_back_to_default_when_unset_or_invalid() {
        assert_eq!(env_usize("CRAWLBERG_TEST_UNSET_VAR_XYZ", 42), 42);
    }

    #[test]
    fn default_security_config_has_no_auth_token() {
        let config = ApiSecurityConfig::default();
        assert!(
            config.auth_token.is_none(),
            "default config must not enable auth implicitly"
        );
        assert_eq!(config.max_pages_ceiling, DEFAULT_MAX_PAGES);
        assert_eq!(config.max_batch_urls, DEFAULT_MAX_BATCH_URLS);
        assert_eq!(config.max_concurrent_jobs, DEFAULT_MAX_CONCURRENT_JOBS);
    }
}
