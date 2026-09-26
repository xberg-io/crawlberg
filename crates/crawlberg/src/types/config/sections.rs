//! The nested config sections of [`super::CrawlConfig`]: content conversion and the
//! browser fallback.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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
    /// Prepend a YAML frontmatter block (`title`, `description`, etc., extracted from
    /// `<head>`) to the markdown output. Default: `true`.
    ///
    /// This only controls the frontmatter text inside `markdown.content`. Crawlberg
    /// never reads `<head>` metadata back out of the converter's result -- `PageMetadata`
    /// is populated independently by `crate::html::metadata::extract_metadata` from the
    /// parsed DOM, so turning this off does not lose any metadata field. ~keep
    pub extract_metadata: bool,
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
            extract_metadata: true,
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
    /// Overall deadline for a single browser fetch, covering browser launch (or
    /// page acquisition from a shared pool), page setup, navigation, rendering,
    /// and screenshot capture. Must exceed `timeout` to leave room for launch
    /// and setup overhead; a fetch that has not returned within this deadline
    /// fails with a timeout error (in milliseconds when serialized).
    ///
    /// Shutdown/teardown is governed separately by `shutdown_timeout` and is
    /// not counted against this deadline: an already-computed result is
    /// delivered to the caller without waiting for the browser process to
    /// exit.
    #[serde(with = "duration_ms")]
    pub overall_timeout: Duration,
    /// How long to wait for the browser process to close and exit cleanly
    /// during teardown before the process is forcibly killed (in milliseconds
    /// when serialized).
    #[serde(with = "duration_ms")]
    pub shutdown_timeout: Duration,
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
    /// Chrome or Chromium executable to launch. When set, crawlberg launches only this
    /// binary, and a path that is missing or not executable is an error that names the
    /// path; crawlberg never falls back to a different Chrome. When unset, crawlberg uses
    /// the `CHROME` environment variable, then searches the machine for an installed Chrome,
    /// Chromium or Edge. Chromiumoxide backend only: ignored, with a warning, when `endpoint`
    /// is set, with the native backend, and by scrapes and crawls that use a shared browser pool.
    pub chrome_path: Option<PathBuf>,
    /// Extra Chrome command-line flags, each written as `--flag` or `--flag=value`, for example
    /// `--user-agent=...`. A flag here replaces a crawlberg default flag of the same name
    /// (`--lang=fr` replaces crawlberg's `--lang=en_US`). Rejected: an entry that does not
    /// start with `--`, a flag name with an uppercase letter (Chrome flag names are lowercase),
    /// a flag named twice, and `--headless`, `--remote-debugging-port` and `--user-data-dir`,
    /// which crawlberg sets itself to run Chrome. Set this only from trusted configuration,
    /// like `proxy`: flags such as `--proxy-server` and `--host-resolver-rules` send Chrome's
    /// traffic around the `ssrf` policy. Chromiumoxide backend only: ignored, with a warning,
    /// when `endpoint` is set, with the native backend, and by scrapes and crawls that use a
    /// shared browser pool.
    pub chrome_args: Vec<String>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            mode: BrowserMode::Auto,
            backend: BrowserBackend::Chromiumoxide,
            endpoint: None,
            timeout: Duration::from_secs(30),
            overall_timeout: Duration::from_secs(60),
            shutdown_timeout: Duration::from_secs(5),
            wait: BrowserWait::default(),
            wait_selector: None,
            extra_wait: None,
            proxy: None,
            block_url_patterns: Vec::new(),
            eval_script: None,
            robots_user_agent: None,
            capture_network_events: false,
            session_affinity: true,
            chrome_path: None,
            chrome_args: Vec::new(),
        }
    }
}

/// Chrome flags that the launch itself sets and a caller must not repeat: two values for
/// one of these reach Chrome in no fixed order.
pub(crate) const LAUNCH_OWNED_CHROME_SWITCHES: [&str; 3] = ["headless", "remote-debugging-port", "user-data-dir"];

/// The switch name of a Chrome flag: `--lang=fr` and `lang` both name `lang`.
pub(crate) fn chrome_switch_name(arg: &str) -> &str {
    let key = arg.strip_prefix("--").unwrap_or(arg);
    key.split_once('=').map_or(key, |(name, _)| name)
}

/// Check the entries of `BrowserConfig::chrome_args`: each is `--name` or `--name=value`, with a
/// lowercase name that is not one of [`LAUNCH_OWNED_CHROME_SWITCHES`] and appears only once.
/// `section` names the config that holds the list (`browser` or `BrowserPoolConfig`), so the
/// error names the key the caller wrote.
// ~keep Shared by `CrawlConfig::validate` and the launch helper in `browser_pool.rs`, for the
// ~keep same reason as `check_chrome_executable` below.
pub(crate) fn check_chrome_args(section: &str, chrome_args: &[String]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for arg in chrome_args {
        // ~keep Only `--name` and `--name=value` are flags. Anything else would be turned
        // ~keep into a stray `--x` flag (`["--user-agent", "x"]`), or would dodge the
        // ~keep checks below with a single-dash spelling. A bare `--` ends Chrome's switch
        // ~keep parsing and turns every later flag into a URL to open.
        let key = arg.strip_prefix("--").unwrap_or_default();
        let name = chrome_switch_name(key);
        if name.is_empty() || key.starts_with('-') {
            return Err(format!(
                "{section}.chrome_args entry {arg:?} must start with -- followed by a flag name; \
                 write a flag with a value as --flag=value"
            ));
        }
        // ~keep Chrome lowercases switch names on Windows and not elsewhere, so a mixed-case
        // ~keep name would collide with a default or reserved flag on one platform only.
        // ~keep Refusing it keeps the exact comparisons below true on every platform.
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(format!(
                "{section}.chrome_args entry {arg:?} must name the flag in lowercase, as Chrome does"
            ));
        }
        // ~keep chromiumoxide keeps launch flags in a HashMap, so two values for one
        // ~keep switch reach Chrome in no fixed order and either could win.
        if LAUNCH_OWNED_CHROME_SWITCHES.contains(&name) {
            return Err(format!(
                "{section}.chrome_args must not set --{name}; crawlberg sets it to run Chrome"
            ));
        }
        if !seen.insert(name) {
            return Err(format!("{section}.chrome_args sets --{name} more than once"));
        }
    }
    Ok(())
}

/// Check that `path` names an executable file, for the `chrome_path` of `section`.
///
/// The error names the path, so a caller can tell a typo from a permission problem.
// ~keep Shared by `CrawlConfig::validate` and the launch helper in `browser_pool.rs`: the
// ~keep Rust-only `BrowserPoolConfig` never passes through `validate`, and chromiumoxide's
// ~keep own spawn error for a missing binary does not name the path.
pub(crate) fn check_chrome_executable(section: &str, path: &Path) -> Result<(), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| format!("{section}.chrome_path '{}' cannot be used: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{section}.chrome_path '{}' is not a file", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("{section}.chrome_path '{}' is not executable", path.display()));
        }
    }
    Ok(())
}

/// Warn that `chrome_path` and `chrome_args` have no effect on this fetch, when either is set.
/// `reason` completes the sentence "... are ignored when ...".
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
pub(crate) fn warn_ignored_launch_options(browser: &BrowserConfig, reason: &str) {
    if browser.chrome_path.is_some() || !browser.chrome_args.is_empty() {
        tracing::warn!(
            chrome_path = ?browser.chrome_path,
            chrome_args = ?browser.chrome_args,
            "browser.chrome_path and browser.chrome_args are ignored when {reason}"
        );
    }
}

/// Create a uniquely named file that `check_chrome_executable` accepts, for tests that need a
/// `chrome_path` without a real Chrome. The caller removes it.
#[cfg(test)]
pub(crate) fn executable_temp_file(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "crawlberg-fake-chrome-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, b"#!/bin/sh\nexit 1\n").expect("the fake chrome file must be writable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("the fake chrome file must be chmod-able");
    }
    path
}
