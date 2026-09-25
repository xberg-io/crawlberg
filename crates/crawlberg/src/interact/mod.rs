//! Page interaction module for action-based browser automation.
//!
//! The public action types and validation helpers are available regardless of
//! which browser backend is compiled. Runtime execution is selected from the
//! configured [`BrowserBackend`].

/// Page action types and validation limits.
pub mod actions;
#[cfg(feature = "browser-chromiumoxide")]
mod chromiumoxide;
#[cfg(feature = "browser-native")]
mod native;
/// Page action validation helpers.
pub mod validation;

pub use actions::{
    DEFAULT_ACTION_TIMEOUT, MAX_ACTIONS, MAX_SCRIPT_LEN, MAX_SCROLL_AMOUNT, MAX_SELECTOR_LEN, MAX_SINGLE_WAIT_MS,
    MAX_TEXT_LEN, MAX_TOTAL_WAIT_SECS, PageAction, ScrollDirection,
};
pub use validation::validate_actions;

#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use base64::Engine as _;

use crate::engine::CrawlEngine;
use crate::error::CrawlError;
use crate::types::{BrowserBackend, InteractionResult};

/// Base64-encode PNG screenshot bytes for the `screenshot_base64` result field.
///
/// Shared by both interact backends so a screenshot captured via chromiumoxide
/// or the native renderer reaches bindings the same way scrape/crawl results do.
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
pub(crate) fn encode_screenshot_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Execute browser actions on a single page.
///
/// ~keep This is the single chokepoint for both `interact` backends
/// (`BrowserBackend::Chromiumoxide` and `BrowserBackend::Native`): the pre-flight SSRF
/// check on the target URL lives here, once, rather than duplicated into
/// `chromiumoxide::run`/`native::run`. The chromiumoxide backend otherwise enforced no
/// SSRF policy at all (xberg-io/crawlberg#74); the native backend already carries its own
/// interception via `NativeBrowserConfig.ssrf` (see `native::build_native_config`), but
/// that only guards navigation/redirects, not this seed URL, so it never doubles up with
/// the check below -- a rejected seed URL fails here before either backend runs.
pub(crate) async fn run(
    engine: &CrawlEngine,
    url: &str,
    actions: &[PageAction],
) -> Result<InteractionResult, CrawlError> {
    validate_actions(actions)?;
    engine.config.validate()?;
    validate_seed_url(url, &engine.config.ssrf).await?;

    match engine.config.browser.backend {
        BrowserBackend::Chromiumoxide => run_chromiumoxide(url, actions, &engine.config).await,
        BrowserBackend::Native => run_native(engine, url, actions).await,
    }
}

#[cfg(feature = "browser-chromiumoxide")]
async fn run_chromiumoxide(
    url: &str,
    actions: &[PageAction],
    config: &crate::types::CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    chromiumoxide::run(url, actions, config).await
}

#[cfg(not(feature = "browser-chromiumoxide"))]
async fn run_chromiumoxide(
    _url: &str,
    _actions: &[PageAction],
    _config: &crate::types::CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    Err(CrawlError::unsupported(
        "interact() with BrowserBackend::Chromiumoxide requires the browser-chromiumoxide feature",
    ))
}

#[cfg(feature = "browser-native")]
async fn run_native(engine: &CrawlEngine, url: &str, actions: &[PageAction]) -> Result<InteractionResult, CrawlError> {
    let native_executor = engine.native_browser_executor.as_deref().ok_or_else(|| {
        CrawlError::browser_error("native browser executor is not available for BrowserBackend::Native")
    })?;
    native::run(url, actions, &engine.config, native_executor).await
}

#[cfg(not(feature = "browser-native"))]
async fn run_native(
    _engine: &CrawlEngine,
    _url: &str,
    _actions: &[PageAction],
) -> Result<InteractionResult, CrawlError> {
    Err(CrawlError::unsupported(
        "interact() with BrowserBackend::Native requires the browser-native feature",
    ))
}

/// Reject `url` before any browser is launched, for either backend.
async fn validate_seed_url(url: &str, policy: &crate::net::SsrfPolicy) -> Result<(), CrawlError> {
    let target = url::Url::parse(url).map_err(|e| CrawlError::ssrf_violation(url, format!("invalid URL: {e}")))?;
    crate::net::ssrf::validate_url(&target, policy)
        .await
        .map_err(|e| CrawlError::ssrf_violation(url, e.to_string()))
}

#[cfg(test)]
mod validate_seed_url_tests {
    //! Hermetic (no Chrome, no network) coverage for the pre-flight check itself, isolated
    //! from `run()`'s dispatch -- xberg-io/crawlberg#74. `tests/test_interact.rs` covers the
    //! same contract end-to-end through the public `interact()` API, but that path also
    //! exercises the per-request CDP interception installed around navigation (defense in
    //! depth, matching `browser::navigation::page_fetch`), so it cannot in isolation prove
    //! that *this* pre-flight function is what rejects the seed URL. This module can.
    use super::validate_seed_url;
    use crate::error::CrawlError;
    use crate::net::SsrfPolicy;

    #[tokio::test]
    async fn rejects_a_loopback_seed_url() {
        let result = validate_seed_url("http://127.0.0.1:9/", &SsrfPolicy::default()).await;
        assert!(
            matches!(result, Err(CrawlError::SsrfPolicyViolation { .. })),
            "loopback seed URL must be rejected, got {result:?}"
        );
    }

    #[tokio::test]
    async fn allows_a_loopback_seed_url_when_private_networks_are_permitted() {
        let policy = SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        };
        let result = validate_seed_url("http://127.0.0.1:9/", &policy).await;
        assert!(result.is_ok(), "loopback must pass when deny_private=false: {result:?}");
    }

    #[tokio::test]
    async fn allows_a_public_seed_url() {
        // ~keep A literal IP, not a hostname: `validate_url` resolves hostnames via
        // ~keep `tokio::net::lookup_host`, which would make this test depend on live DNS/network.
        let result = validate_seed_url("https://1.1.1.1/", &SsrfPolicy::default()).await;
        assert!(result.is_ok(), "a public IP must pass the default policy: {result:?}");
    }
}
