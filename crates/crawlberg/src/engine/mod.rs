//! CrawlEngine composes trait implementations into a crawl pipeline.

mod admission;
#[cfg(not(target_arch = "wasm32"))]
mod batch;
mod builder;
#[cfg(not(target_arch = "wasm32"))]
mod crawl_loop;
#[cfg(not(target_arch = "wasm32"))]
mod crawl_state;
#[cfg(not(target_arch = "wasm32"))]
mod dispatch;
#[cfg(not(target_arch = "wasm32"))]
mod fetch;
#[cfg(not(target_arch = "wasm32"))]
mod link_discovery;
mod link_scope;
#[cfg(not(target_arch = "wasm32"))]
mod page_result;
#[cfg(not(target_arch = "wasm32"))]
mod redirect;
#[cfg(not(target_arch = "wasm32"))]
mod robots_cache;
mod scrape_page;
mod selection;
// ~keep Also compiled under `cfg(test)` natively: see the module doc for why.
#[cfg(any(target_arch = "wasm32", test))]
mod wasm_crawl;

pub(crate) use admission::SeedUrl;
pub(crate) use selection::take_selected;

use std::sync::Arc;

use crate::error::CrawlError;

/// Default cap on links enqueued from one page, when `max_links_per_page` is unset.
///
/// ~keep Lives here rather than in `crawl_loop` because that module is native-only and
/// the wasm loop needs the same value; it previously carried its own copy.
pub(crate) const DEFAULT_MAX_LINKS_PER_PAGE: usize = 10_000;

#[cfg(not(target_arch = "wasm32"))]
use crate::sink::EventSink;
#[cfg(not(target_arch = "wasm32"))]
use crate::tower::CrawlRequest;
use crate::traits::*;
use crate::types::*;

pub use builder::CrawlEngineBuilder;

/// The main crawl engine, composed of pluggable trait implementations.
#[derive(Clone)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct CrawlEngine {
    pub(crate) config: CrawlConfig,
    pub(crate) frontier: Arc<dyn Frontier>,
    pub(crate) rate_limiter: Arc<dyn RateLimiter>,
    pub(crate) store: Arc<dyn CrawlStore>,
    pub(crate) event_emitter: Arc<dyn EventEmitter>,
    pub(crate) strategy: Arc<dyn CrawlStrategy>,
    pub(crate) content_filter: Arc<dyn ContentFilter>,
    pub(crate) document_filter: Option<Arc<crate::document::DocumentFilter>>,
    pub(crate) cache: Arc<dyn CrawlCache>,
    /// Optional event sink for streaming crawl events to external consumers
    /// (e.g., NATS, dashboards, analytics).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) event_sink: Option<Arc<dyn EventSink>>,
    /// Optional page budget hook for enforcing per-crawl page allowances.
    #[allow(dead_code)]
    pub(crate) page_budget: Arc<dyn crate::budget::PageBudget>,
    /// Shared UA rotation layer — preserves rotation counter across service builds.
    #[cfg(not(target_arch = "wasm32"))]
    ua_rotation: crate::tower::UaRotationLayer,
    #[cfg(not(target_arch = "wasm32"))]
    robots_cache: Arc<robots_cache::RobotsCache>,
    #[cfg(all(not(target_arch = "wasm32"), feature = "browser-native"))]
    pub(crate) native_browser_executor: Option<Arc<crawlberg_browser::adapter::NativeBrowserExecutor>>,
}

impl CrawlEngine {
    /// Create a new [`CrawlEngineBuilder`].
    pub fn builder() -> CrawlEngineBuilder {
        CrawlEngineBuilder::new()
    }

    /// Build the Tower service stack for HTTP fetching.
    ///
    /// Layers (outermost to innermost):
    /// 1. Per-domain rate limiting
    /// 2. HTTP response caching
    /// 3. User-agent rotation
    /// 4. Base HTTP fetch
    #[cfg(not(target_arch = "wasm32"))]
    fn build_service(
        &self,
        client: &reqwest::Client,
    ) -> tower::util::BoxCloneService<CrawlRequest, crate::tower::CrawlResponse, CrawlError> {
        use tower::ServiceBuilder;

        let service = ServiceBuilder::new()
            .layer(crate::tower::PerDomainRateLimitLayer::new(self.rate_limiter.clone()))
            .layer(
                crate::tower::CrawlCacheLayer::new(self.cache.clone())
                    .bypassing_credentials(Arc::new(self.config.clone())),
            )
            .layer(self.ua_rotation.clone())
            .service(crate::tower::HttpFetchService::new(client.clone(), self.config.clone()));

        let service = tower::ServiceBuilder::new()
            .layer(crate::tower::CrawlTracingLayer::new())
            .service(service);

        tower::util::BoxCloneService::new(service)
    }

    /// Whether `fetch_response` (`engine/fetch.rs`) sends this request straight to the browser
    /// tier, bypassing the Tower stack -- and with it, UA rotation -- entirely.
    ///
    /// ~keep Mirrors the exact condition `fetch_response` itself checks before routing:
    /// `feature = "browser"` compiled in and `BrowserMode::Always`/`Stealth` configured. Kept in
    /// one place so `choose_request_user_agent` cannot drift from what `fetch_response` actually
    /// does (crawlberg#423).
    #[cfg(not(target_arch = "wasm32"))]
    fn request_will_use_browser(&self) -> bool {
        cfg!(feature = "browser") && matches!(self.config.browser.mode, BrowserMode::Always | BrowserMode::Stealth)
    }

    /// Decide the agent the next request should send.
    ///
    /// ~keep The single place that picks an agent ahead of a request: it advances the exact
    /// round-robin counter the UA rotation layer uses, so pinning the result onto a
    /// `CrawlRequest` before it reaches that layer (`RedirectPolicy::admits`, for the robots
    /// decision) and the agent the layer would otherwise have chosen are never two different
    /// picks. Falls back to the configured default when no rotation list is set, so a
    /// non-rotating crawl sees no change (crawlberg#423).
    ///
    /// ~keep A request the browser tier will fetch never reaches UA rotation at all -- the
    /// browser always sends the configured or custom-header agent (`default_robots_user_agent`),
    /// never a rotated pick -- so robots decisions for it must judge that same agent, not one
    /// the browser will never send (crawlberg#423).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn choose_request_user_agent(&self) -> String {
        if self.request_will_use_browser() {
            return crate::helpers::default_robots_user_agent(&self.config).to_owned();
        }
        self.ua_rotation
            .choose_next()
            .unwrap_or_else(|| crate::helpers::default_robots_user_agent(&self.config).to_owned())
    }

    /// Execute browser actions on a single page.
    ///
    /// The public API is always available. Runtime execution depends on the
    /// configured browser backend and the browser backend features compiled
    /// into the crate.
    pub async fn interact(
        &self,
        url: &str,
        actions: &[crate::interact::PageAction],
    ) -> Result<InteractionResult, CrawlError> {
        let (engine, seed) = self.admit(url)?;
        engine.interact_seed(&seed, actions).await
    }

    /// Run browser actions on an admitted seed URL. See [`CrawlEngine::interact`].
    // ~keep `actions` may carry user-typed form text (TypeText), so only its length is traced.
    #[tracing::instrument(
        name = "crawl.engine.interact",
        skip_all,
        fields(url.full = %seed, action_count = actions.len())
    )]
    async fn interact_seed(
        &self,
        seed: &SeedUrl,
        actions: &[crate::interact::PageAction],
    ) -> Result<InteractionResult, CrawlError> {
        crate::interact::run(self, seed, actions).await
    }

    /// Discover all pages on a website by following links and sitemaps.
    pub async fn map(&self, url: &str) -> Result<MapResult, CrawlError> {
        let (engine, seed) = self.admit(url)?;
        engine.map_seed(&seed).await
    }

    /// Map an admitted seed URL. See [`CrawlEngine::map`].
    #[tracing::instrument(name = "crawl.engine.map", skip_all, fields(url.full = %seed))]
    async fn map_seed(&self, seed: &SeedUrl) -> Result<MapResult, CrawlError> {
        self.config.validate()?;
        crate::map::map(seed, &self.config).await
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// Minimal `Subscriber` that captures every field recorded on any span,
    /// used to assert on `#[tracing::instrument]` field values without adding
    /// a `tracing-subscriber` dev-dependency.
    #[derive(Default)]
    struct FieldCapture(std::sync::Mutex<Vec<(String, String)>>);

    impl FieldCapture {
        fn visitor(&self) -> impl tracing::field::Visit + '_ {
            struct V<'a>(&'a FieldCapture);
            impl tracing::field::Visit for V<'_> {
                fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                    self.0
                        .0
                        .lock()
                        .unwrap()
                        .push((field.name().to_string(), format!("{value:?}")));
                }
            }
            V(self)
        }
    }

    struct CapturingSubscriber(std::sync::Arc<FieldCapture>);

    impl tracing::Subscriber for CapturingSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            span.record(&mut self.0.visitor());
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
            values.record(&mut self.0.visitor());
        }
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, _event: &tracing::Event<'_>) {}
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    /// Regression test: `CrawlEngine::scrape`'s `crawl.engine.scrape` span must record
    /// the admitted `url.full`, without the caller's userinfo, whether or not the fetch succeeds.
    // ~keep Serial with the sequential-crawl tests in `engine::wasm_crawl`: this assertion
    // ~keep reads a thread-local capturing subscriber, and `tracing` rebuilds its global
    // ~keep callsite-interest cache whenever a dispatcher is installed. Heavy concurrent span
    // ~keep traffic from another test races that rebuild and the `crawl.engine.scrape`
    // ~keep callsite intermittently reports no fields at all.
    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn scrape_span_records_redacted_url() {
        let captured = std::sync::Arc::new(FieldCapture::default());
        let subscriber = CapturingSubscriber(captured.clone());
        let engine = CrawlEngine::builder().build().expect("engine build must not fail");

        let _guard = tracing::subscriber::set_default(subscriber);
        let _ = engine.scrape("http://user:hunter2@127.0.0.1:1/").await;
        drop(_guard);

        let fields = captured.0.lock().unwrap();
        let (_, value) = fields
            .iter()
            .find(|(name, _)| name == "url.full")
            .unwrap_or_else(|| panic!("expected a url.full span field to be recorded, got {fields:?}"));
        assert!(
            !value.contains("hunter2"),
            "redacted url.full must not leak the password, got {value}"
        );
        assert!(
            value.contains("127.0.0.1"),
            "redacted url.full should retain the host, got {value}"
        );
    }

    /// Verify that a connection-refused error propagates with [network:connection] tag
    /// rather than being swallowed by browser fallback. The engine's BrowserMode::Auto
    /// arm must not include Connection errors.
    #[tokio::test]
    async fn connection_refused_propagates_network_tag() {
        use crate::error::classify_reqwest_error;
        use std::time::Duration;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .expect("client build must not fail");

        let raw_err = client
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("expected connection error");

        let err = classify_reqwest_error(raw_err);
        let msg = err.to_string();
        assert!(
            msg.contains("[network:connection]"),
            "expected [network:connection] in '{msg}'"
        );
        assert!(
            matches!(err, CrawlError::Connection { .. }),
            "expected CrawlError::Connection, got {err:?}"
        );
    }

    /// Verify that a DNS resolution failure propagates with [network:dns] tag.
    #[tokio::test]
    async fn dns_failure_propagates_network_tag() {
        use crate::error::classify_reqwest_error;
        use std::time::Duration;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(1000))
            .build()
            .expect("client build must not fail");

        let raw_err = client
            .get("http://this-host-does-not-exist-crawlberg-engine-test.invalid/")
            .send()
            .await
            .expect_err("expected dns error");

        let err = classify_reqwest_error(raw_err);
        let msg = err.to_string();
        assert!(msg.contains("[network:dns]"), "expected [network:dns] in '{msg}'");
        assert!(
            matches!(err, CrawlError::Dns { .. }),
            "expected CrawlError::Dns, got {err:?}"
        );
    }
}
