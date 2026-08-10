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

use base64::Engine as _;

use crate::engine::CrawlEngine;
use crate::error::CrawlError;
use crate::types::{BrowserBackend, InteractionResult};

/// Base64-encode PNG screenshot bytes for the `screenshot_base64` result field.
///
/// Shared by both interact backends so a screenshot captured via chromiumoxide
/// or the native renderer reaches bindings the same way scrape/crawl results do.
pub(crate) fn encode_screenshot_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Execute browser actions on a single page.
pub(crate) async fn run(
    engine: &CrawlEngine,
    url: &str,
    actions: &[PageAction],
) -> Result<InteractionResult, CrawlError> {
    validate_actions(actions)?;
    engine.config.validate()?;

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
    Err(CrawlError::Unsupported(
        "interact() with BrowserBackend::Chromiumoxide requires the browser-chromiumoxide feature".into(),
    ))
}

#[cfg(feature = "browser-native")]
async fn run_native(engine: &CrawlEngine, url: &str, actions: &[PageAction]) -> Result<InteractionResult, CrawlError> {
    let native_executor = engine.native_browser_executor.as_deref().ok_or_else(|| {
        CrawlError::BrowserError("native browser executor is not available for BrowserBackend::Native".into())
    })?;
    native::run(url, actions, &engine.config, native_executor).await
}

#[cfg(not(feature = "browser-native"))]
async fn run_native(
    _engine: &CrawlEngine,
    _url: &str,
    _actions: &[PageAction],
) -> Result<InteractionResult, CrawlError> {
    Err(CrawlError::Unsupported(
        "interact() with BrowserBackend::Native requires the browser-native feature".into(),
    ))
}
