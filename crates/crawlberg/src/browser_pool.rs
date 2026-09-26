//! Browser pool for managing a persistent Chrome instance with bounded concurrency.
//!
//! ~keep This module is feature-gated behind `#[cfg(feature = "browser-chromiumoxide")]` at
//! ~keep the module level in `lib.rs` (the narrower flag -- `browser` implies it, see the
//! ~keep `~keep` there). Two methods compiled under this module, `PooledPage::into_parts` and
//! ~keep `BrowserPool::firewall`, are only called from code gated on the wider `browser`
//! ~keep feature, so each carries its own `#[cfg(feature = "browser")]` inline; those are the
//! ~keep sanctioned in-file feature gates, not a precedent for adding more.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig, BrowserConfigBuilder};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use crate::chrome_args::chrome_arg_key;
use crate::error::CrawlError;
use crate::ssrf_intercept::{BrowserFirewall, BrowserOrigin};

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
        // ~keep No startup tab: Chrome's new-tab page fetches remote content that belongs to no
        // ~keep watched page, so the SSRF check refuses it. Pages are created on demand.
        "--no-startup-window",
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
    browser: Arc<Browser>,
    /// The SSRF check every page of this browser runs under. It holds the other reference
    /// to `browser` until it is stopped.
    firewall: BrowserFirewall,
    handler_handle: JoinHandle<()>,
    user_data_dir: Option<std::path::PathBuf>,
}

impl BrowserState {
    /// Stop the SSRF check and close the browser, bounded by the handler shutdown timeout.
    async fn close(self) {
        self.firewall.stop().await;
        if let Some(mut browser) = Arc::into_inner(self.browser) {
            close_browser_within(&mut browser, HANDLER_SHUTDOWN_TIMEOUT).await;
        }
        abort_handler_after_timeout(self.handler_handle).await;
        if let Some(dir) = self.user_data_dir {
            remove_profile_dir(dir).await;
        }
    }
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

/// Close `browser` and wait for its process to exit, bounding the wait by
/// `shutdown_timeout`. If the browser has not exited before the deadline, the
/// process is force-killed via [`Browser::kill`] rather than left to linger.
///
/// ~keep `Browser::wait` is a bare `child.wait().await` with no built-in limit, so a
/// ~keep Chrome instance stuck behind a blocking OS dialog (observed: a macOS "wants to
/// ~keep use your confidential information" keychain prompt) previously held this call
/// ~keep open indefinitely. Only `wait`/`kill` are documented no-ops when this `Browser`
/// ~keep connected to an external process (chromiumoxide 0.9.1, `src/browser/mod.rs`) --
/// ~keep `close` is not: it sends a real CDP `Browser.close`. `launch_or_connect`
/// ~keep (`browser/launch.rs`) uses `Browser::connect` whenever `config.browser.endpoint`
/// ~keep is set, and this function is called unconditionally on every teardown path, so a
/// ~keep `browser.endpoint`-configured crawl shuts down the caller's external Chrome on
/// ~keep teardown. Pre-existing, tracked separately -- not fixed here.
pub(crate) async fn close_browser_within(browser: &mut Browser, shutdown_timeout: Duration) {
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
            bs.close().await;
        }
    }

    /// The SSRF check of the running browser, which a page of this pool must be watched by.
    #[cfg(feature = "browser")]
    pub(crate) async fn firewall(&self) -> Result<crate::ssrf_intercept::FirewallHandle, CrawlError> {
        self.state
            .lock()
            .await
            .as_ref()
            .map(|bs| bs.firewall.handle())
            .ok_or_else(|| CrawlError::browser_error("browser pool has no running browser"))
    }

    /// Try to create a new page from the current browser. Takes the mutex
    /// briefly, creates the page, and releases.
    async fn try_new_page(&self) -> Result<chromiumoxide::Page, CrawlError> {
        let mut guard = self.state.lock().await;

        if guard.is_none() || guard.as_ref().is_some_and(|bs| bs.handler_handle.is_finished()) {
            self.healthy.store(false, Ordering::Release);
            if let Some(old) = guard.take() {
                old.firewall.stop().await;
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
            old.close().await;
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
        let browser = Arc::new(browser);
        let firewall = match BrowserFirewall::start(
            Arc::clone(&browser),
            BrowserOrigin::of_endpoint(self.config.browser_endpoint.as_deref()),
        )
        .await
        {
            Ok(firewall) => firewall,
            Err(error) => {
                if let Some(mut browser) = Arc::into_inner(browser) {
                    close_browser_within(&mut browser, HANDLER_SHUTDOWN_TIMEOUT).await;
                }
                abort_handler_after_timeout(handler_handle).await;
                if let Some(dir) = data_dir {
                    remove_profile_dir(dir).await;
                }
                return Err(error);
            }
        };

        Ok(BrowserState {
            browser,
            firewall,
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

    /// A launched browser opens no tab of its own, so nothing loads before crawlberg asks.
    #[tokio::test]
    #[allow(
        clippy::print_stderr,
        reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
    )]
    async fn a_launched_browser_opens_no_startup_tab() {
        let user_data_dir = std::env::temp_dir().join(format!("crawlberg-startup-tab-test-{}", std::process::id()));
        let launched = match build_pool_launch_builder(&user_data_dir, &[]).build() {
            Ok(config) => Browser::launch(config).await,
            Err(error) => {
                eprintln!("skipping a_launched_browser_opens_no_startup_tab: no usable Chrome: {error}");
                return;
            }
        };
        let (mut browser, mut handler) = match launched {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("skipping a_launched_browser_opens_no_startup_tab: no usable Chrome: {error}");
                return;
            }
        };
        let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let pages: Vec<String> = browser
            .fetch_targets()
            .await
            .expect("the targets must be listed")
            .into_iter()
            .filter(|target| target.r#type == "page")
            .map(|target| target.url)
            .collect();
        close_browser_within(&mut browser, HANDLER_SHUTDOWN_TIMEOUT).await;
        handler_task.abort();
        let _ = std::fs::remove_dir_all(&user_data_dir);
        assert!(
            pages.is_empty(),
            "the browser must open no tab of its own, got {pages:?}"
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
