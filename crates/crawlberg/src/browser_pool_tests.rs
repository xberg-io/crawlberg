//! Unit tests for [`super`]'s browser pool, session reuse and teardown.
//!
//! ~keep In its own file because `browser_pool.rs` crossed poly's 1000-line limit when the
//! ~keep #146 close-outcome work landed, and `alef.toml` exempts `**/*_tests.rs` from the
//! ~keep quality metrics -- the same split `engine/wasm_crawl_tests.rs` already uses. Nothing
//! ~keep but test code belongs here: a production helper moved in would become lint-exempt
//! ~keep by accident.

use super::*;

#[test]
fn test_config_defaults() {
    let config = BrowserPoolConfig::default();
    assert_eq!(config.max_pages, 8);
    assert_eq!(config.launch_timeout, Duration::from_secs(30));
    assert!(config.browser_endpoint.is_none());
    assert!(config.chrome_path.is_none());
    assert!(config.chrome_args.is_empty());
}

#[test]
fn test_pool_creation() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    assert!(!pool.shutdown.load(Ordering::Relaxed));
}

#[tokio::test]
async fn test_shutdown_idempotent() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    pool.shutdown().await;
    pool.shutdown().await;
}

#[tokio::test]
async fn test_acquire_after_shutdown_fails() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    pool.shutdown().await;
    let result = pool.acquire_page().await;
    assert!(result.is_err());
}

/// `close_browser_within` must return near its configured `shutdown_timeout`, and the
/// process must actually be dead afterward, even when `Browser::close`/`wait` cannot make
/// progress -- the reported case was a Chrome process blocked behind an OS dialog
/// (see the `~keep` on `close_browser_within`'s own doc comment).
///
/// ~keep Simulates that without a real dialog: `SIGSTOP` freezes a genuinely launched
/// ~keep Chrome process so it cannot respond to the CDP `Browser.close` command or exit,
/// ~keep without killing it -- `close()`/`wait()` then hang exactly as they did against
/// ~keep the keychain-prompt report. Skipped (not failed) when this machine has no usable
/// ~keep Chrome or `kill -STOP` is unavailable (non-Unix), matching the browser
/// ~keep integration tests' skip convention. Requires a real Chrome binary; a fully mocked
/// ~keep `Browser` was not practical here (`chromiumoxide::Browser` wraps a real child
/// ~keep process and CDP connection with no test seam for either).
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn close_browser_within_returns_promptly_when_the_process_is_stopped() {
    if !cfg!(unix) {
        eprintln!("skipping close_browser_within_returns_promptly_when_the_process_is_stopped: not unix");
        return;
    }

    let user_data_dir = std::env::temp_dir().join(format!("crawlberg-shutdown-timeout-test-{}", std::process::id()));
    let browser_config = match build_pool_launch_builder(&user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "skipping close_browser_within_returns_promptly_when_the_process_is_stopped \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let (mut browser, mut handler) = match Browser::launch(browser_config).await {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!(
                "skipping close_browser_within_returns_promptly_when_the_process_is_stopped \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let pid = browser
        .get_mut_child()
        .and_then(|child| child.as_mut_inner().id())
        .expect("a freshly launched child must have a pid");

    let stopped = std::process::Command::new("kill")
        .args(["-STOP", &pid.to_string()])
        .status()
        .expect("`kill -STOP` must run")
        .success();
    assert!(stopped, "failed to SIGSTOP the launched Chrome process (pid {pid})");

    let shutdown_timeout = Duration::from_millis(500);
    let start = std::time::Instant::now();
    let close_outcome = close_browser_within(&mut browser, shutdown_timeout).await;
    let elapsed = start.elapsed();

    // ~keep Always sent, even if the assertions below fail: a stopped process left behind
    // ~keep by a broken implementation would otherwise leak past this test.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
    handler_task.abort();

    assert!(
        elapsed < Duration::from_secs(5),
        "close_browser_within must return near its configured budget ({shutdown_timeout:?}) \
         even when close()/wait() cannot make progress on a stopped process; took {elapsed:?}"
    );

    let still_running = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .expect("`kill -0` must run")
        .success();
    assert!(
        !still_running,
        "the Chrome process (pid {pid}) must be dead after close_browser_within returns, \
         via its Browser::kill() fallback"
    );
    // ~keep A guard on the reported outcome rather than a second behaviour: the timing this
    // ~keep distinction buys is asserted by
    // ~keep `release_browser_kills_a_stopped_launched_chrome_within_one_shutdown_timeout`.
    assert_eq!(
        close_outcome,
        BrowserCloseOutcome::Killed,
        "a process that could not answer close()/wait() must be reported as killed, so the \
         caller can skip waiting on a CDP handler that will never end"
    );
}

/// Releasing a launched Chrome that has stopped responding kills it within one
/// `shutdown_timeout`, and returns within one too, even when a tab is handed over for
/// closing: the tab dies with the browser, so no separate tab close may run ahead of the
/// close-and-kill.
///
/// ~keep `SIGSTOP` freezes the process so every CDP call hangs, as in
/// ~keep `close_browser_within_returns_promptly_when_the_process_is_stopped`.
/// ~keep The return-time assertion is xberg-io/crawlberg#146's regression coverage and fails
/// ~keep before its fix: the release used to wait the full `HANDLER_SHUTDOWN_TIMEOUT` for a
/// ~keep handler loop that a killed Chrome can never end, so it returned about one
/// ~keep `shutdown_timeout` plus five seconds after it started.
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn release_browser_kills_a_stopped_launched_chrome_within_one_shutdown_timeout() {
    const TEST_NAME: &str = "release_browser_kills_a_stopped_launched_chrome_within_one_shutdown_timeout";
    if !cfg!(unix) {
        eprintln!("skipping {TEST_NAME}: not unix");
        return;
    }
    let user_data_dir = std::env::temp_dir().join(format!("crawlberg-release-stopped-test-{}", std::process::id()));
    let launched = match build_pool_launch_builder(&user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
        Ok(config) => Browser::launch(config).await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let (mut browser, mut handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("skipping {TEST_NAME} because no usable Chrome was found: {error}");
            return;
        }
    };
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let page = browser
        .new_page("about:blank")
        .await
        .expect("a launched Chrome must open a tab");
    let tab = page.target_id().clone();
    let pid = browser
        .get_mut_child()
        .and_then(|child| child.as_mut_inner().id())
        .expect("a freshly launched child must have a pid");
    let stopped = std::process::Command::new("kill")
        .args(["-STOP", &pid.to_string()])
        .status()
        .expect("`kill -STOP` must run")
        .success();
    assert!(stopped, "failed to SIGSTOP the launched Chrome process (pid {pid})");

    // ~keep The process's death is timed on its own thread because it lands while
    // ~keep `release_browser` is still running; the release's own return is timed below.
    let alive = move || {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    };
    let start = std::time::Instant::now();
    let died_after = std::thread::spawn(move || {
        while alive() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        start.elapsed()
    });
    let shutdown_timeout = Duration::from_secs(1);
    let cleanup = ExternalTabCleanup {
        open_tab: Some(tab),
        ..ExternalTabCleanup::default()
    };
    release_browser(browser, handler_task, cleanup, shutdown_timeout).await;
    let released_after = start.elapsed();
    let died_after = died_after.join().expect("the watcher thread must not panic");

    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
    let _ = std::fs::remove_dir_all(&user_data_dir);

    // ~keep The kill lands one `shutdown_timeout` after the release starts. A tab close run
    // ~keep ahead of the close-and-kill would add a second timeout before it.
    assert!(
        died_after < shutdown_timeout + Duration::from_millis(700),
        "a stopped launched Chrome must be killed within one shutdown_timeout \
         ({shutdown_timeout:?}); it died after {died_after:?}"
    );
    assert!(
        released_after < shutdown_timeout + Duration::from_secs(2),
        "the release must return once the process is reaped, not wait \
         {HANDLER_SHUTDOWN_TIMEOUT:?} for a CDP handler that a killed Chrome can never end \
         (xberg-io/crawlberg#146); it returned after {released_after:?}"
    );
}

/// Releasing a connected browser disconnects from it: the handler task that owns the CDP
/// websocket stops at once, and the Chrome at the other end keeps running.
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn release_browser_disconnects_from_a_connected_browser_without_closing_it() {
    let user_data_dir = std::env::temp_dir().join(format!("crawlberg-release-connected-test-{}", std::process::id()));
    let launched = match build_pool_launch_builder(&user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
        Ok(config) => Browser::launch(config).await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let (mut owner, mut owner_handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!(
                "skipping release_browser_disconnects_from_a_connected_browser_without_closing_it \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let owner_task = tokio::spawn(async move { while owner_handler.next().await.is_some() {} });

    let (connected, mut handler) = Browser::connect(owner.websocket_address().clone())
        .await
        .expect("connecting to the launched Chrome must succeed");
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let handler_abort = handler_task.abort_handle();

    release_browser(
        connected,
        handler_task,
        ExternalTabCleanup::default(),
        Duration::from_secs(5),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let disconnected = handler_abort.is_finished();
    let version = tokio::time::timeout(Duration::from_secs(5), owner.version()).await;

    let _ = owner.kill().await;
    owner_task.abort();
    let _ = std::fs::remove_dir_all(&user_data_dir);

    assert!(
        disconnected,
        "the handler task for a connected browser must stop, closing its websocket"
    );
    assert!(
        matches!(version, Ok(Ok(_))),
        "the connected Chrome must still answer CDP after release: {version:?}"
    );
}

#[test]
fn test_safe_default_args_never_double_prefixes_for_chromiumoxide() {
    // ~keep chromiumoxide's BrowserConfig::arg renders every entry as `--{arg}`; an
    // ~keep already-`--`-prefixed entry would render as `----...` and Chrome discards
    // ~keep it as an unknown flag (see chrome_args.rs).
    for arg in safe_default_args() {
        let rendered = format!("--{arg}");
        assert!(!rendered.starts_with("----"), "double-prefixed flag: {rendered}");
    }
}

#[test]
fn test_safe_default_args_adds_use_mock_keychain_on_macos_only() {
    let args = safe_default_args();
    if cfg!(target_os = "macos") {
        assert!(
            args.contains(&"use-mock-keychain"),
            "missing --use-mock-keychain on macOS"
        );
    } else {
        assert!(
            !args.contains(&"use-mock-keychain"),
            "use-mock-keychain should be macOS-only"
        );
    }
}

#[test]
fn test_apply_default_args_produces_normalized_flags() {
    let builder = apply_default_args(BrowserConfig::builder(), &[]);
    assert_launch_flags_are_normalized(&builder);
}

#[test]
fn the_pool_launch_builder_carries_no_double_dashed_flag_and_the_macos_keychain_flag() {
    // ~keep Behavioral, not textual: this calls the exact function `launch_browser`
    // ~keep uses to build its `BrowserConfig`, so a path that stops calling
    // ~keep `apply_default_args` (even by looping over a raw flag instead) fails here
    // ~keep because the returned flags actually change.
    let builder = build_pool_launch_builder(
        std::path::Path::new("/tmp/pool-test-profile"),
        &BrowserPoolConfig::default(),
    )
    .expect("the default pool config names no binary to check");
    assert_launch_flags_are_normalized(&builder);
}

#[test]
fn the_pool_launch_builder_uses_the_configured_chrome_path_and_args() {
    assert_launch_overrides_reach_the_builder(|chrome_path, chrome_args| {
        build_pool_launch_builder(
            std::path::Path::new("/tmp/pool-test-profile"),
            &BrowserPoolConfig {
                chrome_path,
                chrome_args,
                ..BrowserPoolConfig::default()
            },
        )
    });
}

#[test]
fn the_pool_launch_builder_refuses_the_chrome_args_validate_refuses_and_names_the_pool_key() {
    for (chrome_args, expected) in [
        (
            vec!["disable-gpu"],
            "browser: BrowserPoolConfig.chrome_args entry \"disable-gpu\" must start with --",
        ),
        (
            vec!["--headless=new"],
            "browser: BrowserPoolConfig.chrome_args must not set --headless; crawlberg sets it to run Chrome",
        ),
        (
            vec!["--LANG=fr"],
            "browser: BrowserPoolConfig.chrome_args entry \"--LANG=fr\" must name the flag in lowercase",
        ),
        (
            vec!["--enable-features=A", "--enable-features=B"],
            "browser: BrowserPoolConfig.chrome_args sets --enable-features more than once",
        ),
    ] {
        let err = build_pool_launch_builder(
            std::path::Path::new("/tmp/pool-test-profile"),
            &BrowserPoolConfig {
                chrome_args: chrome_args.iter().map(|arg| (*arg).to_owned()).collect(),
                ..BrowserPoolConfig::default()
            },
        )
        .expect_err("the pool must refuse what CrawlConfig::validate refuses")
        .to_string();
        assert!(err.contains(expected), "{chrome_args:?}: unexpected error: {err}");
    }
}

#[tokio::test]
async fn a_pool_with_a_missing_chrome_path_fails_to_launch_and_names_the_path() {
    let pool = BrowserPool::new(BrowserPoolConfig {
        chrome_path: Some(std::path::PathBuf::from("/nonexistent/crawlberg-pool-chrome")),
        ..BrowserPoolConfig::default()
    });
    let error = match pool.acquire_page().await {
        Ok(_) => panic!("a pool must not launch a different Chrome when chrome_path is missing"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("BrowserPoolConfig.chrome_path '/nonexistent/crawlberg-pool-chrome' cannot be used"),
        "the error must name the pool key and the path, got: {error}"
    );
}

#[test]
fn test_every_known_launch_path_calls_the_shared_apply_default_args_helper() {
    // ~keep Textual guard, kept alongside the behavioral tests above and in browser.rs's
    // ~keep and interact/chromiumoxide.rs's own test modules (each builds the real
    // ~keep launch config for its path and inspects the flags). Comments are stripped
    // ~keep and apply_default_args' own definition line is excluded, so a path that
    // ~keep only mentions the helper's name in a comment, or is the file that defines
    // ~keep it, does not satisfy this; only a real call site does.
    // ~keep Limitation: this can only check the three files named here. A fourth
    // ~keep launch path added in a new file is NOT caught by this test; it needs its
    // ~keep own behavioral test or a new entry in this list.
    for (path, src) in [
        ("browser/launch.rs", include_str!("browser/launch.rs")),
        ("browser_pool.rs", include_str!("browser_pool.rs")),
        ("interact/chromiumoxide.rs", include_str!("interact/chromiumoxide.rs")),
    ] {
        let code_only: String = src
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(src)
            .lines()
            .filter(|line| !line.contains("fn apply_default_args("))
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code_only.contains("apply_default_args("),
            "{path} does not call the shared apply_default_args helper outside a comment or its own definition"
        );
    }
}
