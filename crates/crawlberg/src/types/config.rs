use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::AssetCategory;
use super::dispatch::DispatchProfile;
use crate::net::SsrfPolicy;

/// Metadata about an LLM extraction pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExtractionMeta {
    /// Estimated cost of the LLM call in USD.
    pub cost: Option<f64>,
    /// Number of prompt (input) tokens consumed.
    pub prompt_tokens: Option<u64>,
    /// Number of completion (output) tokens generated.
    pub completion_tokens: Option<u64>,
    /// The model identifier used for extraction.
    pub model: Option<String>,
    /// Number of content chunks sent to the LLM.
    pub chunks_processed: usize,
}

/// When to use the headless browser fallback.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMode {
    /// Automatically detect when JS rendering is needed and fall back to browser.
    #[default]
    Auto,
    /// Always use the browser for every request.
    Always,
    /// Never use the browser fallback.
    Never,
    /// Always use the browser with all stealth surfaces enabled.
    ///
    /// Behaves like [`Always`](BrowserMode::Always) for escalation purposes
    /// (every request is routed through the browser tier), but additionally
    /// enables:
    ///
    /// - browser JavaScript stealth patches
    /// - native-backend TLS fingerprint spoofing
    /// - stealth-aware default user-agent when no explicit UA is set
    /// - 1920×1080 viewport override
    ///
    /// Use this instead of setting the now-removed `BrowserConfig.stealth`
    /// boolean field.
    Stealth,
}

/// Wait strategy for browser page rendering.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserWait {
    /// Wait until network activity is idle.
    #[default]
    NetworkIdle,
    /// Wait for a specific CSS selector to appear in the DOM.
    Selector,
    /// Wait for a fixed duration after navigation.
    Fixed,
}

/// Browser backend used for JavaScript rendering.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserBackend {
    /// Existing Chromium/CDP backend powered by chromiumoxide.
    #[default]
    Chromiumoxide,
    /// Crawlberg-owned native browser backend derived from Obscura.
    Native,
}

/// Opt-in encoding applied to a downloaded document's bytes for callers who need the
/// content available in a serializable field rather than reading it from disk.
///
/// `None` (the `CrawlConfig.document_content_encoding` default) produces neither — unlike
/// screenshots, base64-encoding a document by default would duplicate an already
/// up-to-`document_max_size` buffer (50 MB default) in memory per document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentContentEncoding {
    /// Populate `DownloadedDocument.content_base64` with a base64-encoded copy.
    Base64,
}

/// Traversal order for a crawl.
///
/// Selects both the queue discipline and the selection strategy, because global order is a
/// property of the frontier: the engine hands its bounded selection window to the strategy, so
/// a strategy alone can only reorder URLs that have already been dequeued.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CrawlStrategyKind {
    /// Breadth-first: a FIFO frontier visits every URL at one depth before the next.
    #[default]
    Bfs,
    /// Depth-first: a LIFO frontier descends into a page's children before its siblings.
    Dfs,
    /// Highest-priority-first within the selection window, scored by `CrawlStrategy::score_url`.
    BestFirst,
    /// Like `BestFirst`, but stops once newly crawled pages stop contributing new terms.
    Adaptive,
}

/// Content filter applied to each crawled page before it reaches the result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ContentFilterKind {
    /// Keep only pages scoring at or above `bm25_threshold` for `bm25_query`.
    Bm25,
}

pub(crate) mod duration_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_millis().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

pub(crate) mod option_duration_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        d.map(|d| d.as_millis() as u64).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let ms: Option<u64> = Option::deserialize(d)?;
        Ok(ms.map(Duration::from_millis))
    }
}

/// Proxy configuration for HTTP requests.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    /// Proxy URL (e.g. "http://proxy:8080", "socks5://proxy:1080").
    pub url: String,
    /// Optional username for proxy authentication.
    pub username: Option<String>,
    /// Optional password for proxy authentication.
    pub password: Option<String>,
}

impl std::fmt::Debug for ProxyConfig {
    /// Redacted: the derived `Debug` would print `password` verbatim, and `url` may
    /// itself carry `user:pass@` userinfo. Any `tracing::debug!(?proxy, ...)` or
    /// `{:?}` capture would leak it into logs. Shows the redacted URL and the username,
    /// but only whether a password is set — never the password itself.
    // ~keep alef extracts public inherent AND trait-impl methods; `Formatter` has no
    // binding representation, so without this the surface fails generation with
    // lossy_sanitized_surface. The derived Debug this replaced emitted no method at all.
    #[cfg_attr(alef, alef(skip))]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("url", &crate::net::redact_url_credentials(&self.url))
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Authentication configuration.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type")]
pub enum AuthConfig {
    /// HTTP Basic authentication.
    #[serde(rename = "basic")]
    Basic {
        /// Username sent in the `Authorization: Basic` header.
        username: String,
        /// Password sent in the `Authorization: Basic` header.
        password: String,
    },
    /// Bearer token authentication.
    #[serde(rename = "bearer")]
    Bearer {
        /// Token sent in the `Authorization: Bearer` header.
        token: String,
    },
    /// Custom authentication header.
    #[serde(rename = "header")]
    Header {
        /// HTTP header name to set on each request.
        name: String,
        /// HTTP header value to send.
        value: String,
    },
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self::Basic {
            username: String::new(),
            password: String::new(),
        }
    }
}

impl std::fmt::Debug for AuthConfig {
    /// Redacted: the derived `Debug` would print `password`, `token`, and `value` (the
    /// header value carrying the secret) verbatim, and any `tracing::debug!(?auth, ...)`
    /// or `{:?}` capture would leak it into logs. Shows which variant is configured and
    /// whether its secret field is non-empty, never the secret's contents.
    // ~keep alef extracts public inherent AND trait-impl methods; `Formatter` has no
    // binding representation, so without this the surface fails generation with
    // lossy_sanitized_surface. The derived Debug this replaced emitted no method at all.
    #[cfg_attr(alef, alef(skip))]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { username, password } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &(!password.is_empty()).then_some("***"))
                .finish(),
            Self::Bearer { token } => f
                .debug_struct("Bearer")
                .field("token", &(!token.is_empty()).then_some("***"))
                .finish(),
            Self::Header { name, value } => f
                .debug_struct("Header")
                .field("name", name)
                .field("value", &(!value.is_empty()).then_some("***"))
                .finish(),
        }
    }
}

/// Content extraction and conversion configuration.
///
/// Controls how HTML is converted to the output format. Uses
/// html-to-markdown-rs as the conversion engine for all formats
/// (markdown, plain text, djot).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContentConfig {
    /// Output format: `"markdown"` (default), `"plain"`, `"djot"`.
    pub output_format: String,
    /// Preprocessing aggressiveness: `"minimal"`, `"standard"` (default), `"aggressive"`.
    ///
    /// - Minimal: only scripts/styles removed.
    /// - Standard: also removes nav, nav-hinted headers/footers/asides, forms.
    /// - Aggressive: removes all footers/asides unconditionally.
    pub preprocessing_preset: String,
    /// Remove navigation elements (nav, breadcrumbs, menus). Default: `true`.
    pub remove_navigation: bool,
    /// Remove form elements. Default: `true`.
    pub remove_forms: bool,
    /// HTML tag names to strip (render children only, remove the tag wrapper).
    /// Default: `[]`.
    #[serde(default)]
    pub strip_tags: Vec<String>,
    /// HTML tag names to preserve as raw HTML in output.
    #[serde(default)]
    pub preserve_tags: Vec<String>,
    /// CSS selectors for elements to exclude entirely (element + all content).
    ///
    /// Unlike `strip_tags` (which removes the wrapper but keeps children),
    /// excluded elements and all descendants are dropped. Supports CSS selectors:
    /// `.class`, `#id`, `[attribute]`, compound selectors.
    ///
    /// Default: `["noscript"]`. `<noscript>` fallback content (no-JS notices,
    /// tracking pixels, GTM iframes) is meant for browsers with JavaScript
    /// disabled, not for a markdown reader, and `strip_tags` cannot drop it —
    /// on `preprocessing_preset: "standard"` (crawlberg's only path) it only
    /// removes the wrapper and still renders the children. ~keep
    ///
    /// Example: `[".cookie-banner", "#ad-container", "[role='complementary']"]`
    #[serde(default)]
    pub exclude_selectors: Vec<String>,
    /// Skip image elements in output. Default: `false`.
    pub skip_images: bool,
    /// Max DOM traversal depth. Prevents stack overflow on deeply nested HTML.
    pub max_depth: Option<usize>,
    /// Enable line wrapping. Default: `false`.
    pub wrap: bool,
    /// Wrap width when `wrap` is enabled. Default: `80`.
    pub wrap_width: usize,
    /// Include document structure tree in output. Default: `true`.
    pub include_document_structure: bool,
}

impl Default for ContentConfig {
    fn default() -> Self {
        Self {
            output_format: "markdown".to_owned(),
            preprocessing_preset: "standard".to_owned(),
            remove_navigation: true,
            remove_forms: true,
            strip_tags: Vec::new(),
            preserve_tags: Vec::new(),
            exclude_selectors: vec!["noscript".to_owned()],
            skip_images: false,
            max_depth: None,
            wrap: false,
            wrap_width: 80,
            include_document_structure: true,
        }
    }
}

/// Browser fallback configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BrowserConfig {
    /// When to use the headless browser fallback.
    pub mode: BrowserMode,
    /// Browser backend used to render JavaScript-heavy pages.
    pub backend: BrowserBackend,
    /// CDP WebSocket endpoint for connecting to an external browser instance.
    pub endpoint: Option<String>,
    /// Timeout for browser page load and rendering (in milliseconds when serialized).
    #[serde(with = "duration_ms")]
    pub timeout: Duration,
    /// Wait strategy after browser navigation.
    pub wait: BrowserWait,
    /// CSS selector to wait for when `wait` is `Selector`.
    pub wait_selector: Option<String>,
    /// Extra time to wait after the wait condition is met.
    #[serde(default, with = "option_duration_ms")]
    pub extra_wait: Option<Duration>,
    /// Proxy for browser fetches. Overrides `CrawlConfig.proxy` when set.
    /// Native backend supports http/https only (no SOCKS5).
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
    /// URL patterns to block before the network request fires. Supports `*`
    /// wildcards. Useful for skipping ads/analytics/large images. Honored by
    /// `BrowserBackend::Native`; chromiumoxide ignores this field today.
    #[serde(default)]
    pub block_url_patterns: Vec<String>,
    /// JavaScript snippet evaluated after navigation completes.
    ///
    /// Scraping captures the native backend result in `ScrapeResult.browser.eval_result`.
    /// Interactions run this script before page actions on both browser backends but do
    /// not include the script result in `InteractionResult`.
    #[serde(default)]
    pub eval_script: Option<String>,
    /// User-agent used when fetching robots.txt. Defaults to `BrowserConfig.user_agent`
    /// (or crawlberg's default) if unset. Native only.
    #[serde(default)]
    pub robots_user_agent: Option<String>,
    /// Capture the full network event stream into the result. Default false
    /// (only the document event is captured). Native only.
    #[serde(default)]
    pub capture_network_events: bool,
    /// Enable session affinity: reuse chromiumoxide Pages for same-domain
    /// requests so cookies + fingerprint + solved challenges persist.
    /// Default: true. When false, each request gets a fresh Page.
    #[serde(default)]
    pub session_affinity: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            mode: BrowserMode::Auto,
            backend: BrowserBackend::Chromiumoxide,
            endpoint: None,
            timeout: Duration::from_secs(30),
            wait: BrowserWait::default(),
            wait_selector: None,
            extra_wait: None,
            proxy: None,
            block_url_patterns: Vec::new(),
            eval_script: None,
            robots_user_agent: None,
            capture_network_events: false,
            session_affinity: true,
        }
    }
}

/// Configuration for crawl, scrape, and map operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Whether to restrict crawling to the same domain.
    pub stay_on_domain: bool,
    /// Whether to allow subdomains when `stay_on_domain` is true.
    pub allow_subdomains: bool,
    /// Regex patterns for paths to include during crawling.
    #[serde(default)]
    pub include_paths: Vec<String>,
    /// Regex patterns for paths to exclude during crawling.
    #[serde(default)]
    pub exclude_paths: Vec<String>,
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
    /// Number of retry attempts for failed requests.
    pub retry_count: usize,
    /// HTTP status codes that should trigger a retry.
    #[serde(default)]
    pub retry_codes: Vec<u16>,
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
    /// `deny_private`, `allowlist` and `max_redirects` are exposed to all language
    /// bindings. `scheme_allowlist` stays Rust-only — see `SsrfPolicy`.
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
            custom_headers: HashMap::new(),
            request_timeout: Duration::from_secs(30),
            rate_limit_ms: None,
            max_redirects: 10,
            retry_count: 0,
            retry_codes: Vec::new(),
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
    pub fn validate(&self) -> Result<(), crate::error::CrawlError> {
        use crate::error::CrawlError;

        if let Some(0) = self.max_concurrent {
            return Err(CrawlError::invalid_config("max_concurrent must be > 0"));
        }
        // ~keep Reject rather than fall back to keeping every page: a filter that silently
        // does nothing looks identical to one that matched everything.
        if self.content_filter == Some(ContentFilterKind::Bm25) && self.bm25_query.is_none() {
            return Err(CrawlError::invalid_config(
                "bm25_query is required when content_filter is bm25",
            ));
        }
        if self.browser.wait == BrowserWait::Selector && self.browser.wait_selector.is_none() {
            return Err(CrawlError::invalid_config(
                "browser.wait_selector required when browser.wait is Selector",
            ));
        }
        if let Some(max_depth) = self.max_depth
            && max_depth > 100
        {
            return Err(CrawlError::invalid_config(format!(
                "max_depth must be <= 100 (got {max_depth})"
            )));
        }
        if let Some(max_pages) = self.max_pages
            && max_pages == 0
        {
            return Err(CrawlError::invalid_config("max_pages must be > 0"));
        }
        if self.max_redirects > 100 {
            return Err(CrawlError::invalid_config("max_redirects must be <= 100"));
        }
        if let Some(max_body_size) = self.max_body_size
            && max_body_size == 0
        {
            return Err(CrawlError::invalid_config("max_body_size must be > 0"));
        }
        if let Some(ref proxy) = self.proxy {
            let parsed = url::Url::parse(&proxy.url)
                .map_err(|e| CrawlError::invalid_config(format!("invalid proxy URL '{}': {e}", proxy.url)))?;
            let scheme = parsed.scheme();
            if !matches!(scheme, "http" | "https" | "socks5" | "socks5h") {
                return Err(CrawlError::invalid_config(format!(
                    "invalid proxy URL scheme '{scheme}' (expected http, https, socks5, or socks5h)"
                )));
            }
        }
        if let Some(ref auth) = self.auth {
            match auth {
                AuthConfig::Basic { username, .. } if username.is_empty() => {
                    return Err(CrawlError::invalid_config("auth.basic.username must not be empty"));
                }
                AuthConfig::Bearer { token } if token.is_empty() => {
                    return Err(CrawlError::invalid_config("auth.bearer.token must not be empty"));
                }
                AuthConfig::Header { name, value } if name.is_empty() || value.is_empty() => {
                    return Err(CrawlError::invalid_config(
                        "auth.header.name and auth.header.value must not be empty",
                    ));
                }
                _ => {}
            }
        }
        for pattern in &self.include_paths {
            regex::Regex::new(pattern)
                .map_err(|e| CrawlError::invalid_config(format!("invalid include_path regex '{pattern}': {e}")))?;
        }
        for pattern in &self.exclude_paths {
            regex::Regex::new(pattern)
                .map_err(|e| CrawlError::invalid_config(format!("invalid exclude_path regex '{pattern}': {e}")))?;
        }
        for &code in &self.retry_codes {
            if !(100..=599).contains(&code) {
                return Err(CrawlError::invalid_config(format!("invalid retry code: {code}")));
            }
        }
        if self.request_timeout.is_zero() {
            return Err(CrawlError::invalid_config("request_timeout must be > 0"));
        }
        if let Some(ref endpoint) = self.browser.endpoint
            && !endpoint.starts_with("ws://")
            && !endpoint.starts_with("wss://")
        {
            return Err(CrawlError::invalid_config(format!(
                "browser.endpoint must start with ws:// or wss://, got: {endpoint:?}"
            )));
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
