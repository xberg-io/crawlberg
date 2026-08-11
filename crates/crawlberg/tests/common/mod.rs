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

/// Prints a loud, unambiguous skip notice to stderr naming the test and the
/// reason. A silently-passing test that never actually launched Chrome would
/// exercise nothing while still reporting green — this makes the skip visible
/// in CI logs instead.
pub fn announce_chrome_skip(test_name: &str, reason: &str) {
    eprintln!("skipping {test_name} because no usable Chrome was found: {reason}");
}
