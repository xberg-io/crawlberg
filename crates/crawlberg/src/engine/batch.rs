//! Batch and streaming crawl operations.
#![allow(dead_code)]

use std::future::Future;
#[cfg(any(test, feature = "browser"))]
use std::sync::Arc;

use tokio_stream::wrappers::ReceiverStream;
use tracing::Instrument as _;

use crate::error::CrawlError;
use crate::telemetry::attributes::{CRAWL_SEED_COUNT, URL_FULL};
use crate::types::*;

use super::admission::admission_key;
use super::{CrawlEngine, SeedUrl};

/// Default concurrency limit when `max_concurrent` is not set.
pub(super) const DEFAULT_MAX_CONCURRENT: usize = 10;

/// Channel buffer multiplier relative to max_concurrent for streaming operations.
const STREAM_BUFFER_MULTIPLIER: usize = 16;

impl CrawlEngine {
    /// Return a clone of this engine whose frontier is isolated to a single crawl call.
    ///
    /// `CrawlEngine` is designed to be constructed once and reused for many `crawl()`
    /// calls, and `CrawlEngine::clone()` shares the same `Arc<dyn Frontier>` across all
    /// clones. Without per-call isolation, a frontier's `seen` state (e.g. the default
    /// `InMemoryFrontier`) would leak across calls on a reused engine and race under
    /// concurrent `batch_crawl` calls sharing the same `seen` set.
    fn with_isolated_frontier(&self) -> Self {
        let mut engine = self.clone();
        if let Some(fresh) = self.frontier.isolated() {
            engine.frontier = fresh;
        }
        engine
    }

    /// Return a clone of this engine for a single crawl call: its frontier is isolated, and
    /// with `cookies_enabled` its Chrome pages share cookies that no other crawl call sees.
    /// Every entry point that invokes `crawl_with_sender` must route through this helper.
    pub(super) fn for_one_crawl(&self) -> Self {
        let engine = self.with_isolated_frontier();
        #[cfg(feature = "browser")]
        let engine = Self {
            crawl_cookies: self.config.cookies_enabled.then(Arc::default),
            ..engine
        };
        engine
    }

    /// Crawl a website starting from `url`, following links up to the configured depth.
    ///
    /// Uses the engine's [`CrawlStrategy`](crate::traits::CrawlStrategy) and
    /// [`Frontier`](crate::traits::Frontier) traits to control URL selection order
    /// and deduplication.
    pub async fn crawl(&self, url: &str) -> Result<CrawlResult, CrawlError> {
        let (engine, seed) = self.admit(url)?;
        engine.crawl_seed(&seed).await
    }

    /// Crawl an admitted seed URL. See [`CrawlEngine::crawl`].
    #[tracing::instrument(name = "crawl.engine.crawl", skip_all, fields(url.full = %seed))]
    async fn crawl_seed(&self, seed: &SeedUrl) -> Result<CrawlResult, CrawlError> {
        self.for_one_crawl().crawl_with_sender(seed, None).await
    }

    /// Crawl a website and return a stream of events as pages are processed.
    ///
    /// Uses the engine's trait implementations (strategy, frontier, etc.) for the crawl.
    pub fn crawl_stream(&self, url: &str) -> ReceiverStream<CrawlEvent> {
        let admitted = self.admit(url).map(|(engine, seed)| (engine.for_one_crawl(), seed));
        let span = match &admitted {
            Ok((_, seed)) => tracing::info_span!("crawl.engine.crawl_stream", { URL_FULL } = %seed),
            Err(_) => tracing::info_span!("crawl.engine.crawl_stream"),
        };
        let error_url = admission_key(url, admitted.as_ref().ok().map(|(_, seed)| seed));
        let event_sink = self.event_sink.clone();

        // ~keep The fallback must match `DEFAULT_MAX_CONCURRENT`, which is what the crawl loop
        // actually uses when `max_concurrent` is unset. A hardcoded 4 here sized this channel for a
        // concurrency the engine never runs at, and disagreed with the batch variant below.
        let channel_size = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT) * STREAM_BUFFER_MULTIPLIER;
        let (tx, rx) = tokio::sync::mpsc::channel(channel_size);

        tokio::spawn(
            async move {
                let outcome = match admitted {
                    Ok((engine, seed)) => engine.crawl_with_sender(&seed, Some(tx.clone())).await.map(|_| ()),
                    Err(e) => Err(e),
                };
                if let Err(e) = outcome {
                    let error_event = CrawlEvent::Error {
                        url: error_url,
                        error: e.to_string(),
                    };
                    let _ = tx.send(error_event.clone()).await;
                    if let Some(ref sink) = event_sink {
                        sink.emit(error_event).await;
                    }
                    let complete_event = CrawlEvent::Complete { pages_crawled: 0 };
                    let _ = tx.send(complete_event.clone()).await;
                    if let Some(ref sink) = event_sink {
                        sink.emit(complete_event).await;
                    }
                }
            }
            .instrument(span),
        );

        ReceiverStream::new(rx)
    }

    /// Admit each of `urls` and run `operation(engine, seed)` for it with bounded concurrency,
    /// pairing every result with its admitted URL — including when a spawned task panics.
    ///
    /// A URL that admission refuses is reported with its refusal and never runs.
    ///
    async fn run_batch<T, F, Fut>(&self, urls: &[&str], operation: F) -> Vec<(String, Result<T, CrawlError>)>
    where
        F: Fn(CrawlEngine, SeedUrl) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, CrawlError>> + Send + 'static,
        T: Send + 'static,
    {
        use super::continuation::run_resumable_jobs;
        use super::host_turn::{HostJob, HostStep};
        use futures::FutureExt;
        let mut refused = Vec::new();
        // ~keep Admission is pulled by the scheduler, one URL when a slot is free: the first
        // operation starts after the first admission, not after the last one. The closure
        // takes an index, not `&&str`: a closure over a reference inside a spawned future
        // trips "implementation of `FnOnce` is not general enough".
        let jobs = (0..urls.len()).filter_map(|index| match self.admit(urls[index]) {
            Ok((engine, seed)) => Some(HostJob {
                host: super::redirect::url_host(seed.as_str()),
                data: (engine, seed),
            }),
            Err(error) => {
                refused.push((admission_key(urls[index], None), Err(error)));
                None
            }
        });
        let completed = run_resumable_jobs(
            jobs,
            self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT),
            self.scheduling_limiter(),
            |(_, seed)| seed.as_str().to_owned(),
            move |(engine, seed), routing| {
                let engine = routing.bind_engine(engine);
                let url = seed.as_str().to_owned();
                let future = operation(engine, seed);
                async move {
                    match std::panic::AssertUnwindSafe(future).catch_unwind().await {
                        Ok(result) => result.map(HostStep::Done),
                        Err(_) => Err(CrawlError::other(format!("task panicked while processing {url}"))),
                    }
                }
            },
        )
        .await;
        refused_first(refused, completed)
    }

    /// Scrape multiple URLs concurrently.
    ///
    /// Unlike the standalone `batch::batch_scrape`, this method routes each URL
    /// through the engine's middleware chain, rate limiter, and cache.
    #[tracing::instrument(name = "crawl.engine.batch_scrape", skip(self, urls), fields(url_count = urls.len()))]
    pub async fn batch_scrape(&self, urls: &[&str]) -> Vec<(String, Result<ScrapeResult, CrawlError>)> {
        if self.config.capture_screenshot
            && self.config.browser.backend == BrowserBackend::Chromiumoxide
            && matches!(self.config.browser.mode, BrowserMode::Always | BrowserMode::Stealth)
        {
            return self
                .run_batch(urls, |engine, seed| async move { engine.scrape_seed(&seed).await })
                .await;
        }
        use super::continuation::run_resumable_jobs;
        use super::host_turn::{HostJob, HostStep};
        use super::redirect::{Hop, RedirectCursor, RedirectResolution, RedirectStep, url_host};
        let mut refused = Vec::new();
        // ~keep Admission is pulled by the scheduler, one URL when a slot is free: the first
        // fetch starts after the first admission, not after the last one. The closure takes
        // an index, not `&&str`: see `run_batch`.
        let jobs = (0..urls.len()).filter_map(|index| match self.admit(urls[index]) {
            Ok((engine, seed)) => {
                let span = tracing::info_span!("crawl.engine.scrape", { URL_FULL } = %seed);
                if let Err(error) = span.in_scope(|| engine.scrape_preflight()) {
                    refused.push((seed.as_str().to_owned(), Err(error)));
                    return None;
                }
                Some(HostJob {
                    host: url_host(seed.as_str()),
                    data: (
                        engine,
                        RedirectCursor::new(seed.as_str(), self.config.max_redirects),
                        span,
                    ),
                })
            }
            Err(error) => {
                refused.push((admission_key(urls[index], None), Err(error)));
                None
            }
        });
        let completed = run_resumable_jobs(
            jobs,
            self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT),
            self.scheduling_limiter(),
            |(_, cursor, _)| cursor.url().to_owned(),
            |(engine, cursor, span), routing| {
                let owned_span = span.clone();
                let hop_engine = routing.bind_engine(engine.clone());
                async move {
                    #[cfg(feature = "browser-native")]
                    let hop = if matches!(engine.config.browser.mode, BrowserMode::Always | BrowserMode::Stealth)
                        && engine.config.browser.backend == BrowserBackend::Native
                    {
                        Hop::NativeRender
                    } else {
                        Hop::Fetch
                    };
                    #[cfg(not(feature = "browser-native"))]
                    let hop = Hop::Fetch;
                    match cursor.step(&hop_engine, None, None, hop).await? {
                        RedirectStep::Continue(cursor) => Ok(HostStep::Continue(HostJob {
                            host: url_host(cursor.url()),
                            data: (engine, cursor, span),
                        })),
                        RedirectStep::Done(RedirectResolution::Fetched(outcome)) => {
                            Ok(HostStep::Done(hop_engine.scrape_redirect_outcome(*outcome).await?))
                        }
                        RedirectStep::Done(RedirectResolution::Refused { refusal, .. }) => Err(refusal.into_error()),
                    }
                }
                .instrument(owned_span)
            },
        )
        .await;
        refused_first(refused, completed)
    }

    /// Crawl multiple seed URLs, each following links to configured depth.
    /// Returns results paired with seed URLs as they complete.
    pub async fn batch_crawl(&self, urls: &[&str]) -> Vec<(String, Result<CrawlResult, CrawlError>)> {
        let seed_count = urls.len();
        let span = tracing::info_span!("crawl.engine.batch", { CRAWL_SEED_COUNT } = seed_count as i64,);

        self.batch_crawl_inner(urls).instrument(span).await
    }

    async fn batch_crawl_inner(&self, urls: &[&str]) -> Vec<(String, Result<CrawlResult, CrawlError>)> {
        self.run_scheduled_crawls(urls, None).await
    }

    /// Crawl multiple seed URLs and stream events from all crawls.
    ///
    /// Dropping the stream stops the batch: no further seed starts, and a seed that never
    /// started sends nothing to the event emitter or the event sink. A seed already running
    /// at the drop still reports its own completion.
    pub fn batch_crawl_stream(&self, urls: &[&str]) -> ReceiverStream<CrawlEvent> {
        let span = tracing::info_span!("crawl.engine.batch_crawl_stream", url_count = urls.len());
        let urls: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
        let engine = self.clone();
        let channel_size = self.config.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT) * STREAM_BUFFER_MULTIPLIER;
        let (tx, rx) = tokio::sync::mpsc::channel(channel_size);

        tokio::spawn(
            async move {
                let urls = urls.iter().map(String::as_str).collect::<Vec<_>>();
                engine.run_scheduled_crawls(&urls, Some(tx)).await;
            }
            .instrument(span),
        );

        ReceiverStream::new(rx)
    }
}

/// The results of a batch: the URLs that admission refused first, then the completed
/// operations in completion order. Without a refusal the completed list is returned as is:
/// a result is about 2 KB, and a batch of thousands should not copy its results once more.
fn refused_first<T>(
    mut refused: Vec<(String, Result<T, CrawlError>)>,
    completed: Vec<(String, Result<T, CrawlError>)>,
) -> Vec<(String, Result<T, CrawlError>)> {
    if refused.is_empty() {
        return completed;
    }
    refused.extend(completed);
    refused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(feature = "browser", feature = "browser-native"))]
    fn configured_test_browser(mode: BrowserMode) -> CrawlConfig {
        let mut config = CrawlConfig::default();
        config.browser.mode = mode;
        #[cfg(all(feature = "browser-native", not(feature = "browser")))]
        {
            config.browser.backend = BrowserBackend::Native;
        }
        config
    }

    #[cfg(any(feature = "browser", feature = "browser-native"))]
    #[derive(Clone, Default)]
    struct BrowserAdmissionCount(Arc<std::sync::atomic::AtomicUsize>);

    #[cfg(any(feature = "browser", feature = "browser-native"))]
    #[async_trait::async_trait]
    impl crate::traits::RateLimiter for BrowserAdmissionCount {
        async fn acquire(&self, _: &str) -> Result<(), CrawlError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
            Ok(())
        }

        async fn set_crawl_delay(&self, _: &str, _: std::time::Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    #[cfg(any(feature = "browser", feature = "browser-native"))]
    #[tokio::test(start_paused = true)]
    async fn should_keep_default_browser_batch_turns_free_of_the_http_delay() {
        let mut config = configured_test_browser(BrowserMode::Always);
        config.max_concurrent = Some(1);
        let engine = CrawlEngine::builder().config(config).build().expect("engine");
        let start = tokio::time::Instant::now();
        let results = engine
            .run_batch(
                &["https://example.com/first", "https://example.com/second"],
                |engine, _| async move { engine.browser_rate_limiter.acquire("example.com").await },
            )
            .await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        assert_eq!(start.elapsed(), std::time::Duration::ZERO);
    }

    #[cfg(any(feature = "browser", feature = "browser-native"))]
    #[tokio::test]
    async fn should_acquire_an_installed_browser_batch_limiter_once_per_operation() {
        let config = configured_test_browser(BrowserMode::Always);
        let count = BrowserAdmissionCount::default();
        let engine = CrawlEngine::builder()
            .config(config)
            .rate_limiter(count.clone())
            .build()
            .expect("engine");
        let results = engine
            .run_batch(&["https://example.com/"], |engine, _| async move {
                engine.browser_rate_limiter.acquire("example.com").await
            })
            .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_ok());
        assert_eq!(count.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(any(feature = "browser", feature = "browser-native"))]
    #[derive(Default)]
    struct FallbackAdmission {
        slow_calls: std::sync::atomic::AtomicUsize,
        release: tokio::sync::Notify,
        pending: tokio::sync::Notify,
    }

    #[cfg(any(feature = "browser", feature = "browser-native"))]
    #[async_trait::async_trait]
    impl crate::traits::RateLimiter for Arc<FallbackAdmission> {
        async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
            if host == "slow.example" && self.slow_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                self.pending.notify_one();
                self.release.notified().await;
            }
            Ok(())
        }

        async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
            Ok(())
        }

        async fn set_crawl_delay(&self, _: &str, _: std::time::Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    #[cfg(any(feature = "browser", feature = "browser-native"))]
    #[tokio::test]
    async fn should_release_batch_capacity_when_an_automatic_browser_fallback_waits_for_its_turn() {
        let limiter = Arc::new(FallbackAdmission::default());
        let free_done = Arc::new(tokio::sync::Notify::new());
        let mut config = configured_test_browser(BrowserMode::Auto);
        config.max_concurrent = Some(1);
        let engine = CrawlEngine::builder()
            .config(config)
            .rate_limiter(limiter.clone())
            .build()
            .expect("engine");
        let free = free_done.clone();
        let running = tokio::spawn(async move {
            engine
                .run_batch(
                    &["https://slow.example/", "https://free.example/"],
                    move |engine, seed| {
                        let free = free.clone();
                        async move {
                            let host = super::super::redirect::url_host(seed.as_str());
                            engine.rate_limiter.acquire(&host).await?;
                            if host == "slow.example" {
                                engine.browser_rate_limiter.acquire(&host).await?;
                            } else {
                                free.notify_one();
                            }
                            Ok(())
                        }
                    },
                )
                .await
        });
        limiter.pending.notified().await;
        let progressed = tokio::time::timeout(std::time::Duration::from_secs(1), free_done.notified()).await;
        limiter.release.notify_waiters();
        let results = running.await.expect("batch joins");
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        assert!(
            progressed.is_ok(),
            "a parked browser turn must leave room for the ready host"
        );
    }

    #[derive(Clone, Default)]
    struct WarningCapture(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for WarningCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct ErrorPanickingSink;

    #[async_trait::async_trait]
    impl crate::sink::EventSink for ErrorPanickingSink {
        async fn emit(&self, event: CrawlEvent) {
            if matches!(event, CrawlEvent::Error { .. }) {
                panic!("custom error sink panic");
            }
        }
    }

    #[tokio::test]
    async fn should_preserve_healthy_stream_seeds_when_an_error_sink_panics() {
        use tokio_stream::StreamExt;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let site = MockServer::start().await;
        Mock::given(wiremock::matchers::path("/healthy"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>healthy</p>", "text/html"))
            .mount(&site)
            .await;
        let mut config = CrawlConfig::builder().allow_private_networks(true).build();
        config.respect_robots_txt = false;
        config.max_concurrent = Some(2);
        let engine = CrawlEngine::builder()
            .config(config)
            .event_sink(ErrorPanickingSink)
            .build()
            .unwrap();
        let url = format!("{}/healthy", site.uri());
        let events = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            engine.batch_crawl_stream(&["invalid url", &url]).collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        assert_eq!(site.received_requests().await.unwrap().len(), 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, CrawlEvent::Page { result } if result.url == url))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, CrawlEvent::Complete { pages_crawled: 1 }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, CrawlEvent::Error { url, .. } if url == "(unparseable URL)"))
                .count(),
            1
        );
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn should_preserve_one_initial_url_span_per_logical_batch_operation() {
        use tokio_stream::StreamExt;
        use tracing::instrument::WithSubscriber;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let site = MockServer::start().await;
        Mock::given(wiremock::matchers::path("/source"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{}/target", site.uri())))
            .mount(&site)
            .await;
        Mock::given(wiremock::matchers::path("/target"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
            .mount(&site)
            .await;
        let mut config = CrawlConfig::builder().allow_private_networks(true).build();
        config.respect_robots_txt = false;
        let engine = CrawlEngine::builder().config(config).build().unwrap();
        let captured = Arc::new(super::super::tests::FieldCapture::default());
        let subscriber = super::super::tests::CapturingSubscriber(captured.clone());
        let source = format!("{}/source", site.uri());
        async {
            assert!(engine.scrape(&source).await.is_ok());
            // ~keep Async peer subscribers can change global callsite interest during this local capture.
            tracing::callsite::rebuild_interest_cache();
            let batch = engine.batch_scrape(&[&source]).await;
            assert!(batch[0].1.is_ok());
            assert_eq!(
                captured.values("crawl.engine.scrape", "url.full"),
                [source.clone(), source.clone()]
            );
            assert!(engine.crawl(&source).await.is_ok());
            tracing::callsite::rebuild_interest_cache();
            let batch = engine.batch_crawl(&[&source]).await;
            assert!(batch[0].1.is_ok());
            assert_eq!(
                captured.values("crawl.engine.crawl", "url.full"),
                [source.clone(), source.clone()]
            );
            let streamed = engine.batch_crawl_stream(&[&source]).collect::<Vec<_>>().await;
            assert_eq!(
                streamed
                    .iter()
                    .filter(|event| matches!(event, CrawlEvent::Complete { .. }))
                    .count(),
                1
            );
            assert_eq!(
                captured.values("crawl.engine.crawl", "url.full"),
                [source.clone(), source.clone()]
            );
        }
        .with_subscriber(subscriber)
        .await;
        assert_eq!(site.received_requests().await.unwrap().len(), 10);
    }

    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn should_preserve_one_screenshot_warning_per_batch_scrape_across_redirects() {
        use tracing::instrument::WithSubscriber;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let site = MockServer::start().await;
        Mock::given(wiremock::matchers::path("/source"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{}/target", site.uri())))
            .mount(&site)
            .await;
        Mock::given(wiremock::matchers::path("/target"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
            .mount(&site)
            .await;
        let mut config = CrawlConfig::builder().allow_private_networks(true).build();
        config.capture_screenshot = true;
        config.respect_robots_txt = false;
        let engine = CrawlEngine::builder().config(config).build().unwrap();
        let logs = WarningCapture::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let url = format!("{}/source", site.uri());
        async {
            assert!(engine.scrape(&url).await.is_ok());
            let batch = engine.batch_scrape(&[&url]).await;
            assert_eq!(batch.len(), 1);
            assert!(batch[0].1.is_ok());
        }
        .with_subscriber(subscriber)
        .await;
        let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            logs.matches("capture_screenshot has no effect for this configuration")
                .count(),
            2
        );
        assert_eq!(site.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn should_validate_batch_scrape_configuration_before_any_wire_request() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let site = MockServer::start().await;
        Mock::given(wiremock::matchers::method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
            .mount(&site)
            .await;
        let engine = CrawlEngine::builder()
            .config(CrawlConfig {
                retry_count: 21,
                max_concurrent: Some(2),
                ssrf_deny_private_explicit: Some(false),
                respect_robots_txt: false,
                ..CrawlConfig::default()
            })
            .build()
            .unwrap();
        let url = format!("{}/page", site.uri());
        let baseline = engine.scrape(&url).await.unwrap_err();
        assert!(matches!(baseline, CrawlError::InvalidConfig { .. }));
        assert_eq!(site.received_requests().await.unwrap().len(), 0);
        let results = engine.batch_scrape(&[&url]).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, url);
        assert_eq!(site.received_requests().await.unwrap().len(), 0);
        assert!(matches!(&results[0].1, Err(CrawlError::InvalidConfig { .. })));
        assert_eq!(results[0].1.as_ref().unwrap_err().to_string(), baseline.to_string());
    }

    #[tokio::test(start_paused = true)]
    async fn should_admit_another_host_before_the_second_page_of_a_delayed_host() {
        const HOST_DELAY: std::time::Duration = std::time::Duration::from_secs(60);
        let engine = CrawlEngine::builder()
            .config(CrawlConfig {
                max_concurrent: Some(2),
                ..CrawlConfig::default()
            })
            .rate_limiter(crate::defaults::PerDomainThrottle::new(HOST_DELAY))
            .build()
            .expect("engine");
        let requested = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&requested);
        let started = tokio::time::Instant::now();
        let results = engine
            .run_batch(
                &[
                    "https://slow.example/1",
                    "https://slow.example/2",
                    "https://slow.example/3",
                    "https://slow.example/4",
                    "https://other.example/",
                ],
                move |engine, seed| {
                    let requested = record.clone();
                    async move {
                        let domain = url::Url::parse(seed.as_str()).unwrap().host_str().unwrap().to_owned();
                        engine.rate_limiter.acquire(&domain).await?;
                        requested
                            .lock()
                            .unwrap()
                            .push((seed.as_str().to_owned(), started.elapsed()));
                        Ok::<_, CrawlError>(())
                    }
                },
            )
            .await;
        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        let requested = requested.lock().unwrap();
        let other = requested
            .iter()
            .position(|(url, _)| url == "https://other.example/")
            .unwrap();
        let second = requested
            .iter()
            .position(|(url, _)| url == "https://slow.example/2")
            .unwrap();
        assert!(
            other < second,
            "other host waited behind the delayed host: {requested:?}"
        );
    }

    #[derive(Clone, Default)]
    struct AdmissionCount(Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait::async_trait]
    impl crate::traits::RateLimiter for AdmissionCount {
        async fn acquire(&self, _: &str) -> Result<(), CrawlError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
            Ok(())
        }

        async fn set_crawl_delay(&self, _: &str, _: std::time::Duration) -> Result<(), CrawlError> {
            Ok(())
        }
    }

    /// The scheduler admits a URL when a slot is free, not every URL before the first
    /// operation: a batch of 2,000 hosts started its first fetch after 2,000 admissions.
    #[tokio::test]
    async fn should_admit_batch_urls_as_slots_free_not_all_before_the_first_operation() {
        let admissions = AdmissionCount::default();
        let engine = CrawlEngine::builder()
            .config(CrawlConfig {
                max_concurrent: Some(1),
                ..CrawlConfig::default()
            })
            .rate_limiter(admissions.clone())
            .build()
            .expect("engine");
        let first_started = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let started = first_started.clone();
        let release = release_first.clone();
        let running = tokio::spawn(async move {
            engine
                .run_batch(
                    &["https://a.example/", "https://b.example/", "https://c.example/"],
                    move |_, seed| {
                        let started = started.clone();
                        let release = release.clone();
                        async move {
                            if seed.as_str() == "https://a.example/" {
                                started.notify_one();
                                release.notified().await;
                            }
                            Ok(())
                        }
                    },
                )
                .await
        });
        first_started.notified().await;
        assert_eq!(
            admissions.0.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the other URLs must wait for a free slot before their host is admitted"
        );
        release_first.notify_one();
        let results = running.await.expect("batch joins");
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        assert_eq!(admissions.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn should_park_later_host_turns_inside_an_opaque_batch_operation() {
        let limiter = Arc::new(crate::defaults::PerDomainThrottle::new(std::time::Duration::from_secs(
            60,
        )));
        crate::traits::RateLimiter::acquire(limiter.as_ref(), "target.example")
            .await
            .unwrap();
        let mut engine = CrawlEngine::builder()
            .config(CrawlConfig {
                max_concurrent: Some(2),
                ..CrawlConfig::default()
            })
            .build()
            .unwrap();
        engine.rate_limiter = limiter;
        let requested = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&requested);
        let results = engine
            .run_batch(
                &[
                    "https://source1.example/",
                    "https://source2.example/",
                    "https://free.example/",
                ],
                move |engine, seed| {
                    let record = Arc::clone(&record);
                    async move {
                        let host = url::Url::parse(seed.as_str()).unwrap().host_str().unwrap().to_owned();
                        engine.rate_limiter.acquire(&host).await?;
                        let mut preserved = vec![host.clone()];
                        if host != "free.example" {
                            engine.rate_limiter.acquire("target.example").await?;
                            preserved.push("target.example".to_owned());
                        }
                        record.lock().unwrap().push(host);
                        Ok::<_, CrawlError>(preserved)
                    }
                },
            )
            .await;
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        assert_eq!(requested.lock().unwrap()[0], "free.example");
        for (url, result) in results {
            let values = result.unwrap();
            assert_eq!(values[0], url::Url::parse(&url).unwrap().host_str().unwrap());
            assert_eq!(values.len(), if url == "https://free.example/" { 1 } else { 2 });
        }
    }

    /// Regression test: a panicking task must not lose the URL it was processing.
    /// Previously, `join_set.join_next()` returning `Err(JoinError)` on panic pushed
    /// `(String::new(), Err(...))`, discarding the URL entirely.
    #[tokio::test]
    async fn run_batch_preserves_url_when_task_panics() {
        let engine = CrawlEngine::builder().build().expect("engine build must not fail");

        let results = engine
            .run_batch(
                &["https://ok.example/", "https://panics.example/"],
                |_engine, seed| async move {
                    if seed.as_str() == "https://panics.example/" {
                        panic!("simulated task panic");
                    }
                    Ok::<u32, CrawlError>(42)
                },
            )
            .await;

        assert_eq!(results.len(), 2, "expected one result per URL, got {results:?}");

        let ok_result = results
            .iter()
            .find(|(url, _)| url == "https://ok.example/")
            .expect("ok URL must be present in results");
        assert_eq!(ok_result.1.as_ref().copied().ok(), Some(42));

        let panic_result = results
            .iter()
            .find(|(url, _)| url == "https://panics.example/")
            .unwrap_or_else(|| panic!("panicking task must still report its URL, got: {results:?}"));
        let error_msg = panic_result
            .1
            .as_ref()
            .expect_err("panicking task must return Err")
            .to_string();
        assert!(
            error_msg.contains("https://panics.example/"),
            "panic error must mention the URL that was being processed, got: {error_msg}"
        );
    }
}

#[cfg(test)]
mod scheduling_regression_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    #[serial_test::serial(engine_tracing_callsites)]
    async fn should_preserve_the_callers_trace_scope_when_dispatching_a_ready_batch_operation() {
        use tracing::Instrument;
        use tracing::instrument::WithSubscriber;
        let engine = CrawlEngine::builder().build().unwrap();
        let results = async {
            tracing::callsite::rebuild_interest_cache();
            let parent = tracing::info_span!("batch_operation_parent");
            assert!(!parent.is_disabled(), "the trace scope control must be active");
            engine
                .run_batch(&["https://one.example/"], |_, _| async {
                    Ok::<_, CrawlError>(
                        tracing::Span::current()
                            .metadata()
                            .map(|metadata| metadata.name().to_owned()),
                    )
                })
                .instrument(parent)
                .await
        }
        .with_subscriber(tracing_subscriber::registry())
        .await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].1.as_ref().unwrap().as_deref(),
            Some("batch_operation_parent")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn should_run_ready_batch_operations_on_multiple_runtime_workers() {
        let engine = CrawlEngine::builder().build().unwrap();
        let live = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let peak = maximum.clone();
        let results = engine
            .run_batch(&["https://one.example/", "https://two.example/"], move |_, _| {
                let live = live.clone();
                let maximum = maximum.clone();
                async move {
                    let active = live.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(active, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, CrawlError>(42)
                }
            })
            .await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(_, result)| result.as_ref().unwrap() == &42));
        assert_eq!(
            peak.load(Ordering::SeqCst),
            2,
            "ready CPU work must not serialize on the batch coordinator"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn should_bound_engine_batch_operations_by_configured_concurrency() {
        const CONCURRENCY: usize = 2;
        const URLS: usize = 7;
        let config = CrawlConfig::builder().max_concurrent(CONCURRENCY).build();
        let engine = CrawlEngine::builder().config(config).build().unwrap();
        let live = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let peak = maximum.clone();
        let remaining = live.clone();
        let urls = (0..URLS)
            .map(|host| format!("https://host{host}.example/"))
            .collect::<Vec<_>>();
        let refs = urls.iter().map(String::as_str).collect::<Vec<_>>();
        let results = engine
            .run_batch(&refs, move |_, _| {
                let live = live.clone();
                let maximum = maximum.clone();
                async move {
                    let active = live.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(active, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, CrawlError>(42)
                }
            })
            .await;
        assert_eq!(results.len(), URLS);
        assert!(results.iter().all(|(_, result)| result.as_ref().unwrap() == &42));
        assert_eq!(peak.load(Ordering::SeqCst), CONCURRENCY);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }
}
