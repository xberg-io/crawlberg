use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::AssetCategory;
use super::dispatch::DispatchProfile;
use crate::error::CrawlError;
use crate::net::SsrfPolicy;

/// Upper bound accepted for `CrawlConfig::max_depth`.
const MAX_CRAWL_DEPTH: usize = 100;

/// Upper bound accepted for `CrawlConfig::max_redirects`.
const MAX_REDIRECT_HOPS: usize = 100;

/// Proxy URL schemes reqwest can build a proxy from.
const SUPPORTED_PROXY_SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

/// Range a `CrawlConfig::retry_codes` entry must fall in to be a real HTTP status code.
const HTTP_STATUS_CODE_RANGE: std::ops::RangeInclusive<u16> = 100..=599;

/// Upper bound accepted for `CrawlConfig::retry_count`.
///
/// ~keep `retry_count` is caller-supplied through every language binding. `compute_backoff_ms`
/// ~keep caps each individual delay at `retry_max_delay_ms` (default 60_000ms), so no bounded
/// ~keep `retry_count` can produce a multi-day sleep through it whatever the exponent -- an
/// ~keep earlier version of this comment claimed attempt 30 was a 12.4-day sleep, which was
/// ~keep wrong by 100x and reachable only without the cap. The bound exists to reject
/// ~keep obviously-mistaken input: unbounded, one failing URL retries for as long as the caller
/// ~keep asked, at up to `retry_max_delay_ms` apart, stalling the crawl on it instead of failing
/// ~keep it and moving on.
const MAX_RETRY_COUNT: usize = 20;

/// Default for `CrawlConfig::retry_initial_delay_ms`.
const DEFAULT_RETRY_INITIAL_DELAY_MS: u64 = 100;

/// Default for `CrawlConfig::retry_max_delay_ms`.
const DEFAULT_RETRY_MAX_DELAY_MS: u64 = 60_000;

/// Default for `CrawlConfig::tracking_params`: the query-parameter name patterns
/// `strip_tracking_params` removes once it is enabled.
fn default_tracking_params() -> Vec<String> {
    vec![
        "utm_*".to_owned(),
        "fbclid".to_owned(),
        "gclid".to_owned(),
        "ref".to_owned(),
    ]
}
mod credentials;
mod primitives;
mod sections;

// ~keep These names are the binding-generator surface (`crates/crawlberg-{wasm,node,py,php,ffi}`)
// ~keep and are re-exported from `crate::types`; the submodule split must stay invisible to them.
pub use credentials::{AuthConfig, ProxyConfig};
pub use primitives::{
    BrowserBackend, BrowserMode, BrowserWait, ContentFilterKind, CrawlStrategyKind, DocumentContentEncoding,
    ExtractionMeta,
};
pub use sections::{BrowserConfig, ContentConfig};

pub(crate) use primitives::duration_ms;

/// Configuration for crawl, scrape, and map operations.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CrawlConfig {
    /// Maximum crawl depth (number of link hops from the start URL).
    pub max_depth: Option<usize>,
    /// Maximum number of pages to crawl.
    pub max_pages: Option<usize>,
    /// Maximum links enqueued from a single page. Defaults to 10000.
    ///
    /// Bounds the work one hostile or pathological page can create; links past the
    /// cap are dropped and a warning is logged.
    pub max_links_per_page: Option<usize>,
    /// Maximum number of concurrent requests.
    pub max_concurrent: Option<usize>,
    /// Traversal order. Defaults to breadth-first.
    ///
    /// A frontier or strategy set explicitly on `CrawlEngineBuilder` takes precedence over
    /// this field.
    pub crawl_strategy: CrawlStrategyKind,
    /// Content filter applied to each page. `None` keeps every page.
    ///
    /// A content filter set explicitly on `CrawlEngineBuilder` takes precedence.
    pub content_filter: Option<ContentFilterKind>,
    /// Query the BM25 content filter scores pages against. Required by `ContentFilterKind::Bm25`.
    pub bm25_query: Option<String>,
    /// Minimum BM25 score a page must reach to be kept. Defaults to `0.0`.
    pub bm25_threshold: Option<f64>,
    /// Whether to respect robots.txt directives.
    pub respect_robots_txt: bool,
    /// When true, HTTP-level error responses (404 NotFound, 403 Forbidden, WAF blocks)
    /// are surfaced as `ScrapeResult` records with the matching `status_code` rather
    /// than raised as `CrawlError`. Default `false` preserves the historical
    /// throw-on-error contract for direct fetches. Independently of this flag,
    /// 404s reached at the end of a redirect chain are *always* surfaced softly —
    /// the user opted into redirect-following, so receiving a 404 there is part of
    /// the normal flow rather than an unexpected error.
    #[serde(default)]
    pub soft_http_errors: bool,
    /// Custom user-agent string.
    pub user_agent: Option<String>,
    /// Whether to confine *document* links (`.pdf`, `.docx`, `.zip`, ...) to the seed domain.
    ///
    /// Page links are always confined to the seed host, widened to its subdomains by
    /// [`Self::allow_subdomains`]; this flag does not loosen that. It applies only to document
    /// links, which are classified by file extension before their host is considered and so
    /// are followed cross-host by default -- the usual case being documents served from a CDN
    /// or object store. Set this to `true` to require documents to live on the seed domain too.
    pub stay_on_domain: bool,
    /// Whether subdomains of the seed host are in scope.
    ///
    /// Applies to page links unconditionally, and to document links when
    /// [`Self::stay_on_domain`] is set.
    pub allow_subdomains: bool,
    /// Regex patterns for paths to include during crawling.
    #[serde(default)]
    pub include_paths: Vec<String>,
    /// Regex patterns for paths to exclude during crawling.
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    /// Whether `include_paths`/`exclude_paths` match against `path?query` instead of just
    /// `path`. Defaults to `false`, matching path only: a pattern anchored with `$` (e.g.
    /// `/feed/?$`) changes meaning once the query joins the matched text, so this must stay
    /// opt-in rather than silently changing what an existing config matches.
    #[serde(default)]
    pub path_patterns_match_query: bool,
    /// Whether the crawl-dedup key includes the (sorted) query string. Defaults to `false`,
    /// matching historical behavior: `/item?id=1` and `/item?id=2` are treated as one page and
    /// only the first is fetched. `true` keeps the query, sorted, in the key, so each distinct
    /// query is fetched once.
    #[serde(default)]
    pub dedup_include_query: bool,
    /// Whether to strip `tracking_params` from a discovered URL before it is deduplicated,
    /// fetched, and reported. Defaults to `false`, so no tracking parameters are stripped
    /// unless explicitly enabled.
    #[serde(default)]
    pub strip_tracking_params: bool,
    /// Query parameter name patterns to strip when `strip_tracking_params` is `true`. A
    /// pattern ending in `*` matches by prefix (`utm_*` matches `utm_source`, `utm_campaign`,
    /// ...); any other pattern matches the parameter name exactly. Defaults to
    /// `["utm_*", "fbclid", "gclid", "ref"]`, applied only once `strip_tracking_params` is
    /// enabled.
    #[serde(default = "default_tracking_params")]
    pub tracking_params: Vec<String>,
    /// Custom HTTP headers to send with each request.
    #[serde(default)]
    pub custom_headers: HashMap<String, String>,
    /// Timeout for individual HTTP requests (in milliseconds when serialized).
    #[serde(with = "duration_ms")]
    pub request_timeout: Duration,
    /// Per-domain rate limit in milliseconds. When set, enforces a minimum delay
    /// between requests to the same domain. Defaults to 200ms when `None`.
    pub rate_limit_ms: Option<u64>,
    /// Maximum number of redirects to follow.
    pub max_redirects: usize,
    /// Number of retry attempts for failed requests. Bounded by [`MAX_RETRY_COUNT`].
    pub retry_count: usize,
    /// HTTP status codes that should trigger a retry.
    #[serde(default)]
    pub retry_codes: Vec<u16>,
    /// Initial delay, in milliseconds, before the first retry. Doubled on each
    /// subsequent attempt (capped at `retry_max_delay_ms`). Defaults to 100ms.
    pub retry_initial_delay_ms: u64,
    /// Upper bound, in milliseconds, on the exponential retry backoff. Defaults to 60s.
    pub retry_max_delay_ms: u64,
    /// Fraction of the per-domain rate-limit delay to randomly jitter by, in `[0.0, 1.0]`.
    /// `0.0` (the default) applies no jitter and preserves the previous fixed-interval
    /// behaviour; `0.1` jitters the delay by up to ±10%.
    pub rate_limit_jitter_ratio: f64,
    /// Whether to enable cookie handling.
    pub cookies_enabled: bool,
    /// Authentication configuration.
    pub auth: Option<AuthConfig>,
    /// Maximum response body size in bytes.
    ///
    /// `None` does not mean unbounded: an unset cap falls back to a 100 MiB safety
    /// ceiling, because HTTP responses are decompressed while being read and a few
    /// hundred compressed bytes can otherwise expand to gigabytes in memory. To read
    /// bodies larger than that, set this explicitly.
    pub max_body_size: Option<usize>,
    /// CSS selectors for tags to remove from HTML before processing.
    #[serde(default)]
    pub remove_tags: Vec<String>,
    /// Content extraction and conversion configuration.
    #[serde(default)]
    pub content: ContentConfig,
    /// Maximum number of URLs to return from a map operation.
    pub map_limit: Option<usize>,
    /// Search filter for map results (case-insensitive substring match on URLs).
    pub map_search: Option<String>,
    /// Whether to download assets (CSS, JS, images, etc.) from the page.
    pub download_assets: bool,
    /// Filter for asset categories to download.
    #[serde(default)]
    pub asset_types: Vec<AssetCategory>,
    /// Maximum size in bytes for individual asset downloads.
    pub max_asset_size: Option<usize>,
    /// Browser configuration.
    #[serde(default)]
    pub browser: BrowserConfig,
    /// Proxy configuration for HTTP requests.
    pub proxy: Option<ProxyConfig>,
    /// List of user-agent strings for rotation. If non-empty, overrides `user_agent`.
    #[serde(default)]
    pub user_agents: Vec<String>,
    /// Whether to capture a screenshot when using the browser.
    ///
    /// Only supported by `scrape()` with `BrowserBackend::Chromiumoxide` and
    /// `BrowserMode::Always` or `Stealth`. A screenshot is 100–500 KB of PNG per page,
    /// so `crawl()` does not carry screenshots in `CrawlPageResult`/`CrawlResult` at
    /// all — a multi-thousand-page crawl holding one per page in memory is not a safe
    /// default. Setting this with any other configuration (a different backend,
    /// `BrowserMode::Auto`/`Never`, or during `crawl()`) has no effect and logs a
    /// warning rather than silently doing nothing.
    pub capture_screenshot: bool,
    /// Re-enqueue discovered `LinkType::Document` URLs into the crawl frontier so
    /// the crawl follows links *from* document pages (PDFs, etc.) as it would
    /// from HTML pages. Default: `false` (documents terminate at materialisation).
    #[serde(default)]
    pub follow_document_urls: bool,
    /// Maximum document-depth (from the seed URL through document links only)
    /// when `follow_document_urls` is true. `None` means inherit `max_depth`.
    /// Independent of `max_depth`: a document URL is enqueued only if BOTH the
    /// outer `max_depth` and (if set) `document_url_depth` permit it.
    #[serde(default)]
    pub document_url_depth: Option<u32>,
    /// Whether to download non-HTML documents (PDF, DOCX, images, code, etc.) instead of skipping them.
    /// Defaults to `true` — unlike `download_assets` and `capture_screenshot`, which default to `false`.
    pub download_documents: bool,
    /// Maximum size in bytes for document downloads. Defaults to 50 MB.
    pub document_max_size: Option<usize>,
    /// Allowlist of MIME types to download. If empty, uses built-in defaults.
    #[serde(default)]
    pub document_mime_types: Vec<String>,
    /// Directory to stream downloaded document bytes into instead of holding them in
    /// memory on `DownloadedDocument.content`. When set, `content` is left empty and
    /// `DownloadedDocument.content_path` is populated with `<dir>/<content_hash>.<ext>`.
    /// `None` (default) preserves today's in-memory-only behavior. Has no effect on
    /// wasm32, which has no filesystem — use `document_content_encoding` there instead.
    #[serde(default)]
    pub document_output_dir: Option<PathBuf>,
    /// Opt-in encoding that duplicates `DownloadedDocument.content` into a serializable
    /// field for language bindings that need the bytes in-memory (`content` itself is
    /// `alef(skip)`ed). `None` (default) means no encoding is produced. Independent of
    /// `document_output_dir` — set both to get a file on disk and an in-memory copy.
    #[serde(default)]
    pub document_content_encoding: Option<DocumentContentEncoding>,
    /// Path to write WARC output. If `None`, WARC output is disabled.
    pub warc_output: Option<PathBuf>,
    /// Named browser profile for persistent sessions (cookies, localStorage).
    ///
    /// Chromiumoxide backend only. The native backend runs an in-process JavaScript
    /// engine with no Chrome process and therefore no profile directory, so this is
    /// ignored there and logs a warning. It is also ignored — with a warning — when a
    /// shared browser pool is in use (the pool launches before any per-crawl config
    /// exists) or when connecting to an external CDP endpoint whose process crawlberg
    /// does not own.
    pub browser_profile: Option<String>,
    /// Whether to save changes back to the browser profile on exit.
    pub save_browser_profile: bool,
    /// SSRF policy for outbound network requests. Default: deny private networks,
    /// allow http/https only, max 5 redirects.
    ///
    /// All policy fields are exposed to language bindings.
    ///
    /// **wasm32 (including Node.js): `deny_private` does not stop hostname-based
    /// requests.** There is no DNS resolution on this target, so only a literal IP host is
    /// checked against the policy — a domain name is always permitted, regardless of
    /// `deny_private`. Under Node, where `fetch` enforces no CORS, this means a service
    /// embedding the wasm binding can be driven to internal hosts by domain name even with
    /// `deny_private = true`. Enforce egress restrictions at the network layer for that
    /// deployment target; do not rely on this field. See `crawlberg::net::validate_url`.
    #[serde(default = "SsrfPolicy::from_env")]
    pub ssrf: SsrfPolicy,
    /// Pins [`SsrfPolicy::deny_private`] to a caller-chosen value, bypassing the
    /// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` operator override entirely for this config.
    ///
    /// `ssrf.deny_private` is a plain, always-serialized `bool`: several alef-generated
    /// bindings construct `SsrfPolicy::default()` (hardcoding `deny_private: true`)
    /// whenever their caller never touches SSRF settings at all, so `true` on that field
    /// alone cannot distinguish "the caller wants private networks denied" from "the
    /// binding's own structural default landed on `true`". The environment variable
    /// exists precisely to resolve that ambiguity in the common case by treating any
    /// `true` as inconclusive and deferring to the operator.
    ///
    /// Set this field when that default-deferral is wrong for your call — e.g. a test
    /// that must prove `deny_private: true` still denies even while the operator has set
    /// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` suite-wide for every other call. `None` (default)
    /// preserves today's behavior: the environment variable may still flip
    /// `ssrf.deny_private` to `false`. `Some(value)` pins `ssrf.deny_private` to `value`
    /// and the environment variable is not consulted for this config.
    #[serde(default)]
    pub ssrf_deny_private_explicit: Option<bool>,
    /// Pluggable dispatch components: bypass provider, escalation strategy,
    /// retry policy, WAF classifier, domain state, escalation budget, and
    /// max_total_attempts.
    ///
    /// When `None`, the engine uses its built-in defaults (no bypass, `BrowserOnly`
    /// strategy, `SimpleRetryPolicy`, built-in WAF classifier, no domain state,
    /// unlimited budget, 10 total attempt cap).
    ///
    /// Rust-only advanced field. Generated language bindings do not expose
    /// pluggable dispatch components; language clients use the built-in
    /// dispatch defaults configured by the Rust engine.
    ///
    /// Not serializable — Rust callers construct this at runtime and skip it
    /// in TOML/JSON configs.
    #[serde(skip)]
    #[cfg_attr(alef, alef(skip))]
    pub dispatch: Option<DispatchProfile>,
    /// Shared browser pool for reusing Chrome across requests (not serializable).
    #[cfg(feature = "browser")]
    #[serde(skip)]
    #[cfg_attr(alef, alef(skip))]
    pub browser_pool: Option<std::sync::Arc<crate::browser_pool::BrowserPool>>,
    /// Optional [`crate::ProxyProvider`] for per-request proxy rotation on the
    /// reqwest HTTP path. Takes precedence over the static [`ProxyConfig`] in
    /// `proxy` when set. Not serializable — Rust callers inject at runtime.
    #[serde(skip)]
    pub proxy_provider: Option<std::sync::Arc<dyn crate::ProxyProvider>>,
    /// Shared browser session pool for session affinity (not serializable).
    /// When set alongside `session_affinity: true` in BrowserConfig, the pool
    /// is used to cache Pages by (domain, proxy) so cookies and fingerprint
    /// persist across requests.
    #[cfg(feature = "browser")]
    #[serde(skip)]
    #[cfg_attr(alef, alef(skip))]
    pub browser_session_pool: Option<std::sync::Arc<crate::browser_session_pool::BrowserSessionPool>>,
}

impl std::fmt::Debug for CrawlConfig {
    /// Redacted: `custom_headers` often carries an `Authorization` or API key header, so
    /// its values print as `***`. `auth`, `proxy` and `browser` redact their own secrets.
    /// The exhaustive destructure makes a new field a compile error here, not a silent gap.
    // ~keep alef extracts public inherent AND trait-impl methods; `Formatter` has no
    // binding representation, so without this the surface fails generation with
    // lossy_sanitized_surface.
    #[cfg_attr(alef, alef(skip))]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            max_depth,
            max_pages,
            max_links_per_page,
            max_concurrent,
            crawl_strategy,
            content_filter,
            bm25_query,
            bm25_threshold,
            respect_robots_txt,
            soft_http_errors,
            user_agent,
            stay_on_domain,
            allow_subdomains,
            include_paths,
            exclude_paths,
            path_patterns_match_query,
            dedup_include_query,
            strip_tracking_params,
            tracking_params,
            custom_headers,
            request_timeout,
            rate_limit_ms,
            max_redirects,
            retry_count,
            retry_codes,
            retry_initial_delay_ms,
            retry_max_delay_ms,
            rate_limit_jitter_ratio,
            cookies_enabled,
            auth,
            max_body_size,
            remove_tags,
            content,
            map_limit,
            map_search,
            download_assets,
            asset_types,
            max_asset_size,
            browser,
            proxy,
            user_agents,
            capture_screenshot,
            follow_document_urls,
            document_url_depth,
            download_documents,
            document_max_size,
            document_mime_types,
            document_output_dir,
            document_content_encoding,
            warc_output,
            browser_profile,
            save_browser_profile,
            ssrf,
            ssrf_deny_private_explicit,
            dispatch,
            #[cfg(feature = "browser")]
            browser_pool,
            proxy_provider,
            #[cfg(feature = "browser")]
            browser_session_pool,
        } = self;
        let mut debug = f.debug_struct("CrawlConfig");
        debug.field("max_depth", max_depth);
        debug.field("max_pages", max_pages);
        debug.field("max_links_per_page", max_links_per_page);
        debug.field("max_concurrent", max_concurrent);
        debug.field("crawl_strategy", crawl_strategy);
        debug.field("content_filter", content_filter);
        debug.field("bm25_query", bm25_query);
        debug.field("bm25_threshold", bm25_threshold);
        debug.field("respect_robots_txt", respect_robots_txt);
        debug.field("soft_http_errors", soft_http_errors);
        debug.field("user_agent", user_agent);
        debug.field("stay_on_domain", stay_on_domain);
        debug.field("allow_subdomains", allow_subdomains);
        debug.field("include_paths", include_paths);
        debug.field("exclude_paths", exclude_paths);
        debug.field("path_patterns_match_query", path_patterns_match_query);
        debug.field("dedup_include_query", dedup_include_query);
        debug.field("strip_tracking_params", strip_tracking_params);
        debug.field("tracking_params", tracking_params);
        debug.field("custom_headers", &crate::net::redact::RedactedValues(custom_headers));
        debug.field("request_timeout", request_timeout);
        debug.field("rate_limit_ms", rate_limit_ms);
        debug.field("max_redirects", max_redirects);
        debug.field("retry_count", retry_count);
        debug.field("retry_codes", retry_codes);
        debug.field("retry_initial_delay_ms", retry_initial_delay_ms);
        debug.field("retry_max_delay_ms", retry_max_delay_ms);
        debug.field("rate_limit_jitter_ratio", rate_limit_jitter_ratio);
        debug.field("cookies_enabled", cookies_enabled);
        debug.field("auth", auth);
        debug.field("max_body_size", max_body_size);
        debug.field("remove_tags", remove_tags);
        debug.field("content", content);
        debug.field("map_limit", map_limit);
        debug.field("map_search", map_search);
        debug.field("download_assets", download_assets);
        debug.field("asset_types", asset_types);
        debug.field("max_asset_size", max_asset_size);
        debug.field("browser", browser);
        debug.field("proxy", proxy);
        debug.field("user_agents", user_agents);
        debug.field("capture_screenshot", capture_screenshot);
        debug.field("follow_document_urls", follow_document_urls);
        debug.field("document_url_depth", document_url_depth);
        debug.field("download_documents", download_documents);
        debug.field("document_max_size", document_max_size);
        debug.field("document_mime_types", document_mime_types);
        debug.field("document_output_dir", document_output_dir);
        debug.field("document_content_encoding", document_content_encoding);
        debug.field("warc_output", warc_output);
        debug.field("browser_profile", browser_profile);
        debug.field("save_browser_profile", save_browser_profile);
        debug.field("ssrf", ssrf);
        debug.field("ssrf_deny_private_explicit", ssrf_deny_private_explicit);
        debug.field("dispatch", dispatch);
        #[cfg(feature = "browser")]
        debug.field("browser_pool", browser_pool);
        debug.field("proxy_provider", proxy_provider);
        #[cfg(feature = "browser")]
        debug.field("browser_session_pool", browser_session_pool);
        debug.finish()
    }
}

impl Default for CrawlConfig {
    fn default() -> Self {
        Self {
            max_depth: None,
            max_pages: None,
            max_links_per_page: None,
            max_concurrent: None,
            crawl_strategy: CrawlStrategyKind::Bfs,
            content_filter: None,
            bm25_query: None,
            bm25_threshold: None,
            respect_robots_txt: false,
            soft_http_errors: false,
            user_agent: None,
            stay_on_domain: false,
            allow_subdomains: false,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            path_patterns_match_query: false,
            dedup_include_query: false,
            strip_tracking_params: false,
            tracking_params: default_tracking_params(),
            custom_headers: HashMap::new(),
            request_timeout: Duration::from_secs(30),
            rate_limit_ms: None,
            max_redirects: 10,
            retry_count: 0,
            retry_codes: Vec::new(),
            retry_initial_delay_ms: DEFAULT_RETRY_INITIAL_DELAY_MS,
            retry_max_delay_ms: DEFAULT_RETRY_MAX_DELAY_MS,
            rate_limit_jitter_ratio: 0.0,
            cookies_enabled: false,
            auth: None,
            max_body_size: None,
            remove_tags: Vec::new(),
            content: ContentConfig::default(),
            map_limit: None,
            map_search: None,
            download_assets: false,
            asset_types: Vec::new(),
            max_asset_size: None,
            browser: BrowserConfig::default(),
            proxy: None,
            user_agents: Vec::new(),
            capture_screenshot: false,
            follow_document_urls: false,
            document_url_depth: None,
            download_documents: true,
            document_max_size: Some(50 * 1024 * 1024),
            document_mime_types: Vec::new(),
            document_output_dir: None,
            document_content_encoding: None,
            warc_output: None,
            browser_profile: None,
            save_browser_profile: false,
            ssrf: SsrfPolicy::from_env(),
            ssrf_deny_private_explicit: None,
            dispatch: None,
            #[cfg(feature = "browser")]
            browser_pool: None,
            #[cfg(feature = "browser")]
            browser_session_pool: None,
            proxy_provider: None,
        }
    }
}

impl CrawlConfig {
    /// Start a fluent builder for `CrawlConfig`. See [`crate::CrawlConfigBuilder`].
    #[cfg_attr(alef, alef(skip))]
    pub fn builder() -> crate::types::builder::CrawlConfigBuilder {
        crate::types::builder::CrawlConfigBuilder::default()
    }

    /// Validate the configuration, returning an error if any values are invalid.
    ///
    /// Only the first violation encountered is reported, not every one, so fixing a rejected
    /// configuration can surface a further error on the next attempt.
    // ~keep Everything above is copied verbatim into all sixteen generated language bindings, so
    // ~keep it says what a caller in any language observes and nothing about Rust. The
    // ~keep maintenance facts live here instead: the sequence of checks below IS that observable
    // ~keep order, and `validate_reports_violations_in_a_fixed_order` walks all 16 violations to
    // ~keep pin it, so reordering these calls will fail that test rather than slip through.
    pub fn validate(&self) -> Result<(), crate::error::CrawlError> {
        self.validate_max_concurrent()?;
        self.validate_content_filter()?;
        self.validate_browser_wait()?;
        self.validate_traversal_limits()?;
        self.ssrf
            .validate_scheme_allowlist()
            .map_err(CrawlError::invalid_config)?;
        self.validate_max_body_size()?;
        self.validate_proxy()?;
        self.validate_auth()?;
        self.validate_path_patterns()?;
        self.validate_retry_codes()?;
        self.validate_retry_count()?;
        self.validate_request_timeout()?;
        self.validate_browser_endpoint()?;
        Ok(())
    }

    fn validate_max_concurrent(&self) -> Result<(), CrawlError> {
        if let Some(0) = self.max_concurrent {
            return Err(CrawlError::invalid_config("max_concurrent must be > 0"));
        }
        Ok(())
    }

    fn validate_content_filter(&self) -> Result<(), CrawlError> {
        // ~keep Reject rather than fall back to keeping every page: a filter that silently
        // does nothing looks identical to one that matched everything.
        if self.content_filter == Some(ContentFilterKind::Bm25) && self.bm25_query.is_none() {
            return Err(CrawlError::invalid_config(
                "bm25_query is required when content_filter is bm25",
            ));
        }
        Ok(())
    }

    fn validate_browser_wait(&self) -> Result<(), CrawlError> {
        if self.browser.wait == BrowserWait::Selector && self.browser.wait_selector.is_none() {
            return Err(CrawlError::invalid_config(
                "browser.wait_selector required when browser.wait is Selector",
            ));
        }
        Ok(())
    }

    fn validate_traversal_limits(&self) -> Result<(), CrawlError> {
        if let Some(max_depth) = self.max_depth
            && max_depth > MAX_CRAWL_DEPTH
        {
            return Err(CrawlError::invalid_config(format!(
                "max_depth must be <= {MAX_CRAWL_DEPTH} (got {max_depth})"
            )));
        }
        if let Some(max_pages) = self.max_pages
            && max_pages == 0
        {
            return Err(CrawlError::invalid_config("max_pages must be > 0"));
        }
        if self.max_redirects > MAX_REDIRECT_HOPS {
            return Err(CrawlError::invalid_config(format!(
                "max_redirects must be <= {MAX_REDIRECT_HOPS}"
            )));
        }
        Ok(())
    }

    fn validate_max_body_size(&self) -> Result<(), CrawlError> {
        if let Some(max_body_size) = self.max_body_size
            && max_body_size == 0
        {
            return Err(CrawlError::invalid_config("max_body_size must be > 0"));
        }
        Ok(())
    }

    fn validate_proxy(&self) -> Result<(), CrawlError> {
        let Some(ref proxy) = self.proxy else {
            return Ok(());
        };
        let parsed = url::Url::parse(&proxy.url).map_err(|e| {
            // ~keep This fires precisely when `Url::parse` fails, and the parsing redaction
            // ~keep helper returns its input unchanged in that case — so it would be a no-op
            // ~keep here. The textual strip is what actually removes `user:password@`.
            CrawlError::invalid_config(format!(
                "invalid proxy URL '{}': {e}",
                crate::net::redact::redact_userinfo_textually(&proxy.url)
            ))
        })?;
        let scheme = parsed.scheme();
        if !SUPPORTED_PROXY_SCHEMES.contains(&scheme) {
            return Err(CrawlError::invalid_config(format!(
                "invalid proxy URL scheme '{scheme}' (expected http, https, socks5, or socks5h)"
            )));
        }
        Ok(())
    }

    fn validate_auth(&self) -> Result<(), CrawlError> {
        let Some(ref auth) = self.auth else {
            return Ok(());
        };
        match auth {
            AuthConfig::Basic { username, .. } if username.is_empty() => {
                Err(CrawlError::invalid_config("auth.basic.username must not be empty"))
            }
            AuthConfig::Bearer { token } if token.is_empty() => {
                Err(CrawlError::invalid_config("auth.bearer.token must not be empty"))
            }
            AuthConfig::Header { name, value } if name.is_empty() || value.is_empty() => Err(
                CrawlError::invalid_config("auth.header.name and auth.header.value must not be empty"),
            ),
            _ => Ok(()),
        }
    }

    fn validate_path_patterns(&self) -> Result<(), CrawlError> {
        for pattern in &self.include_paths {
            regex::Regex::new(pattern)
                .map_err(|e| CrawlError::invalid_config(format!("invalid include_path regex '{pattern}': {e}")))?;
        }
        for pattern in &self.exclude_paths {
            regex::Regex::new(pattern)
                .map_err(|e| CrawlError::invalid_config(format!("invalid exclude_path regex '{pattern}': {e}")))?;
        }
        Ok(())
    }

    fn validate_retry_codes(&self) -> Result<(), CrawlError> {
        for &code in &self.retry_codes {
            if !HTTP_STATUS_CODE_RANGE.contains(&code) {
                return Err(CrawlError::invalid_config(format!("invalid retry code: {code}")));
            }
        }
        Ok(())
    }

    fn validate_retry_count(&self) -> Result<(), CrawlError> {
        if self.retry_count > MAX_RETRY_COUNT {
            return Err(CrawlError::invalid_config(format!(
                "retry_count must be <= {MAX_RETRY_COUNT} (got {})",
                self.retry_count
            )));
        }
        Ok(())
    }

    fn validate_request_timeout(&self) -> Result<(), CrawlError> {
        if self.request_timeout.is_zero() {
            return Err(CrawlError::invalid_config("request_timeout must be > 0"));
        }
        Ok(())
    }

    fn validate_browser_endpoint(&self) -> Result<(), CrawlError> {
        if let Some(ref endpoint) = self.browser.endpoint
            && !endpoint.starts_with("ws://")
            && !endpoint.starts_with("wss://")
        {
            // ~keep Do not echo the value, not even redacted: this fires exactly when the
            // ~keep endpoint is not `ws(s)://`, so `redact_url_to_origin` would print `***`
            // ~keep for it anyway, and echoing it raw would put a `?token=` or a
            // ~keep `/devtools/browser/<GUID>` into a `CrawlError` Display, and from there
            // ~keep into logs and API error bodies. The field name is enough to find it.
            return Err(CrawlError::invalid_config(
                "browser.endpoint must start with ws:// or wss://",
            ));
        }
        if self.browser.backend == BrowserBackend::Native && self.browser.endpoint.is_some() {
            return Err(CrawlError::invalid_config(
                "browser.endpoint is only supported by the chromiumoxide backend",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ~keep A field-level `#[serde(default)]` OVERRIDES the container-level one, substituting
    /// `FieldType::default()` for the value the struct's `Default` impl declares. On a struct that
    /// already carries `#[serde(default)]` the field attribute is therefore not redundant — it
    /// silently cancels the documented default. Deserialising is how every binding builds a config,
    /// so a disagreement here ships a different default to every non-Rust caller.
    fn assert_default_matches_empty_object<T>(name: &str)
    where
        T: Default + Serialize + serde::de::DeserializeOwned,
    {
        let from_impl = serde_json::to_value(T::default()).expect("serialize Default");
        let parsed: T = serde_json::from_str("{}").expect("deserialize empty object");
        let from_json = serde_json::to_value(parsed).expect("serialize deserialized");
        assert_eq!(
            from_impl, from_json,
            "{name}::default() and from_str(\"{{}}\") disagree; a field-level #[serde(default)] is \
             overriding the struct's Default impl"
        );
    }

    #[test]
    fn should_deserialize_empty_object_to_the_declared_default() {
        assert_default_matches_empty_object::<CrawlConfig>("CrawlConfig");
        assert_default_matches_empty_object::<ContentConfig>("ContentConfig");
        assert_default_matches_empty_object::<BrowserConfig>("BrowserConfig");
    }

    #[test]
    fn should_keep_nested_defaults_when_one_unrelated_field_is_set() {
        let config: CrawlConfig = serde_json::from_str(r#"{"content":{"remove_forms":true}}"#).expect("parse");
        assert_eq!(
            config.content.exclude_selectors,
            vec!["noscript".to_owned()],
            "setting one content field must not drop the other content defaults"
        );

        let config: CrawlConfig =
            serde_json::from_str(r#"{"browser":{"capture_network_events":true}}"#).expect("parse");
        assert!(
            config.browser.session_affinity,
            "setting one browser field must not turn off session_affinity, documented as default true"
        );
    }

    /// Characterization: `validate` reports the FIRST violation, and the order it checks
    /// rules in is observable behaviour — a config that breaks several rules gets exactly one
    /// message, and which one depends on the check order. Pinned here so the order survives
    /// any restructuring of `validate`. ~keep
    #[test]
    fn validate_reports_violations_in_a_fixed_order() {
        let mut config = maximally_invalid_config();

        for (position, (fragment, repair)) in ORDERED_VIOLATIONS.iter().enumerate() {
            let error = config
                .validate()
                .expect_err(&format!("violation {position} ({fragment}) must still be reported"))
                .to_string();
            assert!(
                error.contains(fragment),
                "violation {position}: expected an error containing {fragment:?}, got: {error}"
            );
            repair(&mut config);
        }

        config.validate().expect("every violation has been repaired");
    }

    /// Repairs the violation its table entry names, so the next check becomes reachable.
    type ConfigRepair = fn(&mut CrawlConfig);

    /// A config that breaks every rule `validate` enforces, at once.
    fn maximally_invalid_config() -> CrawlConfig {
        let mut config = CrawlConfig {
            max_concurrent: Some(0),
            content_filter: Some(ContentFilterKind::Bm25),
            bm25_query: None,
            max_depth: Some(101),
            max_pages: Some(0),
            max_redirects: 101,
            max_body_size: Some(0),
            proxy: Some(ProxyConfig {
                url: "ftp://proxy.internal:2121".into(),
                ..Default::default()
            }),
            auth: Some(AuthConfig::Bearer { token: String::new() }),
            include_paths: vec!["(unclosed".into()],
            exclude_paths: vec!["(unclosed".into()],
            retry_codes: vec![999],
            retry_count: MAX_RETRY_COUNT + 1,
            request_timeout: Duration::ZERO,
            browser: BrowserConfig {
                wait: BrowserWait::Selector,
                wait_selector: None,
                backend: BrowserBackend::Native,
                endpoint: Some("http://not-websocket:3000".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        config.ssrf.scheme_allowlist = vec!["ftp".to_owned()];
        config
    }

    /// Every violation `maximally_invalid_config` carries, in the order `validate` reports
    /// them, each paired with the repair that unblocks the next one.
    const ORDERED_VIOLATIONS: &[(&str, ConfigRepair)] = &[
        ("max_concurrent must be > 0", |c| c.max_concurrent = Some(1)),
        ("bm25_query is required when content_filter is bm25", |c| {
            c.bm25_query = Some("query".to_owned())
        }),
        ("browser.wait_selector required when browser.wait is Selector", |c| {
            c.browser.wait_selector = Some("#main".to_owned())
        }),
        ("max_depth must be <= 100 (got 101)", |c| c.max_depth = Some(100)),
        ("max_pages must be > 0", |c| c.max_pages = Some(1)),
        ("max_redirects must be <= 100", |c| c.max_redirects = 100),
        ("ssrf.scheme_allowlist contains unsupported scheme 'ftp'", |c| {
            c.ssrf.scheme_allowlist = vec!["https".to_owned()]
        }),
        ("max_body_size must be > 0", |c| c.max_body_size = Some(1)),
        ("invalid proxy URL scheme 'ftp'", |c| {
            c.proxy = Some(ProxyConfig {
                url: "http://proxy.internal:8080".into(),
                ..Default::default()
            })
        }),
        ("auth.bearer.token must not be empty", |c| {
            c.auth = Some(AuthConfig::Bearer {
                token: "token".to_owned(),
            })
        }),
        ("invalid include_path regex '(unclosed'", |c| {
            c.include_paths = vec!["^/docs".to_owned()]
        }),
        ("invalid exclude_path regex '(unclosed'", |c| {
            c.exclude_paths = vec!["^/private".to_owned()]
        }),
        ("invalid retry code: 999", |c| c.retry_codes = vec![503]),
        ("retry_count must be <= 20 (got 21)", |c| {
            c.retry_count = MAX_RETRY_COUNT
        }),
        ("request_timeout must be > 0", |c| {
            c.request_timeout = Duration::from_secs(30)
        }),
        ("browser.endpoint must start with ws:// or wss://", |c| {
            c.browser.endpoint = Some("ws://localhost:9222".to_owned())
        }),
        ("browser.endpoint is only supported by the chromiumoxide backend", |c| {
            c.browser.backend = BrowserBackend::Chromiumoxide
        }),
    ];

    #[test]
    fn validate_rejects_an_absurd_retry_count() {
        let config = CrawlConfig {
            retry_count: 1_000_000,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("retry_count must be <= 20"),
            "expected a retry_count bound error, got: {msg}"
        );
    }

    #[test]
    fn validate_accepts_the_maximum_allowed_retry_count() {
        let config = CrawlConfig {
            retry_count: 20,
            ..Default::default()
        };
        assert!(config.validate().is_ok(), "retry_count at the bound must be accepted");
    }

    #[test]
    fn validate_rejects_http_browser_endpoint() {
        let config = CrawlConfig {
            browser: BrowserConfig {
                endpoint: Some("http://not-websocket:3000".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("endpoint"), "error should mention 'endpoint', got: {msg}");
    }

    #[test]
    fn validate_rejects_unsupported_ssrf_scheme_allowlist_entries() {
        for scheme in ["ftp", "http://"] {
            let mut config = CrawlConfig::default();
            config.ssrf.scheme_allowlist = vec![scheme.to_owned()];

            let error = config.validate().expect_err("only HTTP transports are supported");
            assert!(
                error.to_string().contains(scheme),
                "validation error must identify the unsupported scheme, got: {error}"
            );
        }

        let mut config = CrawlConfig::default();
        config.ssrf.scheme_allowlist = vec!["http".to_owned(), "HTTP".to_owned()];
        let error = config
            .validate()
            .expect_err("scheme matching is case-insensitive, so case variants are duplicates");
        assert!(
            error.to_string().contains("duplicate scheme 'HTTP'"),
            "validation error must identify the duplicate scheme, got: {error}"
        );
    }

    #[test]
    fn validate_accepts_ws_endpoint() {
        let config = CrawlConfig {
            browser: BrowserConfig {
                endpoint: Some("ws://localhost:9222".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_accepts_wss_endpoint() {
        let config = CrawlConfig {
            browser: BrowserConfig {
                endpoint: Some("wss://remote-browser.example.com/devtools".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_accepts_no_endpoint() {
        let config = CrawlConfig {
            browser: BrowserConfig {
                endpoint: None,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn browser_backend_defaults_to_chromiumoxide() {
        assert_eq!(BrowserConfig::default().backend, BrowserBackend::Chromiumoxide);
    }

    #[test]
    fn validate_rejects_native_endpoint() {
        let config = CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Native,
                endpoint: Some("ws://localhost:9222".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chromiumoxide"), "unexpected error: {msg}");
    }

    #[test]
    fn proxy_config_debug_redacts_password_and_url_userinfo() {
        let proxy = ProxyConfig {
            url: "http://svc-account:hunter2@proxy.internal:8080".into(),
            username: Some("svc-account".into()),
            password: Some("hunter2".into()),
        };
        let rendered = format!("{proxy:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug output must not contain the raw password, got '{rendered}'"
        );
        assert!(
            rendered.contains("svc-account"),
            "Debug output should still show the non-secret username, got '{rendered}'"
        );
    }

    #[test]
    fn proxy_config_debug_shows_none_when_password_unset() {
        let proxy = ProxyConfig {
            url: "http://proxy.internal:8080".into(),
            username: None,
            password: None,
        };
        let rendered = format!("{proxy:?}");
        assert!(
            rendered.contains("password: None"),
            "unset password must render as None, got '{rendered}'"
        );
    }

    #[test]
    fn auth_config_debug_redacts_basic_password() {
        let auth = AuthConfig::Basic {
            username: "alice".into(),
            password: "hunter2".into(),
        };
        let rendered = format!("{auth:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug output must not contain the raw password, got '{rendered}'"
        );
        assert!(
            rendered.contains("alice"),
            "Debug output should still show the non-secret username, got '{rendered}'"
        );
    }

    #[test]
    fn auth_config_debug_redacts_bearer_token() {
        let auth = AuthConfig::Bearer {
            token: "sk-super-secret-token".into(),
        };
        let rendered = format!("{auth:?}");
        assert!(
            !rendered.contains("sk-super-secret-token"),
            "Debug output must not contain the raw bearer token, got '{rendered}'"
        );
    }

    #[test]
    fn auth_config_debug_redacts_header_value() {
        let auth = AuthConfig::Header {
            name: "X-Api-Key".into(),
            value: "sk-super-secret-key".into(),
        };
        let rendered = format!("{auth:?}");
        assert!(
            !rendered.contains("sk-super-secret-key"),
            "Debug output must not contain the raw header value, got '{rendered}'"
        );
        assert!(
            rendered.contains("X-Api-Key"),
            "Debug output should still show the non-secret header name, got '{rendered}'"
        );
    }
}
