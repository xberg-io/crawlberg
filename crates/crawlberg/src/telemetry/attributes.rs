//! Semantic-convention attribute keys for crawlberg telemetry.
//!
//! All upstream OTEL semconv constants are re-exported here so call sites
//! import from a single location rather than reaching into
//! `opentelemetry_semantic_conventions` directly.  Extension constants in the
//! `crawl.*` namespace are defined as `pub const` strings below.
//!
//! Note: `HTTP_RESPONSE_BODY_SIZE` and `URL_DOMAIN` are stable in the semconv
//! attribute registry but gated behind `semconv_experimental` in the trace
//! re-export module.  We import them directly from `attribute` to avoid
//! requiring that Cargo feature flag.

pub use opentelemetry_semantic_conventions::trace::{
    HTTP_REQUEST_METHOD, HTTP_RESPONSE_STATUS_CODE, NETWORK_PROTOCOL_NAME, SERVER_ADDRESS, URL_FULL,
};

/// `http.response.body.size` — size of the HTTP response body in bytes.
pub const HTTP_RESPONSE_BODY_SIZE: &str = "http.response.body.size";

/// `url.domain` — domain part of the request URL.
pub const URL_DOMAIN: &str = "url.domain";

/// Number of seed URLs in the crawl job.
pub const CRAWL_SEED_COUNT: &str = "crawl.seed_count";
/// Configured maximum crawl depth.
pub const CRAWL_MAX_DEPTH: &str = "crawl.max_depth";
/// Configured maximum number of pages to crawl.
pub const CRAWL_MAX_PAGES: &str = "crawl.max_pages";
/// Crawl strategy name (e.g. `bfs`, `dfs`, `best_first`).
pub const CRAWL_STRATEGY: &str = "crawl.strategy";
/// Browser mode in effect (e.g. `never`, `always`, `on_demand`, `stealth`).
pub const CRAWL_BROWSER_MODE: &str = "crawl.browser_mode";
/// Depth of the current URL being processed.
pub const CRAWL_DEPTH: &str = "crawl.depth";
/// Number of URLs currently in the frontier.
pub const CRAWL_FRONTIER_SIZE: &str = "crawl.frontier_size";
/// Number of pages successfully completed so far.
pub const CRAWL_PAGES_COMPLETED: &str = "crawl.pages_completed";
/// Parent URL from which the current link was discovered.
pub const CRAWL_PARENT_URL: &str = "crawl.parent_url";
/// Link type (e.g. `internal`, `external`, `document`).
pub const CRAWL_LINK_TYPE: &str = "crawl.link_type";
/// Dispatch tier (e.g. `http`, `browser`).
pub const CRAWL_TIER: &str = "crawl.tier";
/// Final URL after redirects.
pub const CRAWL_FINAL_URL: &str = "crawl.final_url";
/// MIME type of the fetched resource.
pub const CRAWL_MIME_TYPE: &str = "crawl.mime_type";
/// Browser backend used for rendering (e.g. `chromiumoxide`, `native`).
pub const CRAWL_BROWSER_BACKEND: &str = "crawl.browser.backend";
/// Opaque browser session identifier.
pub const CRAWL_BROWSER_SESSION_ID: &str = "crawl.browser.session_id";
/// Number of pages rendered in the current browser session.
pub const CRAWL_PAGES_RENDERED: &str = "crawl.pages_rendered";
/// Hostname being checked against robots.txt.
pub const CRAWL_HOST: &str = "crawl.host";
/// Whether the robots.txt check allowed the URL.
pub const CRAWL_ALLOWED: &str = "crawl.allowed";
/// Size of the downloaded resource in bytes.
pub const CRAWL_SIZE_BYTES: &str = "crawl.size_bytes";
