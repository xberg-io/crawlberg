//! The nested config sections of [`super::CrawlConfig`]: content conversion and the
//! browser fallback.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::credentials::ProxyConfig;
use super::primitives::{BrowserBackend, BrowserMode, BrowserWait, duration_ms, option_duration_ms};

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
