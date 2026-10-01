//! Shared helpers for Chrome-backed browser integration tests.
//!
//! `ubuntu-24.04-arm` CI runners ship no Chrome binary at all, so any test that
//! drives a real chromiumoxide launch must detect that condition and skip loudly
//! rather than fail. This module centralizes the message-matching and
//! skip-announcement logic that used to be copy-pasted per test file (first in
//! `test_browser_pool_lifecycle.rs`, then `test_interact.rs`); every file that
//! actually launches Chrome should include it via `mod common;` and call
//! [`is_missing_chrome_message`] / [`announce_chrome_skip`] instead of
//! reimplementing the check.
#![allow(dead_code, clippy::print_stderr)]

#[cfg(all(feature = "api", feature = "mcp"))]
pub mod mcp;

/// Whether an error message indicates the runner has no usable Chrome, rather
/// than a genuine regression in the code under test. Three message variants are
/// known:
///
/// - `"auto detect a chrome executable"`: no Chrome binary exists at all, so
///   chromiumoxide reports this as a config error at build time. This is the
///   `ubuntu-24.04-arm` CI case.
/// - `"failed to launch browser"`: `crates/crawlberg/src/browser.rs`'s wording
///   when a binary exists but cannot start (e.g. missing shared libraries).
/// - `"failed to launch Chrome"`: `crates/crawlberg/src/browser_pool.rs`'s
///   wording for the same launch-time failure, driven through an explicit
///   `BrowserPool` instead of the one-shot launch path.
pub fn is_missing_chrome_message(message: &str) -> bool {
    message.contains("failed to launch browser")
        || message.contains("failed to launch Chrome")
        || message.contains("auto detect a chrome executable")
}

/// Whether an error message is crawlberg refusing a saved `browser_profile` because the Chrome on
/// the runner is a snap that cannot open the profile store. This is the `ubuntu-24.04-arm` CI
/// case, where the Chrome found is the Chromium snap and the store is under `~/.local/share`.
/// No Chrome starts, so a test of saved profiles has nothing to observe on that runner.
pub fn is_saved_profile_refusal(message: &str) -> bool {
    message.contains("which cannot open the saved browser profile")
}

/// Prints a loud, unambiguous skip notice to stderr naming the test and the
/// reason. A silently-passing test that never actually launched Chrome would
/// exercise nothing while still reporting green — this makes the skip visible
/// in CI logs instead.
pub fn announce_chrome_skip(test_name: &str, reason: &str) {
    eprintln!("skipping {test_name} because no usable Chrome was found: {reason}");
}

/// Say that `test_name` did not run on this machine, and why.
pub fn announce_skip(test_name: &str, reason: &str) {
    eprintln!("skipping {test_name}: {reason}");
}

/// Launch a Chrome that stands for another program's browser, reached through `browser.endpoint`,
/// with the cookie `owner=1` set for `seed` in its own context. Its handler runs until it closes.
/// `None`, announced, without Chrome.
#[cfg(feature = "browser")]
pub async fn launch_external_chrome_with_cookie(test_name: &str, seed: &str) -> Option<chromiumoxide::Browser> {
    use chromiumoxide::cdp::browser_protocol::network::CookieParam;
    use chromiumoxide::cdp::browser_protocol::storage::SetCookiesParams;
    use tokio_stream::StreamExt;

    let config = match chromiumoxide::browser::BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(std::env::temp_dir().join(format!("crawlberg-{test_name}-{}", std::process::id())))
        .build()
    {
        Ok(config) => config,
        Err(error) => {
            announce_chrome_skip(test_name, &error);
            return None;
        }
    };
    let (browser, mut handler) = match chromiumoxide::Browser::launch(config).await {
        Ok(pair) => pair,
        Err(error) => {
            announce_chrome_skip(test_name, &error.to_string());
            return None;
        }
    };
    tokio::spawn(async move { while handler.next().await.is_some() {} });
    browser
        .execute(SetCookiesParams {
            cookies: vec![CookieParam {
                url: Some(seed.to_owned()),
                ..CookieParam::new("owner", "1")
            }],
            browser_context_id: None,
        })
        .await
        .expect("the browser's own cookie must be set");
    Some(browser)
}
