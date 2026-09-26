//! Browser pool for managing a persistent Chrome instance with bounded concurrency.
//!
//! ~keep This module is feature-gated behind `#[cfg(feature = "browser-chromiumoxide")]` at
//! ~keep the module level in `lib.rs` (the narrower flag -- `browser` implies it, see the
//! ~keep `~keep` there). One method compiled under this module, `PooledPage::into_parts`, is
//! ~keep only called from code gated on the wider `browser` feature, so it carries its own
//! ~keep `#[cfg(feature = "browser")]` inline with a `~keep` explaining why; that is the one
//! ~keep sanctioned in-file feature gate, not a precedent for adding more.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig, BrowserConfigBuilder};
use chromiumoxide::cdp::browser_protocol::target::{CloseTargetParams, TargetId};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use crate::chrome_args::chrome_arg_key;
use crate::error::CrawlError;

/// Timeout for opening a new page (tab) in Chrome.
const PAGE_OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for waiting on the CDP handler task during shutdown.
const HANDLER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Puppeteer-derived default args, filtered for snap chromium compatibility.
///
/// The Ubuntu snap chromium wrapper on linux-arm64 rejects flags that regular
/// chromium accepts:
/// - `--disable-background-networking` → "unknown command"
/// - `--enable-features=NetworkService,NetworkServiceInProcess` → "unknown command"
/// - `--disable-background-timer-throttling` → "unknown flag"
/// - `--metrics-recording-only` → "unknown command"
///
/// We detect snap chromium at runtime and return a filtered set when detected.
///
/// ~keep This is the single source of every launch path's default Chrome flags. None of
/// ~keep `browser.rs`, `browser_pool.rs`, `interact/chromiumoxide.rs` calls this function
/// ~keep directly or keeps its own copy of the list; each calls [`apply_default_args`],
/// ~keep which calls this. The returned strings are already run through `chrome_arg_key`,
/// ~keep so every caller can hand them straight to chromiumoxide's `BrowserConfig::arg`
/// ~keep without re-stripping the `--`. Normalizing here, once, means a fourth launch path
/// ~keep can't reintroduce the double-dash bug (see `chrome_args.rs`) by forgetting to
/// ~keep call `chrome_arg_key` itself.
/// ~keep Caller-supplied `chrome_args` config entries do not come through this function
/// ~keep and still need their own `chrome_arg_key` call at the call site.
pub(crate) fn safe_default_args() -> Vec<&'static str> {
    let mut all_args = vec![
        "--disable-background-networking",
        "--enable-features=NetworkService,NetworkServiceInProcess",
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-breakpad",
        "--disable-client-side-phishing-detection",
        "--disable-component-extensions-with-background-pages",
        "--disable-default-apps",
        "--disable-dev-shm-usage",
        "--disable-features=TranslateUI",
        "--disable-hang-monitor",
        "--disable-ipc-flooding-protection",
        "--disable-popup-blocking",
        "--disable-prompt-on-repost",
        "--disable-renderer-backgrounding",
        "--disable-sync",
        "--force-color-profile=srgb",
        "--metrics-recording-only",
        "--no-first-run",
        "--password-store=basic",
        "--lang=en_US",
    ];

    // ~keep macOS shows a blocking "wants to use your confidential information stored in
    // ~keep Chrome Safe Storage" prompt unless told to use a mock keychain instead.
    // ~keep `--use-mock-keychain` (Chromium's `kUseMockKeychain`) is defined and consumed only
    // ~keep in the macOS os_crypt backend that reads the login keychain; on Linux and Windows
    // ~keep Chrome's os_crypt backend never looks at this switch, so passing it there is a
    // ~keep no-op, not a behavior change. Gate it here anyway rather than relying on that
    // ~keep upstream no-op, so this list documents its own platform scope.
    if cfg!(target_os = "macos") {
        all_args.push("--use-mock-keychain");
    }

    let is_snap = std::path::Path::new("/snap/chromium/current/usr/bin/chromium").exists();

    let filtered: Vec<&'static str> = if is_snap {
        all_args
            .into_iter()
            .filter(|&arg| {
                !matches!(
                    arg,
                    "--disable-background-networking"
                        | "--enable-features=NetworkService,NetworkServiceInProcess"
                        | "--disable-background-timer-throttling"
                        | "--metrics-recording-only"
                )
            })
            .collect()
    } else {
        all_args
    };

    filtered.into_iter().map(chrome_arg_key).collect()
}

/// Push every entry of [`safe_default_args`] onto `builder`.
///
/// ~keep All three launch paths (`browser.rs`, `browser_pool.rs`,
/// ~keep `interact/chromiumoxide.rs`) call this instead of looping over
/// ~keep `safe_default_args()` themselves, so the loop that hands flags to
/// ~keep chromiumoxide exists exactly once. A fourth launch path gets the fix
/// ~keep by calling this function; it cannot reintroduce the double-dash bug by
/// ~keep writing its own loop and forgetting to normalize.
pub(crate) fn apply_default_args(mut builder: BrowserConfigBuilder) -> BrowserConfigBuilder {
    for arg in safe_default_args() {
        builder = builder.arg(arg);
    }
    builder
}

/// Build the [`BrowserConfigBuilder`] for a fresh pooled launch (not the
/// `browser_endpoint` connect branch).
///
/// ~keep Split out from `launch_browser` so a test can assert on the flags this path
/// ~keep actually passes without spawning a real Chrome process.
fn build_pool_launch_builder(user_data_dir: &std::path::Path, chrome_args: &[String]) -> BrowserConfigBuilder {
    let mut builder = BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(user_data_dir)
        .disable_default_args();
    // ~keep Chrome helper forks can trip macOS fork-safety checks; disable the ObjC abort so helpers exec.
    // ~keep The env vars are harmless on older macOS and Linux and keep pooled launches consistent.
    builder = builder
        .env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES")
        .env("OS_ACTIVITY_MODE", "disable");
    builder = apply_default_args(builder);
    for arg in chrome_args {
        builder = builder.arg(chrome_arg_key(arg.as_str()));
    }
    builder
}

/// Configuration for a [`BrowserPool`].
///
/// Rust-only: this type is excluded from alef-generated polyglot bindings.
/// Pool reuse is intended for long-lived Rust processes (e.g. the cloud
/// worker); language bindings construct pools internally per-call.
#[derive(Debug, Clone)]
pub struct BrowserPoolConfig {
    /// Maximum number of concurrent pages (tabs) the pool will open.
    pub max_pages: usize,
    /// If set, connect to an already-running Chrome via this CDP WebSocket URL
    /// instead of launching a new process.
    pub browser_endpoint: Option<String>,
    /// Extra command-line arguments forwarded to the Chrome process.
    pub chrome_args: Vec<String>,
    /// How long to wait for Chrome to start before giving up.
    pub launch_timeout: Duration,
}

impl Default for BrowserPoolConfig {
    fn default() -> Self {
        Self {
            max_pages: 8,
            browser_endpoint: None,
            chrome_args: Vec::new(),
            launch_timeout: Duration::from_secs(30),
        }
    }
}

struct BrowserState {
    browser: Browser,
    handler_handle: JoinHandle<()>,
    user_data_dir: Option<std::path::PathBuf>,
}

/// Wait for the CDP handler loop to finish, aborting it if it outlives the timeout.
///
/// ~keep Dropping a `JoinHandle` detaches its task rather than stopping it, so simply
/// ~keep discarding the timeout result leaked one handler loop per relaunch — unbounded
/// ~keep for a domain that keeps crashing Chrome.
async fn abort_handler_after_timeout(handle: JoinHandle<()>) {
    let abort = handle.abort_handle();
    if tokio::time::timeout(HANDLER_SHUTDOWN_TIMEOUT, handle).await.is_err() {
        tracing::warn!(
            timeout_secs = HANDLER_SHUTDOWN_TIMEOUT.as_secs(),
            "CDP handler did not exit before the shutdown timeout; aborting it"
        );
        abort.abort();
    }
}

/// Tear down `browser` and the task that runs its CDP handler.
///
/// A Chrome that crawlberg launched is closed and reaped within `shutdown_timeout` (see
/// `close_browser_within`); closing it removes every tab it has. A Chrome reached through
/// `Browser::connect` (a configured `browser.endpoint`) belongs to the caller: crawlberg closes
/// only `open_tab`, the tab it left open there, then disconnects by stopping the handler task
/// that owns the CDP websocket. It never sends that Chrome `Browser.close`.
///
/// ~keep chromiumoxide 0.9.1 has no disconnect call, and its handler loop runs until the
/// ~keep websocket closes, which a connected Chrome never does on its own. Aborting the task
/// ~keep drops the websocket. `get_mut_child` is `None` exactly for a connected browser.
/// ~keep `open_tab` is closed only on the connected branch: on a launched Chrome that hangs,
/// ~keep a tab close ahead of `close_browser_within` would push the kill past `shutdown_timeout`.
pub(crate) async fn release_browser(
    mut browser: Browser,
    handler_handle: JoinHandle<()>,
    open_tab: Option<TargetId>,
    shutdown_timeout: Duration,
) {
    if browser.get_mut_child().is_none() {
        if let Some(target_id) = open_tab {
            let _ = tokio::time::timeout(shutdown_timeout, browser.execute(CloseTargetParams::new(target_id))).await;
        }
        drop(browser);
        handler_handle.abort();
        return;
    }
    close_browser_within(&mut browser, shutdown_timeout).await;
    drop(browser);
    abort_handler_after_timeout(handler_handle).await;
}

/// Close `browser` and wait for its process to exit, bounding the wait by
/// `shutdown_timeout`. If the browser has not exited before the deadline, the
/// process is force-killed via [`Browser::kill`] rather than left to linger.
///
/// ~keep `Browser::wait` is a bare `child.wait().await` with no built-in limit, so a
/// ~keep Chrome instance stuck behind a blocking OS dialog (observed: a macOS "wants to
/// ~keep use your confidential information" keychain prompt) previously held this call
/// ~keep open indefinitely. `close` sends a real CDP `Browser.close` even to a browser this
/// ~keep process only connected to, so only `release_browser` calls this, for a launched one.
async fn close_browser_within(browser: &mut Browser, shutdown_timeout: Duration) {
    let closed = tokio::time::timeout(shutdown_timeout, async {
        let _ = browser.close().await;
        let _ = browser.wait().await;
    })
    .await;

    if closed.is_err() {
        tracing::warn!(
            timeout_secs = shutdown_timeout.as_secs_f64(),
            "browser did not close before the shutdown timeout; killing the process"
        );
        let _ = browser.kill().await;
    }
}

/// Remove a Chrome profile directory, logging rather than ignoring a failure.
///
/// ~keep `std::fs::remove_dir_all` here ran a recursive delete on the executor thread
/// ~keep while the pool's state mutex was held, stalling every waiting `acquire_page`.
async fn remove_profile_dir(dir: std::path::PathBuf) {
    if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
        tracing::warn!(
            dir = %dir.display(),
            %error,
            "failed to remove the Chrome profile directory"
        );
    }
}

/// A pool that keeps a single Chrome browser alive and hands out pages (tabs),
/// limiting concurrency via a semaphore. If Chrome crashes the pool will
/// attempt to relaunch on the next [`acquire_page`](Self::acquire_page) call.
///
/// Rust-only: excluded from alef-generated polyglot bindings. Downstream
/// language clients should rely on per-call browser construction inside
/// crawlberg rather than managing a pool themselves.
pub struct BrowserPool {
    config: BrowserPoolConfig,
    state: Mutex<Option<BrowserState>>,
    page_semaphore: Arc<Semaphore>,
    shutdown: AtomicBool,
    /// Lock-free health signal updated whenever browser state changes.
    healthy: AtomicBool,
}

impl BrowserPool {
    /// Create a new pool. Chrome is **not** launched until the first call to
    /// [`acquire_page`](Self::acquire_page) or [`warm`](Self::warm).
    pub fn new(config: BrowserPoolConfig) -> Arc<Self> {
        let semaphore = Arc::new(Semaphore::new(config.max_pages));
        Arc::new(Self {
            config,
            state: Mutex::new(None),
            page_semaphore: semaphore,
            shutdown: AtomicBool::new(false),
            healthy: AtomicBool::new(false),
        })
    }

    /// Eagerly launch the Chrome process so that the first
    /// [`acquire_page`](Self::acquire_page) call does not pay the startup
    /// cost. Returns an error immediately if Chrome cannot be started.
    pub async fn warm(&self) -> Result<(), CrawlError> {
        let mut guard = self.state.lock().await;
        if guard.is_none() {
            let bs = self.launch_browser().await?;
            *guard = Some(bs);
            self.healthy.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// Acquire a new blank page from the pool.
    ///
    /// Blocks asynchronously if `max_pages` pages are already open. The page
    /// should be closed via [`PooledPage::close`] when done; if dropped
    /// without calling `close`, a best-effort async cleanup is spawned.
    pub async fn acquire_page(&self) -> Result<PooledPage, CrawlError> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }

        let permit = self
            .page_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CrawlError::browser_error("page semaphore closed"))?;

        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }

        match self.try_new_page().await {
            Ok(page) => Ok(PooledPage {
                page: Some(page),
                _permit: Some(permit),
            }),
            Err(first_err) => {
                self.relaunch_browser().await?;
                let page = self.try_new_page().await.map_err(|e| {
                    CrawlError::browser_error(format!(
                        "failed to open page after relaunch: {e} (original: {first_err})"
                    ))
                })?;
                Ok(PooledPage {
                    page: Some(page),
                    _permit: Some(permit),
                })
            }
        }
    }

    /// Non-blocking health check. Returns `true` when Chrome is running.
    ///
    /// This is a lock-free atomic read — safe for use in health probes and
    /// monitoring without risking contention.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire) && !self.shutdown.load(Ordering::Acquire)
    }

    /// Gracefully shut the pool down. Safe to call multiple times.
    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.healthy.store(false, Ordering::Release);

        self.page_semaphore.close();

        let mut guard = self.state.lock().await;
        if let Some(bs) = guard.take() {
            release_browser(bs.browser, bs.handler_handle, None, HANDLER_SHUTDOWN_TIMEOUT).await;
            if let Some(dir) = bs.user_data_dir {
                remove_profile_dir(dir).await;
            }
        }
    }

    /// Try to create a new page from the current browser. Takes the mutex
    /// briefly, creates the page, and releases.
    async fn try_new_page(&self) -> Result<chromiumoxide::Page, CrawlError> {
        let mut guard = self.state.lock().await;

        if guard.is_none() || guard.as_ref().is_some_and(|bs| bs.handler_handle.is_finished()) {
            self.healthy.store(false, Ordering::Release);
            if let Some(old) = guard.take() {
                old.handler_handle.abort();
                if let Some(dir) = old.user_data_dir {
                    remove_profile_dir(dir).await;
                }
            }
            let bs = self.launch_browser().await?;
            *guard = Some(bs);
            self.healthy.store(true, Ordering::Release);
        }

        let bs = guard.as_ref().expect("browser state was just set above");
        tokio::time::timeout(PAGE_OPEN_TIMEOUT, bs.browser.new_page("about:blank"))
            .await
            .map_err(|_| CrawlError::browser_error("timeout opening page"))?
            .map_err(|e| CrawlError::browser_error(format!("failed to open page: {e}")))
    }

    /// Force-relaunch Chrome (used after a page-open failure).
    async fn relaunch_browser(&self) -> Result<(), CrawlError> {
        let mut guard = self.state.lock().await;

        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }

        if guard.as_ref().is_some_and(|bs| !bs.handler_handle.is_finished()) {
            return Ok(());
        }

        self.healthy.store(false, Ordering::Release);
        if let Some(old) = guard.take() {
            release_browser(old.browser, old.handler_handle, None, HANDLER_SHUTDOWN_TIMEOUT).await;
            if let Some(dir) = old.user_data_dir {
                remove_profile_dir(dir).await;
            }
        }

        let bs = self.launch_browser().await?;
        *guard = Some(bs);
        self.healthy.store(true, Ordering::Release);
        Ok(())
    }

    /// Launch (or connect to) a Chrome process according to the pool config.
    async fn launch_browser(&self) -> Result<BrowserState, CrawlError> {
        let (browser, mut handler, data_dir) = if let Some(ref endpoint) = self.config.browser_endpoint {
            let (browser, handler) = tokio::time::timeout(self.config.launch_timeout, Browser::connect(endpoint))
                .await
                .map_err(|_| CrawlError::browser_error("timeout connecting to browser endpoint"))?
                .map_err(|e| CrawlError::browser_error(format!("failed to connect to browser: {e}")))?;
            (browser, handler, None)
        } else {
            use std::sync::atomic::AtomicU64;
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let user_data_dir = std::env::temp_dir().join(format!(
                "crawlberg-chrome-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed),
            ));
            let builder = build_pool_launch_builder(&user_data_dir, &self.config.chrome_args);
            let browser_config = builder
                .build()
                .map_err(|e| CrawlError::browser_error(format!("invalid browser config: {e}")))?;

            let (browser, handler) = tokio::time::timeout(self.config.launch_timeout, Browser::launch(browser_config))
                .await
                .map_err(|_| CrawlError::browser_error("timeout launching Chrome"))?
                .map_err(|e| CrawlError::browser_error(format!("failed to launch Chrome: {e}")))?;
            (browser, handler, Some(user_data_dir))
        };

        let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

        Ok(BrowserState {
            browser,
            handler_handle,
            user_data_dir: data_dir,
        })
    }
}

impl std::fmt::Debug for BrowserPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserPool")
            .field("config", &self.config)
            .field("healthy", &self.healthy.load(Ordering::Relaxed))
            .field("shutdown", &self.shutdown.load(Ordering::Relaxed))
            .finish()
    }
}

/// A page (tab) borrowed from a [`BrowserPool`].
///
/// The semaphore permit is released when this value is dropped, allowing
/// another caller to open a page. Prefer calling [`close`](Self::close) for
/// deterministic async cleanup.
pub struct PooledPage {
    page: Option<chromiumoxide::Page>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl PooledPage {
    /// Access the underlying CDP page.
    pub fn page(&self) -> &chromiumoxide::Page {
        self.page.as_ref().expect("page already taken via close()")
    }

    /// Explicitly close the page. Sends a CDP `Target.closeTarget` command so
    /// that Chrome tears down the tab immediately. The semaphore permit is
    /// released when `self` is dropped at the end of this call.
    pub async fn close(mut self) {
        if let Some(page) = self.page.take() {
            let _ = page.close().await;
        }
    }

    // ~keep Hands the page + permit to a new owner (e.g. session affinity) without running
    // ~keep Drop's close-on-drop, which would race a still-in-flight navigation on the same target.
    /// Detach the page and its semaphore permit for handoff to another owner.
    ///
    /// Unlike [`close`](Self::close), this does not close the CDP target — the
    /// caller becomes responsible for eventually closing the page and dropping
    /// the permit. Used when a page is handed off to
    /// [`BrowserSessionPool`](crate::browser_session_pool::BrowserSessionPool)
    /// for reuse, or simply to keep the page+permit alive across a caller's
    /// `.await` boundary instead of dropping them at the end of an expression.
    // ~keep Gated on `browser`, like its only callers in browser.rs and the session pool it hands
    // ~keep off to. This module is compiled under the narrower `browser-chromiumoxide`, where the
    // ~keep method has no caller and would be dead code.
    #[cfg(feature = "browser")]
    pub(crate) fn into_parts(mut self) -> (chromiumoxide::Page, Option<OwnedSemaphorePermit>) {
        let page = self.page.take().expect("page already taken via close()");
        let permit = self._permit.take();
        (page, permit)
    }
}

impl Drop for PooledPage {
    // ~keep `tokio::spawn` panics when no runtime is active on the current thread. These
    // ~keep handles cross an FFI boundary into host GC/finalizer threads, so an unguarded
    // ~keep spawn here turns a late drop into a panic that aborts the embedding process.
    fn drop(&mut self) {
        if let Some(page) = self.page.take() {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let _ = page.close().await;
                    });
                }
                Err(_) => {
                    tracing::warn!("dropping a pooled page outside a Tokio runtime; its CDP target is left to Chrome");
                }
            }
        }
    }
}

/// Assert that `builder` carries no double-dashed flag key and, on macOS, carries the
/// mock-keychain flag. Shared by the behavioral test for each of the three launch paths
/// (this file, `browser.rs`, `interact/chromiumoxide.rs`).
///
/// ~keep chromiumoxide's `Arg` derives `Debug` on its private `key` field, so this reads
/// ~keep the exact string chromiumoxide stored, before it renders it as `--{key}`.
/// ~keep `!debug.contains("----")` cannot fail here: chromiumoxide never performs that
/// ~keep render at `Debug`/`build()` time (only inside `launch()`, which spawns Chrome),
/// ~keep so a leftover `--` in `key` would show as `key: "--foo"`, one dash short of what
/// ~keep an earlier version of this check looked for. Assert on the stored key directly.
/// ~keep Kept right before `mod tests` (not up with `apply_default_args`), on purpose:
/// ~keep `test_every_known_launch_path_calls_the_shared_apply_default_args_helper` below
/// ~keep finds the boundary between production code and test code by splitting each
/// ~keep file on its first `#[cfg(test)]` marker. A `#[cfg(test)]` item placed earlier in
/// ~keep the file would move that boundary and hide a real, later production call site.
#[cfg(test)]
pub(crate) fn assert_launch_flags_are_normalized(builder: &BrowserConfigBuilder) {
    let debug = format!("{builder:?}");
    assert!(
        !debug.contains("key: \"--"),
        "a flag key still carries its own `--`, which chromiumoxide would double-prefix: {debug}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            debug.contains("key: \"use-mock-keychain\""),
            "missing --use-mock-keychain on macOS: {debug}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = BrowserPoolConfig::default();
        assert_eq!(config.max_pages, 8);
        assert_eq!(config.launch_timeout, Duration::from_secs(30));
        assert!(config.browser_endpoint.is_none());
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

        let user_data_dir =
            std::env::temp_dir().join(format!("crawlberg-shutdown-timeout-test-{}", std::process::id()));
        let browser_config = match build_pool_launch_builder(&user_data_dir, &[]).build() {
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
        close_browser_within(&mut browser, shutdown_timeout).await;
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
    }

    /// Releasing a launched Chrome that has stopped responding kills it within one
    /// `shutdown_timeout`, even when a tab is handed over for closing: the tab dies with the
    /// browser, so no separate tab close may run ahead of the close-and-kill.
    ///
    /// ~keep `SIGSTOP` freezes the process so every CDP call hangs, as in
    /// ~keep `close_browser_within_returns_promptly_when_the_process_is_stopped`.
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
        let launched = match build_pool_launch_builder(&user_data_dir, &[]).build() {
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

        // ~keep Time the process's death, not `release_browser`'s return: after the kill it
        // ~keep still waits up to `HANDLER_SHUTDOWN_TIMEOUT` for the handler task to wind down.
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
        release_browser(browser, handler_task, Some(tab), shutdown_timeout).await;
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
    }

    /// Releasing a connected browser disconnects from it: the handler task that owns the CDP
    /// websocket stops at once, and the Chrome at the other end keeps running.
    #[tokio::test]
    #[allow(
        clippy::print_stderr,
        reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
    )]
    async fn release_browser_disconnects_from_a_connected_browser_without_closing_it() {
        let user_data_dir =
            std::env::temp_dir().join(format!("crawlberg-release-connected-test-{}", std::process::id()));
        let launched = match build_pool_launch_builder(&user_data_dir, &[]).build() {
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

        release_browser(connected, handler_task, None, Duration::from_secs(5)).await;
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
        let builder = apply_default_args(BrowserConfig::builder());
        assert_launch_flags_are_normalized(&builder);
    }

    #[test]
    fn the_pool_launch_builder_carries_no_double_dashed_flag_and_the_macos_keychain_flag() {
        // ~keep Behavioral, not textual: this calls the exact function `launch_browser`
        // ~keep uses to build its `BrowserConfig`, so a path that stops calling
        // ~keep `apply_default_args` (even by looping over a raw flag instead) fails here
        // ~keep because the returned flags actually change.
        let builder = build_pool_launch_builder(std::path::Path::new("/tmp/pool-test-profile"), &[]);
        assert_launch_flags_are_normalized(&builder);
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
}
