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

/// Whether an error message indicates the runner has no Chrome binary.
///
/// ~keep Chromiumoxide reports this configuration-time error only when executable detection
/// ~keep finds nothing. Once a binary is found, a launch error is a regression, not a skip.
pub fn is_missing_chrome_message(message: &str) -> bool {
    message.contains("auto detect a chrome executable")
}

/// Whether an error message is crawlberg refusing a saved `browser_profile` because the Chrome on
/// the runner is a snap that cannot open the profile store. This is the `ubuntu-24.04-arm` CI
/// case, where the Chrome found is the Chromium snap and the store is under `~/.local/share`.
/// No Chrome starts, so a test of saved profiles has nothing to observe on that runner.
pub fn is_saved_profile_refusal(message: &str) -> bool {
    message.contains("which cannot open the saved browser profile")
}

/// ~keep Classify both Snap binaries and distro launcher scripts that delegate to Snap.
#[cfg(unix)]
pub fn is_snap_executable(executable: &std::path::Path) -> bool {
    let resolved = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    if resolved.starts_with("/snap") {
        return true;
    }
    launcher_script_delegates_to_snap(&resolved)
}

#[cfg(unix)]
fn launcher_script_delegates_to_snap(path: &std::path::Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut bytes = [0u8; 16 * 1024];
    let Ok(len) = std::io::Read::read(&mut file, &mut bytes) else {
        return false;
    };
    let Ok(source) = std::str::from_utf8(&bytes[..len]) else {
        return false;
    };
    if !source.starts_with("#!") {
        return false;
    }
    source.lines().any(|line| {
        let mut words = line.split_ascii_whitespace();
        let first = words.next().unwrap_or_default();
        let executable = if first == "exec" {
            words.next().unwrap_or_default()
        } else {
            first
        }
        .trim_matches(['\'', '"']);
        executable == "snap" || executable == "/usr/bin/snap" || executable.starts_with("/snap/bin/")
    })
}

/// Prints a loud, unambiguous skip notice to stderr naming the test and the
/// reason. A silently-passing test that never actually launched Chrome would
/// exercise nothing while still reporting green — this makes the skip visible
/// in CI logs instead.
pub fn announce_chrome_skip(test_name: &str, reason: &str) {
    eprintln!("skipping {test_name} because no usable Chrome was found: {reason}");
}

pub fn expect_chrome_or_skip<T, E: std::fmt::Display>(test_name: &str, result: Result<T, E>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) if is_missing_chrome_message(&error.to_string()) => {
            announce_chrome_skip(test_name, &error.to_string());
            None
        }
        Err(error) => panic!("{test_name}: setup with the detected Chrome must succeed: {error}"),
    }
}

/// Say that `test_name` did not run on this machine, and why.
pub fn announce_skip(test_name: &str, reason: &str) {
    eprintln!("skipping {test_name}: {reason}");
}

/// Run a test browser's CDP handler on its own task, until its websocket fails or ends.
///
/// The same rules as crawlberg's `browser_pool::spawn_handler`. A loop that polls on past the
/// websocket error holds every pending command forever. chromiumoxide 0.9.1 checks its request
/// timeout only when the handler is polled, which a quiet connection never causes, so the loop
/// polls it every second: a command Chrome never answers then fails with a timeout
/// (xberg-io/crawlberg#586).
#[cfg(feature = "browser")]
pub fn spawn_handler(mut handler: chromiumoxide::Handler) -> tokio::task::JoinHandle<()> {
    use tokio_stream::StreamExt;
    tokio::spawn(async move {
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(1), handler.next()).await;
            if matches!(event, Ok(None | Some(Err(chromiumoxide::error::CdpError::Ws(_))))) {
                break;
            }
        }
    })
}

/// Launch a Chrome that stands for another program's browser, reached through `browser.endpoint`,
/// with the cookie `owner=1` set for `seed` in its own context. Its handler runs until it closes.
/// `None`, announced, without Chrome.
#[cfg(feature = "browser")]
pub async fn launch_external_chrome_with_cookie(test_name: &str, seed: &str) -> Option<chromiumoxide::Browser> {
    use chromiumoxide::cdp::browser_protocol::network::CookieParam;
    use chromiumoxide::cdp::browser_protocol::storage::SetCookiesParams;

    let config = expect_chrome_or_skip(
        test_name,
        chromiumoxide::browser::BrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(std::env::temp_dir().join(format!("crawlberg-{test_name}-{}", std::process::id())))
            .build(),
    )?;
    let (browser, handler) = expect_chrome_or_skip(test_name, chromiumoxide::Browser::launch(config).await)?;
    spawn_handler(handler);
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
