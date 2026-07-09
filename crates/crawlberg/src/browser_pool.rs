//! Browser pool for managing a persistent Chrome instance with bounded concurrency.
//!
//! This module is feature-gated behind `#[cfg(feature = "browser")]` at the module level
//! in `lib.rs`. Do not add feature gates inside this file.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

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
pub(crate) fn safe_default_args() -> Vec<&'static str> {
    let all_args = vec![
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

    let is_snap = std::path::Path::new("/snap/chromium/current/usr/bin/chromium").exists();

    if is_snap {
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
    }
}

/// Configuration for a [`BrowserPool`].
///
/// Rust-only: this type is excluded from alef-generated polyglot bindings.
/// Pool reuse is intended for long-lived Rust processes (e.g. the cloud
/// worker); language bindings construct pools internally per-call.
#[derive(Debug, Clone)]
#[cfg_attr(alef, alef(skip))]
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

/// A pool that keeps a single Chrome browser alive and hands out pages (tabs),
/// limiting concurrency via a semaphore. If Chrome crashes the pool will
/// attempt to relaunch on the next [`acquire_page`](Self::acquire_page) call.
///
/// Rust-only: excluded from alef-generated polyglot bindings. Downstream
/// language clients should rely on per-call browser construction inside
/// crawlberg rather than managing a pool themselves.
#[cfg_attr(alef, alef(skip))]
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
            return Err(CrawlError::BrowserError("pool is shut down".into()));
        }

        let permit = self
            .page_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CrawlError::BrowserError("page semaphore closed".into()))?;

        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::BrowserError("pool is shut down".into()));
        }

        match self.try_new_page().await {
            Ok(page) => Ok(PooledPage {
                page: Some(page),
                _permit: Some(permit),
            }),
            Err(first_err) => {
                self.relaunch_browser().await?;
                let page = self.try_new_page().await.map_err(|e| {
                    CrawlError::BrowserError(format!(
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
            let mut browser = bs.browser;
            let _ = browser.close().await;
            let _ = browser.wait().await;
            drop(browser);
            let _ = tokio::time::timeout(HANDLER_SHUTDOWN_TIMEOUT, bs.handler_handle).await;
            if let Some(dir) = bs.user_data_dir {
                let _ = std::fs::remove_dir_all(dir);
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
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
            let bs = self.launch_browser().await?;
            *guard = Some(bs);
            self.healthy.store(true, Ordering::Release);
        }

        let bs = guard.as_ref().expect("browser state was just set above");
        tokio::time::timeout(PAGE_OPEN_TIMEOUT, bs.browser.new_page("about:blank"))
            .await
            .map_err(|_| CrawlError::BrowserError("timeout opening page".into()))?
            .map_err(|e| CrawlError::BrowserError(format!("failed to open page: {e}")))
    }

    /// Force-relaunch Chrome (used after a page-open failure).
    async fn relaunch_browser(&self) -> Result<(), CrawlError> {
        let mut guard = self.state.lock().await;

        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::BrowserError("pool is shut down".into()));
        }

        if guard.as_ref().is_some_and(|bs| !bs.handler_handle.is_finished()) {
            return Ok(());
        }

        self.healthy.store(false, Ordering::Release);
        if let Some(old) = guard.take() {
            let mut browser = old.browser;
            let _ = browser.close().await;
            let _ = browser.wait().await;
            drop(browser);
            let _ = tokio::time::timeout(HANDLER_SHUTDOWN_TIMEOUT, old.handler_handle).await;
            if let Some(dir) = old.user_data_dir {
                let _ = std::fs::remove_dir_all(dir);
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
                .map_err(|_| CrawlError::BrowserError("timeout connecting to browser endpoint".into()))?
                .map_err(|e| CrawlError::BrowserError(format!("failed to connect to browser: {e}")))?;
            (browser, handler, None)
        } else {
            use std::sync::atomic::AtomicU64;
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let user_data_dir = std::env::temp_dir().join(format!(
                "crawlberg-chrome-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed),
            ));
            let mut builder = BrowserConfig::builder()
                .no_sandbox()
                .new_headless_mode()
                .user_data_dir(&user_data_dir)
                .disable_default_args();
            // ~keep Chrome helper forks can trip macOS fork-safety checks; disable the ObjC abort so helpers exec.
            // ~keep The env vars are harmless on older macOS and Linux and keep pooled launches consistent.
            builder = builder
                .env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES")
                .env("OS_ACTIVITY_MODE", "disable");
            for arg in safe_default_args() {
                builder = builder.arg(arg);
            }
            for arg in &self.config.chrome_args {
                builder = builder.arg(arg.as_str());
            }
            let browser_config = builder
                .build()
                .map_err(|e| CrawlError::BrowserError(format!("invalid browser config: {e}")))?;

            let (browser, handler) = tokio::time::timeout(self.config.launch_timeout, Browser::launch(browser_config))
                .await
                .map_err(|_| CrawlError::BrowserError("timeout launching Chrome".into()))?
                .map_err(|e| CrawlError::BrowserError(format!("failed to launch Chrome: {e}")))?;
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
}

impl Drop for PooledPage {
    fn drop(&mut self) {
        if let Some(page) = self.page.take() {
            tokio::spawn(async move {
                let _ = page.close().await;
            });
        }
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
}
