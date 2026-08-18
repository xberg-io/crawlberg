//! Builder for [`CrawlEngine`].

use std::sync::Arc;

use crate::defaults;
use crate::error::CrawlError;
#[cfg(not(target_arch = "wasm32"))]
use crate::sink::EventSink;
use crate::traits::*;
use crate::types::*;

use super::CrawlEngine;

/// Builder for [`CrawlEngine`].
///
/// Any field left unset will be filled with a default implementation
/// from the crate's internal `defaults` module.
///
/// # Pool injection
///
/// For long-lived processes (e.g. a worker service that handles many jobs), construct
/// the browser pool(s) once at startup and inject them via the builder methods rather
/// than relying on per-engine pool construction:
///
/// ```rust,ignore
/// use crawlberg::{BrowserPool, BrowserPoolConfig, CrawlEngine};
///
/// let pool = BrowserPool::new(BrowserPoolConfig::default());
/// pool.warm().await?;
///
/// let engine = CrawlEngine::builder()
///     .with_browser_pool(pool)
///     .build()?;
/// // The engine reuses the same Chrome instance across all crawls.
/// ```
pub struct CrawlEngineBuilder {
    config: Option<CrawlConfig>,
    frontier: Option<Arc<dyn Frontier>>,
    rate_limiter: Option<Arc<dyn RateLimiter>>,
    store: Option<Arc<dyn CrawlStore>>,
    event_emitter: Option<Arc<dyn EventEmitter>>,
    strategy: Option<Arc<dyn CrawlStrategy>>,
    content_filter: Option<Arc<dyn ContentFilter>>,
    cache: Option<Arc<dyn CrawlCache>>,
    #[cfg(not(target_arch = "wasm32"))]
    event_sink: Option<Arc<dyn EventSink>>,
    page_budget: Option<Arc<dyn crate::budget::PageBudget>>,
    #[cfg(feature = "browser")]
    browser_pool: Option<Arc<crate::browser_pool::BrowserPool>>,
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
    native_executor: Option<Arc<crawlberg_browser::adapter::NativeBrowserExecutor>>,
    proxy_provider: Option<Arc<dyn crate::ProxyProvider>>,
}

impl CrawlEngineBuilder {
    /// Create a new builder with no fields set.
    pub fn new() -> Self {
        Self {
            config: None,
            frontier: None,
            rate_limiter: None,
            store: None,
            event_emitter: None,
            strategy: None,
            content_filter: None,
            cache: None,
            #[cfg(not(target_arch = "wasm32"))]
            event_sink: None,
            page_budget: None,
            #[cfg(feature = "browser")]
            browser_pool: None,
            #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
            native_executor: None,
            proxy_provider: None,
        }
    }

    /// Set the crawl configuration.
    pub fn config(mut self, config: CrawlConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Set the frontier implementation.
    #[allow(dead_code)]
    pub fn frontier(mut self, frontier: impl Frontier + 'static) -> Self {
        self.frontier = Some(Arc::new(frontier));
        self
    }

    /// Set the rate limiter implementation.
    #[allow(dead_code)]
    pub fn rate_limiter(mut self, rate_limiter: impl RateLimiter + 'static) -> Self {
        self.rate_limiter = Some(Arc::new(rate_limiter));
        self
    }

    /// Set the store implementation.
    #[allow(dead_code)]
    pub fn store(mut self, store: impl CrawlStore + 'static) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    /// Set the event emitter implementation.
    #[allow(dead_code)]
    pub fn event_emitter(mut self, event_emitter: impl EventEmitter + 'static) -> Self {
        self.event_emitter = Some(Arc::new(event_emitter));
        self
    }

    /// Set the crawl strategy implementation.
    #[allow(dead_code)]
    pub fn strategy(mut self, strategy: impl CrawlStrategy + 'static) -> Self {
        self.strategy = Some(Arc::new(strategy));
        self
    }

    /// Set the content filter implementation.
    #[allow(dead_code)]
    pub fn content_filter(mut self, content_filter: impl ContentFilter + 'static) -> Self {
        self.content_filter = Some(Arc::new(content_filter));
        self
    }

    /// Set the persistent cache implementation.
    #[allow(dead_code)]
    pub fn cache(mut self, cache: impl CrawlCache + 'static) -> Self {
        self.cache = Some(Arc::new(cache));
        self
    }

    /// Set the event sink for streaming crawl events.
    ///
    /// The event sink receives [`CrawlEvent`]s as pages are processed, allowing
    /// consumers to integrate with external systems (NATS, dashboards, analytics, etc.)
    /// without crawlberg depending on those backends.
    ///
    /// [`CrawlEvent`]: crate::CrawlEvent
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(dead_code)]
    pub fn event_sink(mut self, event_sink: impl EventSink + 'static) -> Self {
        self.event_sink = Some(Arc::new(event_sink));
        self
    }

    /// Set the page budget hook for controlling crawl extent.
    ///
    /// The page budget is consulted before each page fetch. Returning
    /// `Err(BudgetError::Exhausted)` halts the crawl gracefully.
    ///
    /// Defaults to [`DefaultPageBudget`] if not set.
    ///
    /// [`DefaultPageBudget`]: crate::budget::DefaultPageBudget
    #[allow(dead_code)]
    pub fn page_budget(mut self, page_budget: impl crate::budget::PageBudget + 'static) -> Self {
        self.page_budget = Some(Arc::new(page_budget));
        self
    }

    /// Inject a pre-built [`BrowserPool`] for chromiumoxide-backed browser fetches.
    ///
    /// When set, the engine reuses this pool across all crawl operations rather than
    /// constructing a new pool per engine or per request. Intended for long-lived
    /// worker processes that need to amortise Chrome startup cost.
    ///
    /// The injected pool takes precedence over any pool stored in `CrawlConfig.browser_pool`.
    ///
    /// [`BrowserPool`]: crate::browser_pool::BrowserPool
    #[cfg(feature = "browser")]
    pub fn with_browser_pool(mut self, pool: Arc<crate::browser_pool::BrowserPool>) -> Self {
        self.browser_pool = Some(pool);
        self
    }

    /// Inject a pre-built [`NativeBrowserExecutor`] for native-backend browser fetches.
    ///
    /// When set, the engine reuses this executor across all crawl operations rather than
    /// constructing a new thread-pool per engine. Intended for long-lived worker processes
    /// that need to amortise native browser worker startup cost.
    ///
    /// The injected executor takes precedence over the one constructed from config.
    ///
    /// [`NativeBrowserExecutor`]: crawlberg_browser::adapter::NativeBrowserExecutor
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
    pub fn with_native_executor(mut self, executor: Arc<crawlberg_browser::adapter::NativeBrowserExecutor>) -> Self {
        self.native_executor = Some(executor);
        self
    }

    /// Inject a [`crate::ProxyProvider`] for per-request proxy rotation on the
    /// reqwest HTTP path. Stored on the resolved [`CrawlConfig`] as
    /// `proxy_provider`; takes precedence over the static
    /// `CrawlConfig::proxy` value when both are set.
    ///
    /// Browser-backend proxies (`CrawlConfig::browser::proxy`) still read the
    /// static `ProxyConfig` value — provider rotation only applies to the HTTP
    /// fetcher.
    pub fn with_proxy_provider(mut self, provider: Arc<dyn crate::ProxyProvider>) -> Self {
        self.proxy_provider = Some(provider);
        self
    }

    /// Build the [`CrawlEngine`] with the configured options.
    ///
    /// Config validation is deferred to the first operation (scrape, crawl, etc.) so that
    /// the engine can always be constructed and individual operations report validation errors.
    pub fn build(self) -> Result<CrawlEngine, CrawlError> {
        #[allow(unused_mut)]
        let mut config = self.config.unwrap_or_default();

        #[cfg(feature = "browser")]
        if let Some(pool) = self.browser_pool {
            config.browser_pool = Some(pool);
        }

        if let Some(provider) = self.proxy_provider {
            config.proxy_provider = Some(provider);
        }

        // ~keep `ssrf.deny_private` alone cannot prove caller intent — several alef-generated
        // bindings hand us `SsrfPolicy::default()` (deny=true) whenever their caller never
        // touched SSRF settings, so a bare `true` is as likely to be a binding's structural
        // default as a deliberate choice. `ssrf_deny_private_explicit` is the only reliable
        // "the caller meant it" signal: when set, honor it verbatim and skip the env var
        // entirely; otherwise keep applying `CRAWLBERG_ALLOW_PRIVATE_NETWORK` as the
        // operator-level default it has always been, so binding defaults still cannot hide it.
        if let Some(explicit) = config.ssrf_deny_private_explicit {
            config.ssrf.deny_private = explicit;
        } else if std::env::var("CRAWLBERG_ALLOW_PRIVATE_NETWORK")
            .map(|v| v.to_lowercase())
            .ok()
            .is_some_and(|v| v == "1" || v == "true")
        {
            config.ssrf.deny_private = false;
        }

        // ~keep Empty `scheme_allowlist` cannot be binding caller intent because the field is serde/alef skipped.
        // ~keep Treat empty as the default allowlist; otherwise HTTP/HTTPS requests fail as disallowed schemes.
        if config.ssrf.scheme_allowlist.is_empty() {
            config.ssrf.scheme_allowlist = crate::net::ssrf::default_scheme_allowlist();
        }

        let rate_limit_ms = config.rate_limit_ms.unwrap_or(200);
        #[cfg(not(target_arch = "wasm32"))]
        let ua_rotation = crate::tower::UaRotationLayer::new(config.user_agents.clone());

        #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
        let native_browser_executor = if let Some(executor) = self.native_executor {
            Some(executor)
        } else {
            build_native_browser_executor(&config)?
        };

        #[cfg(not(target_arch = "wasm32"))]
        let event_sink = attach_warc_sink(&config, self.event_sink)?;

        // ~keep Traversal order is a property of the frontier, not the strategy: the engine
        // hands the strategy a bounded selection window, so `DfsStrategy` over a FIFO queue
        // reorders only what has already been dequeued and does not crawl depth-first.
        // `crawl_strategy` therefore picks both halves, and an explicitly supplied frontier or
        // strategy still wins.
        let crawl_strategy = config.crawl_strategy;
        let content_filter = match config.content_filter {
            Some(ContentFilterKind::Bm25) => config
                .bm25_query
                .clone()
                .map(|query| (query, config.bm25_threshold.unwrap_or(0.0))),
            None => None,
        };

        Ok(CrawlEngine {
            config,
            frontier: self.frontier.unwrap_or_else(|| match crawl_strategy {
                CrawlStrategyKind::Dfs => Arc::new(defaults::LifoFrontier::new()),
                _ => Arc::new(defaults::InMemoryFrontier::new()),
            }),
            rate_limiter: self.rate_limiter.unwrap_or_else(|| {
                Arc::new(defaults::PerDomainThrottle::new(std::time::Duration::from_millis(
                    rate_limit_ms,
                )))
            }),
            store: self.store.unwrap_or_else(|| Arc::new(defaults::NoopStore)),
            event_emitter: self.event_emitter.unwrap_or_else(|| Arc::new(defaults::NoopEmitter)),
            strategy: self.strategy.unwrap_or_else(|| match crawl_strategy {
                CrawlStrategyKind::Bfs => Arc::new(defaults::BfsStrategy),
                CrawlStrategyKind::Dfs => Arc::new(defaults::DfsStrategy),
                CrawlStrategyKind::BestFirst => Arc::new(defaults::BestFirstStrategy),
                CrawlStrategyKind::Adaptive => Arc::new(defaults::AdaptiveStrategy::default()),
            }),
            content_filter: self.content_filter.unwrap_or_else(|| match content_filter {
                Some((query, threshold)) => Arc::new(defaults::Bm25Filter::new(&query, threshold)),
                None => Arc::new(defaults::NoopFilter),
            }),
            cache: self.cache.unwrap_or_else(|| Arc::new(defaults::NoopCache)),
            #[cfg(not(target_arch = "wasm32"))]
            event_sink,
            page_budget: self
                .page_budget
                .unwrap_or_else(|| Arc::new(crate::budget::DefaultPageBudget)),
            #[cfg(not(target_arch = "wasm32"))]
            ua_rotation,
            #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
            native_browser_executor,
        })
    }
}

/// Attach a WARC-writing sink when `config.warc_output` is set.
///
/// ~keep `warc_output` is exposed by every language binding and was previously read
/// by nothing, so a caller that set it got a successful crawl and no file. When the
/// `warc` feature is compiled out the request cannot be honoured at all, so it warns
/// rather than failing silently — the one thing this must never do again is accept
/// the option and do nothing without saying so.
#[cfg(not(target_arch = "wasm32"))]
fn attach_warc_sink(
    config: &CrawlConfig,
    existing: Option<Arc<dyn EventSink>>,
) -> Result<Option<Arc<dyn EventSink>>, CrawlError> {
    let Some(path) = config.warc_output.as_ref() else {
        return Ok(existing);
    };

    #[cfg(not(feature = "warc"))]
    {
        tracing::warn!(
            path = %path.display(),
            "warc_output is set but this build has the `warc` feature disabled; no WARC file will be written"
        );
        Ok(existing)
    }

    #[cfg(feature = "warc")]
    {
        let warc_sink: Arc<dyn EventSink> = Arc::new(crate::warc::WarcEventSink::new(path)?);
        Ok(Some(match existing {
            Some(existing) => Arc::new(crate::sink::MultiEventSink::new(vec![existing, warc_sink])),
            None => warc_sink,
        }))
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
fn build_native_browser_executor(
    config: &CrawlConfig,
) -> Result<Option<Arc<crawlberg_browser::adapter::NativeBrowserExecutor>>, CrawlError> {
    if config.browser.backend != BrowserBackend::Native {
        return Ok(None);
    }

    let executor_config = match config.max_concurrent {
        Some(workers) if workers > 0 => crawlberg_browser::adapter::NativeBrowserExecutorConfig::with_workers(workers),
        _ => crawlberg_browser::adapter::NativeBrowserExecutorConfig::default(),
    };
    let executor = crawlberg_browser::adapter::NativeBrowserExecutor::new(executor_config)
        .map_err(|e| CrawlError::browser_error(format!("failed to start native browser executor: {e}")))?;
    Ok(Some(Arc::new(executor)))
}

impl Default for CrawlEngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}
