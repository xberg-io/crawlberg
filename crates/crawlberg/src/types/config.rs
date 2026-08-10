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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    /// Proxy URL (e.g. "http://proxy:8080", "socks5://proxy:1080").
    pub url: String,
    /// Optional username for proxy authentication.
    pub username: Option<String>,
    /// Optional password for proxy authentication.
    pub password: Option<String>,
}

/// Authentication configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Default: `["noscript"]`.
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
            strip_tags: vec!["noscript".to_owned()],
            preserve_tags: Vec::new(),
            exclude_selectors: Vec::new(),
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
    /// Maximum number of concurrent requests.
    pub max_concurrent: Option<usize>,
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
    /// Path to write WARC output. If `None`, WARC output is disabled.
    pub warc_output: Option<PathBuf>,
    /// Named browser profile for persistent sessions (cookies, localStorage).
    pub browser_profile: Option<String>,
    /// Whether to save changes back to the browser profile on exit.
    pub save_browser_profile: bool,
    /// SSRF policy for outbound network requests. Default: deny private networks,
    /// allow http/https only, max 5 redirects.
    ///
    /// `deny_private`, `allowlist` and `max_redirects` are exposed to all language
    /// bindings. `scheme_allowlist` stays Rust-only — see `SsrfPolicy`.
    #[serde(default = "SsrfPolicy::from_env")]
    pub ssrf: SsrfPolicy,
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
    #[cfg_attr(alef, alef(skip))]
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
            max_concurrent: None,
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
            warc_output: None,
            browser_profile: None,
            save_browser_profile: false,
            ssrf: SsrfPolicy::from_env(),
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
            return Err(CrawlError::InvalidConfig("max_concurrent must be > 0".into()));
        }
        if self.browser.wait == BrowserWait::Selector && self.browser.wait_selector.is_none() {
            return Err(CrawlError::InvalidConfig(
                "browser.wait_selector required when browser.wait is Selector".into(),
            ));
        }
        if let Some(max_depth) = self.max_depth
            && max_depth > 100
        {
            return Err(CrawlError::InvalidConfig(format!(
                "max_depth must be <= 100 (got {max_depth})"
            )));
        }
        if let Some(max_pages) = self.max_pages
            && max_pages == 0
        {
            return Err(CrawlError::InvalidConfig("max_pages must be > 0".into()));
        }
        if self.max_redirects > 100 {
            return Err(CrawlError::InvalidConfig("max_redirects must be <= 100".into()));
        }
        if let Some(max_body_size) = self.max_body_size
            && max_body_size == 0
        {
            return Err(CrawlError::InvalidConfig("max_body_size must be > 0".into()));
        }
        if let Some(ref proxy) = self.proxy {
            let parsed = url::Url::parse(&proxy.url)
                .map_err(|e| CrawlError::InvalidConfig(format!("invalid proxy URL '{}': {e}", proxy.url)))?;
            let scheme = parsed.scheme();
            if !matches!(scheme, "http" | "https" | "socks5" | "socks5h") {
                return Err(CrawlError::InvalidConfig(format!(
                    "invalid proxy URL scheme '{scheme}' (expected http, https, socks5, or socks5h)"
                )));
            }
        }
        if let Some(ref auth) = self.auth {
            match auth {
                AuthConfig::Basic { username, .. } if username.is_empty() => {
                    return Err(CrawlError::InvalidConfig(
                        "auth.basic.username must not be empty".into(),
                    ));
                }
                AuthConfig::Bearer { token } if token.is_empty() => {
                    return Err(CrawlError::InvalidConfig("auth.bearer.token must not be empty".into()));
                }
                AuthConfig::Header { name, value } if name.is_empty() || value.is_empty() => {
                    return Err(CrawlError::InvalidConfig(
                        "auth.header.name and auth.header.value must not be empty".into(),
                    ));
                }
                _ => {}
            }
        }
        for pattern in &self.include_paths {
            regex::Regex::new(pattern)
                .map_err(|e| CrawlError::InvalidConfig(format!("invalid include_path regex '{pattern}': {e}")))?;
        }
        for pattern in &self.exclude_paths {
            regex::Regex::new(pattern)
                .map_err(|e| CrawlError::InvalidConfig(format!("invalid exclude_path regex '{pattern}': {e}")))?;
        }
        for &code in &self.retry_codes {
            if !(100..=599).contains(&code) {
                return Err(CrawlError::InvalidConfig(format!("invalid retry code: {code}")));
            }
        }
        if self.request_timeout.is_zero() {
            return Err(CrawlError::InvalidConfig("request_timeout must be > 0".into()));
        }
        if let Some(ref endpoint) = self.browser.endpoint
            && !endpoint.starts_with("ws://")
            && !endpoint.starts_with("wss://")
        {
            return Err(CrawlError::InvalidConfig(format!(
                "browser.endpoint must start with ws:// or wss://, got: {endpoint:?}"
            )));
        }
        if self.browser.backend == BrowserBackend::Native && self.browser.endpoint.is_some() {
            return Err(CrawlError::InvalidConfig(
                "browser.endpoint is only supported by the chromiumoxide backend".into(),
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
}
