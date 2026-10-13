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
    map_rate_limiter: Option<Arc<dyn RateLimiter>>,
    /// The limiter of the browser paths, when it is not the decision of `build`: the one of
    /// another engine, shared through `politeness`.
    #[cfg(any(feature = "browser", feature = "browser-native"))]
    browser_rate_limiter: Option<Arc<dyn RateLimiter>>,
    store: Option<Arc<dyn CrawlStore>>,
    event_emitter: Option<Arc<dyn EventEmitter>>,
    strategy: Option<Arc<dyn CrawlStrategy>>,
    content_filter: Option<Arc<dyn ContentFilter>>,
    document_filter: Option<Arc<crate::document::DocumentFilter>>,
    cache: Option<Arc<dyn CrawlCache>>,
    #[cfg(not(target_arch = "wasm32"))]
    event_sink: Option<Arc<dyn EventSink>>,
    page_budget: Option<Arc<dyn crate::budget::PageBudget>>,
    #[cfg(feature = "browser")]
    browser_pool: Option<Arc<crate::browser_pool::BrowserPool>>,
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
    native_executor: Option<Arc<crawlberg_browser::adapter::NativeBrowserExecutor>>,
    proxy_provider: Option<Arc<dyn crate::ProxyProvider>>,
    #[cfg(not(target_arch = "wasm32"))]
    robots_cache: Option<Arc<super::robots_cache::RobotsCache>>,
}

impl CrawlEngineBuilder {
    /// Create a new builder with no fields set.
    pub fn new() -> Self {
        Self {
            config: None,
            frontier: None,
            rate_limiter: None,
            map_rate_limiter: None,
            #[cfg(any(feature = "browser", feature = "browser-native"))]
            browser_rate_limiter: None,
            store: None,
            event_emitter: None,
            strategy: None,
            content_filter: None,
            document_filter: None,
            cache: None,
            #[cfg(not(target_arch = "wasm32"))]
            event_sink: None,
            page_budget: None,
            #[cfg(feature = "browser")]
            browser_pool: None,
            #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
            native_executor: None,
            proxy_provider: None,
            #[cfg(not(target_arch = "wasm32"))]
            robots_cache: None,
        }
    }

    /// Share the rate limiter and the robots.txt cache of another engine.
    #[cfg(feature = "api")]
    pub(crate) fn politeness(mut self, politeness: super::Politeness) -> Self {
        self.rate_limiter = Some(politeness.rate_limiter);
        self.map_rate_limiter = Some(politeness.map_rate_limiter);
        #[cfg(any(feature = "browser", feature = "browser-native"))]
        {
            self.browser_rate_limiter = Some(politeness.browser_rate_limiter);
        }
        self.robots_cache = Some(politeness.robots_cache);
        self
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

    /// Set the rate limiter implementation. Every fetch takes a turn of it, in HTTP mode and in
    /// browser mode alike.
    #[allow(dead_code)]
    pub fn rate_limiter(mut self, rate_limiter: impl RateLimiter + 'static) -> Self {
        let rate_limiter: Arc<dyn RateLimiter> = Arc::new(rate_limiter);
        #[cfg(any(feature = "browser", feature = "browser-native"))]
        {
            self.browser_rate_limiter = Some(Arc::clone(&rate_limiter));
        }
        self.map_rate_limiter = Some(Arc::clone(&rate_limiter));
        self.rate_limiter = Some(rate_limiter);
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

    /// Set a byte-aware predicate for document materialization.
    ///
    /// The predicate receives the normalized declared MIME type, at most
    /// `document_max_size` bytes of the already bounded response body, and the decision
    /// `document_mime_types`/the built-in classification would have reached. Returning that
    /// third argument reproduces the default; `by_declared_mime || bytes.starts_with(b"%PDF")`
    /// widens it. It applies to `crawl()`, `scrape()` and the wasm crawl loop alike. With no
    /// predicate, the existing MIME decision is unchanged.
    ///
    /// The predicate runs for **every** fetched response, not only the ones the built-in
    /// decision would have accepted — an ordinary HTML page included. A predicate that returns
    /// `true` for HTML therefore materializes every page as a `DownloadedDocument`, duplicating
    /// its whole body into the result and, on native targets, writing it to
    /// `document_output_dir`. Keep the predicate as narrow as the documents it is meant to admit.
    pub fn document_filter(
        mut self,
        document_filter: impl Fn(&str, &[u8], bool) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.document_filter = Some(Arc::new(document_filter));
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

    /// Inject a [`crate::ProxyProvider`] for per-request proxy rotation. Stored
    /// on the resolved [`CrawlConfig`] as `proxy_provider`; takes precedence
    /// over the static `CrawlConfig::proxy` value when both are set.
    ///
    /// A native browser render asks the provider once for the page's host and
    /// sends the page and all of its requests through that proxy.
    /// `CrawlConfig::browser::proxy`, when set, wins over the provider for
    /// renders. The Chrome backend cannot render through a provider, so a
    /// Chrome render with a provider and no `browser.proxy` fails.
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

        resolve_ssrf_deny_private(&mut config);

        // ~keep Always replaced, never kept: a configuration cloned from another engine carries
        // ~keep that engine's store, and a new engine must start with no cookie.
        #[cfg(not(target_arch = "wasm32"))]
        {
            config.cookie_store = config.cookies_enabled.then(Default::default);
        }

        let rate_limiter: Arc<dyn RateLimiter> = self.rate_limiter.unwrap_or_else(|| {
            Arc::new(default_rate_limiter(
                config.rate_limit_ms.unwrap_or(DEFAULT_RATE_LIMIT_MS),
                config.rate_limit_jitter_ratio,
            ))
        });
        // ~keep The only place where the default delay reaches the browser paths. A page of
        // ~keep Chrome and a native render take a turn of the throttle only when the caller set
        // ~keep `rate_limit_ms` or installed a limiter: with the default settings, browser mode
        // ~keep loads the pages of one host with no delay, as before #622 (the owner refused a
        // ~keep slower default). HTTP mode keeps its default delay.
        #[cfg(any(feature = "browser", feature = "browser-native"))]
        let browser_rate_limiter: Arc<dyn RateLimiter> = match self.browser_rate_limiter {
            Some(shared) => shared,
            None if config.rate_limit_ms.is_some() => Arc::clone(&rate_limiter),
            None => Arc::new(defaults::NoopRateLimiter),
        };
        // ~keep Maps retain their zero-delay default while honoring configured/custom pacing.
        // ~keep The zero-delay throttle can adopt robots Crawl-delay; statuses still reach configured/custom backoff.
        let map_rate_limiter: Arc<dyn RateLimiter> = match self.map_rate_limiter {
            Some(shared) => shared,
            None if config.rate_limit_ms.is_some() => Arc::clone(&rate_limiter),
            None => Arc::new(default_rate_limiter(0, config.rate_limit_jitter_ratio)),
        };
        let ua_rotation = crate::tower::UaRotation::new(config.user_agents.clone());

        #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
        let native_browser_executor = if let Some(executor) = self.native_executor {
            Some(executor)
        } else {
            build_native_browser_executor(&config)?
        };

        #[cfg(not(target_arch = "wasm32"))]
        let event_sink = attach_warc_sink(&config, self.event_sink)?;

        let crawl_strategy = config.crawl_strategy;
        let bm25_filter = resolve_bm25_filter(&config);

        Ok(CrawlEngine {
            config,
            frontier: super::frontier_claim::GuardedFrontier::wrap(
                self.frontier.unwrap_or_else(|| default_frontier(crawl_strategy)),
            ),
            rate_limiter,
            map_rate_limiter,
            #[cfg(any(feature = "browser", feature = "browser-native"))]
            browser_rate_limiter,
            store: self.store.unwrap_or_else(|| Arc::new(defaults::NoopStore)),
            event_emitter: self.event_emitter.unwrap_or_else(|| Arc::new(defaults::NoopEmitter)),
            strategy: self.strategy.unwrap_or_else(|| default_strategy(crawl_strategy)),
            content_filter: self
                .content_filter
                .unwrap_or_else(|| default_content_filter(bm25_filter)),
            document_filter: self.document_filter,
            cache: self.cache.unwrap_or_else(|| Arc::new(defaults::NoopCache)),
            #[cfg(not(target_arch = "wasm32"))]
            event_sink,
            page_budget: self
                .page_budget
                .unwrap_or_else(|| Arc::new(crate::budget::DefaultPageBudget)),
            ua_rotation,
            #[cfg(not(target_arch = "wasm32"))]
            robots_cache: self.robots_cache.unwrap_or_default(),
            #[cfg(not(target_arch = "wasm32"))]
            crawl_schedule: None,
            #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
            native_browser_executor,
            #[cfg(feature = "browser")]
            crawl_cookies: None,
        })
    }
}

/// Build the default per-domain throttle.
///
/// ~keep A named function rather than an inline closure so the config-to-throttle plumbing is
/// ~keep directly assertable: `rate_limit_jitter_ratio` was added to `CrawlConfig` and to
/// ~keep `PerDomainThrottle` while nothing joined them, so the setting parsed, validated and
/// ~keep serialized while a live crawl silently got no jitter at all.
fn default_rate_limiter(rate_limit_ms: u64, jitter_ratio: f64) -> defaults::PerDomainThrottle {
    defaults::PerDomainThrottle::with_jitter_ratio(std::time::Duration::from_millis(rate_limit_ms), jitter_ratio)
}

/// Default per-domain throttle interval when `rate_limit_ms` is unset.
const DEFAULT_RATE_LIMIT_MS: u64 = 200;

/// Default BM25 relevance threshold when `bm25_threshold` is unset.
const DEFAULT_BM25_THRESHOLD: f64 = 0.0;

/// Apply the operator-level SSRF override unless the caller pinned `deny_private` explicitly.
///
/// ~keep `ssrf.deny_private` alone cannot prove caller intent — several alef-generated
/// bindings hand us `SsrfPolicy::default()` (deny=true) whenever their caller never
/// touched SSRF settings, so a bare `true` is as likely to be a binding's structural
/// default as a deliberate choice. `ssrf_deny_private_explicit` is the only reliable
/// "the caller meant it" signal: when set, honor it verbatim and skip the env var
/// entirely; otherwise keep applying `CRAWLBERG_ALLOW_PRIVATE_NETWORK` as the
/// operator-level default it has always been, so binding defaults still cannot hide it.
fn resolve_ssrf_deny_private(config: &mut CrawlConfig) {
    if let Some(explicit) = config.ssrf_deny_private_explicit {
        config.ssrf.deny_private = explicit;
    } else if std::env::var("CRAWLBERG_ALLOW_PRIVATE_NETWORK")
        .map(|v| v.to_lowercase())
        .ok()
        .is_some_and(|v| v == "1" || v == "true")
    {
        config.ssrf.deny_private = false;
    }
}

/// Resolve the BM25 query and threshold, if BM25 content filtering is configured.
fn resolve_bm25_filter(config: &CrawlConfig) -> Option<(String, f64)> {
    match config.content_filter {
        Some(ContentFilterKind::Bm25) => config
            .bm25_query
            .clone()
            .map(|query| (query, config.bm25_threshold.unwrap_or(DEFAULT_BM25_THRESHOLD))),
        None => None,
    }
}

/// Pick the frontier queue discipline that matches the configured traversal strategy.
///
/// ~keep Traversal order is a property of the frontier, not the strategy: the engine
/// hands the strategy a bounded selection window, so `DfsStrategy` over a FIFO queue
/// reorders only what has already been dequeued and does not crawl depth-first.
/// `crawl_strategy` therefore picks both halves, and an explicitly supplied frontier or
/// strategy still wins.
fn default_frontier(crawl_strategy: CrawlStrategyKind) -> Arc<dyn Frontier> {
    match crawl_strategy {
        CrawlStrategyKind::Dfs => Arc::new(defaults::LifoFrontier::new()),
        _ => Arc::new(defaults::InMemoryFrontier::new()),
    }
}

/// Pick the selection strategy for the configured traversal kind.
fn default_strategy(crawl_strategy: CrawlStrategyKind) -> Arc<dyn CrawlStrategy> {
    match crawl_strategy {
        CrawlStrategyKind::Bfs => Arc::new(defaults::BfsStrategy),
        CrawlStrategyKind::Dfs => Arc::new(defaults::DfsStrategy),
        CrawlStrategyKind::BestFirst => Arc::new(defaults::BestFirstStrategy),
        CrawlStrategyKind::Adaptive => Arc::new(defaults::AdaptiveStrategy::default()),
    }
}

/// Build the content filter from a resolved BM25 query, or a no-op filter when absent.
fn default_content_filter(bm25_filter: Option<(String, f64)>) -> Arc<dyn ContentFilter> {
    match bm25_filter {
        Some((query, threshold)) => Arc::new(defaults::Bm25Filter::new(&query, threshold)),
        None => Arc::new(defaults::NoopFilter),
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

#[cfg(test)]
mod cookie_store_tests {
    use std::sync::Arc;

    use crate::engine::CrawlEngine;
    use crate::types::CrawlConfig;

    fn engine_with(config: CrawlConfig) -> CrawlEngine {
        CrawlEngine::builder()
            .config(config)
            .build()
            .expect("the engine builds")
    }

    // ~keep The API server builds the engine of each request from the configuration of its
    // ~keep long-lived engine, so a store that the builder kept would be one store for every
    // ~keep request of every API client.
    #[test]
    fn an_engine_built_from_the_configuration_of_another_engine_has_a_store_of_its_own() {
        let first = engine_with(CrawlConfig {
            cookies_enabled: true,
            ..CrawlConfig::default()
        });
        let second = engine_with(first.config.clone());

        let first = first
            .config
            .cookie_store
            .expect("an engine with cookies on has a store");
        let second = second
            .config
            .cookie_store
            .expect("an engine with cookies on has a store");
        assert!(!Arc::ptr_eq(&first, &second), "each engine must have its own store");
    }

    #[test]
    fn an_engine_with_cookies_off_drops_the_store_its_configuration_carries() {
        let first = engine_with(CrawlConfig {
            cookies_enabled: true,
            ..CrawlConfig::default()
        });
        let second = engine_with(CrawlConfig {
            cookies_enabled: false,
            ..first.config.clone()
        });

        assert!(second.config.cookie_store.is_none());
    }

    #[test]
    fn a_clone_of_an_engine_is_the_same_cookie_session() {
        let engine = engine_with(CrawlConfig {
            cookies_enabled: true,
            ..CrawlConfig::default()
        });
        let clone = engine.clone();

        let (first, second) = (engine.config.cookie_store, clone.config.cookie_store);
        assert!(
            Arc::ptr_eq(&first.expect("a store"), &second.expect("a store")),
            "a clone of an engine must share its store"
        );
    }

    #[test]
    fn an_engine_with_default_configuration_has_no_store() {
        assert!(engine_with(CrawlConfig::default()).config.cookie_store.is_none());
    }
}

#[cfg(test)]
mod env_private_network_precedence_tests {
    use crate::engine::CrawlEngine;
    use crate::net::SsrfPolicy;
    use crate::types::CrawlConfig;

    /// Resolves `deny_private` the way `CrawlEngineBuilder::build` does, with
    /// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` set to `value` for the duration of the build.
    ///
    // ~keep These two tests are the only place that may write this variable in this binary,
    // and both are `#[serial]`. `serial_test` does not exclude *non*-serial tests, and this
    // binary has many that read the variable through `CrawlConfig::default` ->
    // `SsrfPolicy::from_env`; on glibc a concurrent `setenv` can realloc `environ` under a
    // `getenv` and abort the process with no failing test name. Keep the write window as
    // narrow as possible and never add a third writer.
    #[allow(unsafe_code)]
    fn deny_private_after_build_with_env(config: CrawlConfig, value: &str) -> bool {
        // ~keep SAFETY: #[serial] on both callers prevents concurrent serial env access.
        unsafe { std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", value) };
        let engine = CrawlEngine::builder().config(config).build();
        unsafe { std::env::remove_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK") };
        engine.expect("engine build must not fail").config.ssrf.deny_private
    }

    /// Regression test for rc.77: `CRAWLBERG_ALLOW_PRIVATE_NETWORK` must override a
    /// hardcoded `deny_private: true` carried on `CrawlConfig.ssrf`. Several alef-generated
    /// bindings (Elixir NIF, PHP, WASM, Ruby) build their config with `SsrfPolicy::default()`
    /// (deny=true) when the host-side `ssrf` field is absent, silently overriding the env var
    /// their e2e harnesses set. `CrawlEngineBuilder::build` must apply the env override so the
    /// operator flag wins regardless of how the policy reached the engine.
    #[test]
    #[serial_test::serial]
    fn env_bypass_overrides_ambient_deny_private() {
        let config = CrawlConfig {
            ssrf: SsrfPolicy::default(),
            ..CrawlConfig::default()
        };
        assert!(
            config.ssrf.deny_private,
            "precondition: SsrfPolicy::default() must hardcode deny_private=true"
        );

        assert!(
            !deny_private_after_build_with_env(config, "true"),
            "engine builder must apply CRAWLBERG_ALLOW_PRIVATE_NETWORK over an ambient deny_private=true"
        );
    }

    /// Regression test for #22: `ssrf_deny_private_explicit` must survive
    /// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` — a caller that pins `deny_private: true` via that
    /// field keeps denying private networks even while the operator env var is set suite-wide.
    ///
    /// Counterpart to `env_bypass_overrides_ambient_deny_private`: that test proves the env var
    /// wins when a binding hands us an *ambient* `SsrfPolicy::default()` with no way to prove
    /// intent; this one proves a caller with a *provable* intent is never overridden.
    #[test]
    #[serial_test::serial]
    fn explicit_deny_private_survives_env_bypass() {
        let config = CrawlConfig {
            ssrf: SsrfPolicy {
                deny_private: true,
                ..SsrfPolicy::default()
            },
            ssrf_deny_private_explicit: Some(true),
            ..CrawlConfig::default()
        };

        assert!(
            deny_private_after_build_with_env(config, "true"),
            "ssrf_deny_private_explicit=Some(true) must survive CRAWLBERG_ALLOW_PRIVATE_NETWORK=true"
        );
    }
}

#[cfg(test)]
mod rate_limiter_plumbing_tests {
    use std::time::Duration;

    use super::CrawlEngine;
    use crate::types::CrawlConfig;

    // ~keep These assert the CONFIG-TO-THROTTLE PLUMBING actually exercised by `build()`, not
    // the jitter maths (which is unit tested in defaults::rate_limiter). An earlier version of
    // this suite called `default_rate_limiter(100, config.rate_limit_jitter_ratio)` directly --
    // reconstructing the exact expression at builder.rs:246 rather than observing it -- so
    // cutting that line to a hardcoded ratio left every test here green (`rate_limit_jitter_ratio`
    // parsed, validated and serialized while a live crawl got no jitter at all). These build a
    // real `CrawlEngine` through `CrawlEngineBuilder` and observe `engine.rate_limiter.acquire()`
    // under a paused tokio clock: `tokio::time::sleep` inside `PerDomainThrottle::acquire` still
    // runs, but the paused clock auto-advances to the sleep's deadline with no wall-clock noise,
    // so the observed wait is exactly the throttle's computed `sleep_duration` -- deterministic,
    // not "timing that happened to be small".
    async fn wait_for_second_acquire(engine: &CrawlEngine, domain: &str) -> Duration {
        engine.rate_limiter.acquire(domain).await.expect("first acquire");
        let start = tokio::time::Instant::now();
        engine.rate_limiter.acquire(domain).await.expect("second acquire");
        start.elapsed()
    }

    #[tokio::test(start_paused = true)]
    async fn configured_jitter_ratio_reaches_the_default_throttle() {
        let config = CrawlConfig {
            rate_limit_ms: Some(100),
            rate_limit_jitter_ratio: 0.5,
            ..CrawlConfig::default()
        };
        let engine = CrawlEngine::builder()
            .config(config)
            .build()
            .expect("engine must build");

        // ~keep One draw cannot carry this assertion. tokio's paused clock advances to a
        // sleep's deadline at whole-millisecond granularity, so a 0.5 ratio over 100ms has
        // exactly 100 reachable outcomes in [51ms, 150ms] and lands on an unperturbed 100ms
        // in 1 of 100 draws -- measured, not estimated. A single `assert_ne!` against 100ms
        // was therefore ~1% flaky, under a comment claiming it could only collide at
        // double-precision float equality; that reasoning was about the f64 factor and
        // missed the timer's rounding. Several domains fix it: each is an independent draw,
        // so requiring at least one to differ collides by chance at 0.01^4 = 1e-8, and the
        // range check still applies to every draw.
        let domains = ["a.example.com", "b.example.com", "c.example.com", "d.example.com"];
        let mut waits = Vec::with_capacity(domains.len());
        for domain in domains {
            let waited = wait_for_second_acquire(&engine, domain).await;
            assert!(
                waited >= Duration::from_millis(50) && waited <= Duration::from_millis(150),
                "a 0.5 jitter_ratio over a 100ms delay must stay within [50ms, 150ms], \
                 got {waited:?} for {domain}"
            );
            waits.push(waited);
        }

        assert!(
            waits.iter().any(|waited| *waited != Duration::from_millis(100)),
            "a nonzero jitter_ratio must perturb the delay away from the unjittered 100ms \
             baseline, but all {} domains waited exactly 100ms: {waits:?}",
            waits.len()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn default_config_leaves_the_throttle_unjittered() {
        let config = CrawlConfig {
            rate_limit_ms: Some(100),
            ..CrawlConfig::default()
        };
        let engine = CrawlEngine::builder()
            .config(config)
            .build()
            .expect("engine must build");

        let waited = wait_for_second_acquire(&engine, "example.com").await;

        assert_eq!(
            waited,
            Duration::from_millis(100),
            "an unset ratio must leave the throttle behaving exactly as before jitter existed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_out_of_range_ratio_is_clamped_before_it_reaches_the_throttle() {
        let config = CrawlConfig {
            rate_limit_ms: Some(100),
            rate_limit_jitter_ratio: 5.0,
            ..CrawlConfig::default()
        };
        let engine = CrawlEngine::builder()
            .config(config)
            .build()
            .expect("engine must build");

        let waited = wait_for_second_acquire(&engine, "example.com").await;

        assert!(
            waited <= Duration::from_millis(200),
            "a ratio above 1.0 must clamp to 1.0 (max 2x the 100ms delay), not scale it by 5x \
             (up to 600ms), got {waited:?}"
        );
    }
}

/// The delay of the domain in browser mode: the Chrome page and the native render take a turn
/// of the limiter the builder gives the browser paths. These tests observe that limiter under a
/// paused tokio clock, as `rate_limiter_plumbing_tests` does: the wait is the computed one.
#[cfg(all(test, any(feature = "browser", feature = "browser-native")))]
mod browser_rate_limiter_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::CrawlEngine;
    use crate::types::CrawlConfig;

    const DOMAIN: &str = "example.com";

    async fn second_turn_of_the_browser_paths(engine: &CrawlEngine) -> Duration {
        engine.browser_rate_limiter.acquire(DOMAIN).await.expect("first turn");
        let start = tokio::time::Instant::now();
        engine.browser_rate_limiter.acquire(DOMAIN).await.expect("second turn");
        start.elapsed()
    }

    async fn second_turn_of_http(engine: &CrawlEngine) -> Duration {
        engine.rate_limiter.acquire(DOMAIN).await.expect("first turn");
        let start = tokio::time::Instant::now();
        engine.rate_limiter.acquire(DOMAIN).await.expect("second turn");
        start.elapsed()
    }

    fn engine_with(config: CrawlConfig) -> CrawlEngine {
        CrawlEngine::builder()
            .config(config)
            .build()
            .expect("engine must build")
    }

    // ~keep The owner refused a slower default: a crawl with the default settings loads the
    // ~keep pages of one host in browser mode as it did before the delay reached the browser
    // ~keep paths, with no delay, while HTTP mode keeps its default delay.
    #[tokio::test(start_paused = true)]
    async fn with_the_default_settings_the_browser_paths_get_their_turns_at_once() {
        let engine = engine_with(CrawlConfig::default());

        assert_eq!(
            second_turn_of_the_browser_paths(&engine).await,
            Duration::ZERO,
            "with rate_limit_ms unset, the second page of a host must get its turn at once in browser mode"
        );
        assert_eq!(
            second_turn_of_http(&engine).await,
            Duration::from_millis(200),
            "HTTP mode keeps the default delay of 200 ms"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_set_rate_limit_ms_spaces_the_turns_of_the_browser_paths() {
        let engine = engine_with(CrawlConfig {
            rate_limit_ms: Some(300),
            ..CrawlConfig::default()
        });

        assert_eq!(
            second_turn_of_the_browser_paths(&engine).await,
            Duration::from_millis(300),
            "with rate_limit_ms set, the second page of a host gets its turn one delay after the first"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_rate_limit_ms_of_zero_spaces_nothing_in_the_browser_paths() {
        let engine = engine_with(CrawlConfig {
            rate_limit_ms: Some(0),
            ..CrawlConfig::default()
        });

        assert_eq!(second_turn_of_the_browser_paths(&engine).await, Duration::ZERO);
    }

    #[test]
    fn an_installed_rate_limiter_is_the_one_of_the_browser_paths_too() {
        let engine = CrawlEngine::builder()
            .config(CrawlConfig::default())
            .rate_limiter(crate::defaults::PerDomainThrottle::new(Duration::from_millis(50)))
            .build()
            .expect("engine must build");

        assert!(
            Arc::ptr_eq(&engine.rate_limiter, &engine.browser_rate_limiter),
            "a limiter the caller installed must take the turns of the browser paths"
        );
    }

    // ~keep The API server builds the engine of each request from its own politeness, so the
    // ~keep decision of the server's build must reach the engine of the request unchanged: with
    // ~keep the default settings, no delay in browser mode there either.
    #[cfg(feature = "api")]
    #[tokio::test(start_paused = true)]
    async fn an_engine_that_shares_the_politeness_of_another_keeps_its_browser_delay() {
        let server = engine_with(CrawlConfig::default());
        let request = CrawlEngine::builder()
            .config(server.config.clone())
            .politeness(server.politeness())
            .build()
            .expect("engine must build");

        assert!(Arc::ptr_eq(&server.rate_limiter, &request.rate_limiter));
        assert_eq!(second_turn_of_the_browser_paths(&request).await, Duration::ZERO);

        let server = engine_with(CrawlConfig {
            rate_limit_ms: Some(300),
            ..CrawlConfig::default()
        });
        let request = CrawlEngine::builder()
            .config(server.config.clone())
            .politeness(server.politeness())
            .build()
            .expect("engine must build");
        assert_eq!(
            second_turn_of_the_browser_paths(&request).await,
            Duration::from_millis(300)
        );
    }
}

#[cfg(test)]
mod map_rate_limiter_tests {
    use super::CrawlEngine;
    use crate::types::CrawlConfig;
    use std::time::Duration;

    async fn second_map_turn(engine: &CrawlEngine) -> Duration {
        let limiter = engine.map_rate_limiter();
        limiter.acquire("example.com").await.expect("first turn");
        let start = tokio::time::Instant::now();
        limiter.acquire("example.com").await.expect("second turn");
        start.elapsed()
    }

    #[tokio::test(start_paused = true)]
    async fn default_maps_have_no_host_delay_while_http_keeps_its_default() {
        let engine = CrawlEngine::builder().build().expect("engine");
        assert_eq!(second_map_turn(&engine).await, Duration::ZERO);
        engine
            .rate_limiter
            .acquire("http.example.com")
            .await
            .expect("first HTTP turn");
        let start = tokio::time::Instant::now();
        engine
            .rate_limiter
            .acquire("http.example.com")
            .await
            .expect("second HTTP turn");
        let waited = start.elapsed();
        assert_eq!(waited, Duration::from_millis(200));
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_map_delay_is_used_for_every_host_turn() {
        let engine = CrawlEngine::builder()
            .config(CrawlConfig {
                rate_limit_ms: Some(150),
                rate_limit_jitter_ratio: 0.0,
                ..CrawlConfig::default()
            })
            .build()
            .expect("engine");
        let limiter = engine.map_rate_limiter();
        limiter.acquire("example.com").await.expect("robots turn");
        for _ in 0..3 {
            let start = tokio::time::Instant::now();
            limiter.acquire("example.com").await.expect("sitemap turn");
            assert_eq!(start.elapsed(), Duration::from_millis(150));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_map_pacing_backs_off_after_a_429_response() {
        let engine = CrawlEngine::builder()
            .config(CrawlConfig {
                rate_limit_ms: Some(150),
                rate_limit_jitter_ratio: 0.0,
                ..CrawlConfig::default()
            })
            .build()
            .expect("engine");
        let limiter = engine.map_rate_limiter();
        limiter.acquire("example.com").await.expect("first turn");
        limiter
            .record_response("example.com", 429)
            .await
            .expect("response status");
        let start = tokio::time::Instant::now();
        limiter.acquire("example.com").await.expect("backoff turn");
        assert_eq!(start.elapsed(), Duration::from_millis(300));
    }

    #[cfg(feature = "api")]
    #[tokio::test(start_paused = true)]
    async fn api_map_pacing_shares_default_and_configured_limiter_state() {
        for delay in [None, Some(150)] {
            let server = CrawlEngine::builder()
                .config(CrawlConfig {
                    rate_limit_ms: delay,
                    rate_limit_jitter_ratio: 0.0,
                    ..CrawlConfig::default()
                })
                .build()
                .expect("server engine");
            let request = CrawlEngine::builder()
                .politeness(server.politeness())
                .build()
                .expect("request engine");
            server
                .map_rate_limiter()
                .acquire("example.com")
                .await
                .expect("server turn");
            let start = tokio::time::Instant::now();
            request
                .map_rate_limiter()
                .acquire("example.com")
                .await
                .expect("request turn");
            assert_eq!(start.elapsed(), Duration::from_millis(delay.unwrap_or(0)));
            server
                .map_rate_limiter()
                .set_crawl_delay("example.com", Duration::from_secs(2))
                .await
                .expect("robots delay");
            let start = tokio::time::Instant::now();
            request
                .map_rate_limiter()
                .acquire("example.com")
                .await
                .expect("shared robots turn");
            assert_eq!(start.elapsed(), Duration::from_secs(2));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn default_map_limiter_honors_robots_crawl_delay() {
        let engine = CrawlEngine::builder().build().expect("engine");
        engine
            .map_rate_limiter()
            .set_crawl_delay("example.com", Duration::from_secs(2))
            .await
            .expect("robots delay");
        assert_eq!(second_map_turn(&engine).await, Duration::from_secs(2));
    }
}
