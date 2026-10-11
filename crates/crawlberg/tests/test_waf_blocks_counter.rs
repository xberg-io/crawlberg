//! `crawl_waf_blocks_total` counts responses refused as a WAF block, once each.
//!
//! ~keep This file is its own test binary with a single test on purpose. The counter is a
//! process-wide OTel instrument that binds to the global meter provider the first time the
//! metric registry is touched, so the in-memory provider must be installed before any fetch in
//! the process, and no other test may fetch concurrently or the deltas read here would include
//! its blocks.

use std::sync::Arc;

use async_trait::async_trait;
use crawlberg::{
    AttemptOutcome, BrowserMode, BypassProvider, BypassResponse, CrawlConfig, CrawlError, DefaultAntibotStrategy,
    DispatchProfile, EscalationReason, EscalationStrategy, RetryDirective, RetryPolicy, TomlClassifier, create_engine,
    scrape,
};
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The cumulative value of `crawl_waf_blocks_total`, summed over every vendor.
struct BlockCounter {
    provider: SdkMeterProvider,
    exporter: InMemoryMetricExporter,
}

impl BlockCounter {
    fn install() -> Self {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        opentelemetry::global::set_meter_provider(provider.clone());
        Self { provider, exporter }
    }

    fn total(&self) -> u64 {
        self.provider.force_flush().expect("flush the in-memory meter provider");
        let exports = self.exporter.get_finished_metrics().expect("read the exported metrics");
        let Some(latest) = exports.last() else {
            return 0;
        };
        latest
            .scope_metrics()
            .flat_map(|scope| scope.metrics())
            .filter(|metric| metric.name() == "crawl_waf_blocks_total")
            .map(|metric| match metric.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                    sum.data_points().map(|point| point.value()).sum::<u64>()
                }
                other => panic!("crawl_waf_blocks_total must be a u64 sum, got {other:?}"),
            })
            .sum()
    }
}

fn config() -> CrawlConfig {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.browser.mode = BrowserMode::Never;
    config.retry_count = 2;
    config.retry_codes = vec![429, 503];
    config.retry_initial_delay_ms = 1;
    config.retry_max_delay_ms = 5;
    config
}

/// The engine with the built-in classifier and antibot strategy set as dispatch hooks.
fn hook_config() -> CrawlConfig {
    let mut config = config();
    config.dispatch = Some(DispatchProfile {
        waf_classifier: Some(Arc::new(TomlClassifier::builtin())),
        antibot_strategy: Some(Arc::new(DefaultAntibotStrategy::new())),
        ..DispatchProfile::default()
    });
    config
}

/// A retry policy that refuses every successful response as a WAF block.
#[derive(Debug)]
struct RefuseEverySuccess;

#[async_trait]
impl RetryPolicy for RefuseEverySuccess {
    async fn decide(&self, outcome: &AttemptOutcome) -> RetryDirective {
        if outcome.error.is_some() {
            return RetryDirective::Stop;
        }
        RetryDirective::Escalate {
            reason: EscalationReason::WafBlocked {
                vendor: "policy".to_owned(),
            },
        }
    }

    fn name(&self) -> &'static str {
        "refuse-every-success"
    }
}

/// A retry policy that retries the first successful response once, then accepts.
#[derive(Debug)]
struct RetryFirstSuccess;

#[async_trait]
impl RetryPolicy for RetryFirstSuccess {
    async fn decide(&self, outcome: &AttemptOutcome) -> RetryDirective {
        if outcome.error.is_none() && outcome.attempt == 0 {
            return RetryDirective::Retry { backoff_ms: 1 };
        }
        RetryDirective::Stop
    }

    fn name(&self) -> &'static str {
        "retry-first-success"
    }
}

/// A bypass tier that answers every URL with a fixed page.
#[derive(Debug)]
struct FixedBypass;

#[async_trait]
impl BypassProvider for FixedBypass {
    async fn fetch(&self, _url: &str) -> Result<BypassResponse, CrawlError> {
        let body = "<html><body>bypass</body></html>";
        Ok(BypassResponse {
            status: 200,
            content_type: "text/html".to_owned(),
            body: body.to_owned(),
            body_bytes: body.as_bytes().to_vec(),
            body_kind: crawlberg::BypassBody::Bytes,
            headers: Default::default(),
            final_url: String::new(),
            cost_usd: Some(0.0),
            vendor_request_id: None,
        })
    }

    fn vendor_name(&self) -> &'static str {
        "fixed"
    }
}

/// Serve `template` at `/` on a fresh server, scrape it once, and return the result together
/// with the counter delta and the number of requests the server received.
async fn scrape_and_count(
    counter: &BlockCounter,
    template: ResponseTemplate,
) -> (Result<crawlberg::ScrapeResult, CrawlError>, u64, usize) {
    scrape_and_count_with(counter, config(), template).await
}

/// [`scrape_and_count`] with an engine built from `engine_config`.
async fn scrape_and_count_with(
    counter: &BlockCounter,
    engine_config: CrawlConfig,
    template: ResponseTemplate,
) -> (Result<crawlberg::ScrapeResult, CrawlError>, u64, usize) {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(template)
        .mount(&mock)
        .await;
    let handle = create_engine(Some(engine_config)).expect("create_engine with the test config");

    let before = counter.total();
    let result = scrape(&handle, &mock.uri()).await;
    let delta = counter.total() - before;
    let requests = mock.received_requests().await.expect("request recording is on").len();
    (result, delta, requests)
}

#[tokio::test]
async fn the_waf_block_counter_counts_each_refused_response_once_and_nothing_else() {
    let counter = BlockCounter::install();

    // (a) A CDN-presence 200 with an ordinary body is returned as content, so it is no block.
    let (result, delta, _) = scrape_and_count(
        &counter,
        ResponseTemplate::new(200)
            .set_body_string("<html><head><title>Blog</title></head><body><h1>Release notes</h1></body></html>")
            .append_header("content-type", "text/html")
            .append_header("x-sucuri-id", "18012"),
    )
    .await;
    assert!(
        result.is_ok(),
        "the sucuri 200 must be returned as content, got {result:?}"
    );
    assert_eq!(delta, 0, "a 200 returned as content must not count as a WAF block");

    // (b) A DataDome 200 interstitial is refused: one response, one block.
    let (result, delta, requests) = scrape_and_count(
        &counter,
        ResponseTemplate::new(200)
            .set_body_string("<html><script src=\"https://js.datadome.co/tags.js\"></script></html>")
            .append_header("content-type", "text/html")
            .append_header("x-datadome", "protected"),
    )
    .await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { ref vendor, .. }) if vendor == "datadome"),
        "the DataDome 200 must be refused as a WAF block, got {result:?}"
    );
    assert_eq!(requests, 1, "the refused 200 must be fetched once");
    assert_eq!(delta, 1, "one refused 2xx response must count exactly one WAF block");

    // (b) A Cloudflare 403 challenge is refused on the challenge-status path: one block.
    let (result, delta, requests) = scrape_and_count(
        &counter,
        ResponseTemplate::new(403)
            .set_body_string("<html><title>Just a moment...</title><div id=\"cf-browser-verification\"></div></html>")
            .append_header("content-type", "text/html")
            .append_header("server", "cloudflare"),
    )
    .await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { ref vendor, .. }) if vendor == "cloudflare"),
        "the Cloudflare 403 must be refused as a WAF block, got {result:?}"
    );
    assert_eq!(requests, 1, "the refused 403 must be fetched once");
    assert_eq!(
        delta, 1,
        "one refused challenge response must count exactly one WAF block"
    );

    // (c) A 429 or 503 whose only WAF evidence is CDN presence is retried, never blocked.
    for status in [429_u16, 503] {
        let (result, delta, requests) = scrape_and_count(
            &counter,
            ResponseTemplate::new(status)
                .set_body_string("<html><body><h1>Try again later</h1></body></html>")
                .append_header("content-type", "text/html")
                .append_header("server", "AkamaiGHost"),
        )
        .await;
        assert!(
            !matches!(result, Err(CrawlError::WafBlocked { .. })),
            "a {status} behind a CDN must not be a WAF block, got {result:?}"
        );
        assert_eq!(
            requests, 3,
            "a {status} must be retried: retry_count=2 sends 3 requests"
        );
        assert_eq!(delta, 0, "a retried {status} must not count as a WAF block");
    }

    // (d) With the classifier and antibot strategy set as engine hooks, a CDN-presence 200 is
    // still content, and still no block.
    let (result, delta, _) = scrape_and_count_with(
        &counter,
        hook_config(),
        ResponseTemplate::new(200)
            .set_body_string("<html><head><title>Blog</title></head><body><h1>Release notes</h1></body></html>")
            .append_header("content-type", "text/html")
            .append_header("x-sucuri-id", "18012"),
    )
    .await;
    assert!(
        result.is_ok(),
        "the sucuri 200 must be returned as content with engine hooks set, got {result:?}"
    );
    assert_eq!(
        delta, 0,
        "a 200 returned as content must not count, with engine hooks set"
    );

    // (e) A response the fetch path refuses is counted there, and the hooks never see it: one block.
    let (result, delta, requests) = scrape_and_count_with(
        &counter,
        hook_config(),
        ResponseTemplate::new(200)
            .set_body_string("<html><script src=\"https://js.datadome.co/tags.js\"></script></html>")
            .append_header("content-type", "text/html")
            .append_header("x-datadome", "protected"),
    )
    .await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { ref vendor, .. }) if vendor == "datadome"),
        "the DataDome 200 must be refused by the fetch path, got {result:?}"
    );
    assert_eq!(requests, 1, "the refused 200 must be fetched once");
    assert_eq!(
        delta, 1,
        "a response refused by the fetch path must count once, not again in the engine"
    );

    // (f) A response the fetch path returns but the engine's antibot strategy refuses: one block.
    let (result, delta, requests) = scrape_and_count_with(
        &counter,
        hook_config(),
        ResponseTemplate::new(418)
            .set_body_string("<html><script src=\"https://js.datadome.co/tags.js\"></script></html>")
            .append_header("content-type", "text/html")
            .append_header("x-datadome", "protected"),
    )
    .await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { ref vendor, .. }) if vendor == "antibot"),
        "the antibot strategy must refuse the DataDome 418, got {result:?}"
    );
    assert_eq!(requests, 1, "the refused 418 must be fetched once");
    assert_eq!(
        delta, 1,
        "a response the antibot strategy refuses must count exactly one WAF block"
    );

    // (g) A successful response a retry policy refuses as a WAF block: one block.
    let mut policy_config = config();
    policy_config.dispatch = Some(DispatchProfile {
        retry_policy: Some(Arc::new(RefuseEverySuccess)),
        ..DispatchProfile::default()
    });
    let (result, delta, requests) = scrape_and_count_with(
        &counter,
        policy_config,
        ResponseTemplate::new(200)
            .set_body_string("<html><body><h1>Release notes</h1></body></html>")
            .append_header("content-type", "text/html"),
    )
    .await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { ref vendor, .. }) if vendor == "policy"),
        "the retry policy must refuse the 200 as a WAF block, got {result:?}"
    );
    assert_eq!(requests, 1, "the refused 200 must be fetched once");
    assert_eq!(
        delta, 1,
        "a response the retry policy refuses must count exactly one WAF block"
    );

    // (h) A response the retry policy refuses, then hands back as content when the attempt cap
    // stops the escalation, is not a block: the caller gets the page.
    let mut capped_config = config();
    capped_config.dispatch = Some(DispatchProfile {
        retry_policy: Some(Arc::new(RefuseEverySuccess)),
        bypass: Some(Arc::new(FixedBypass)),
        strategy: EscalationStrategy::BypassOnly,
        max_total_attempts: 1,
        ..DispatchProfile::default()
    });
    let (result, delta, requests) = scrape_and_count_with(
        &counter,
        capped_config,
        ResponseTemplate::new(200)
            .set_body_string("<html><body><h1>Release notes</h1></body></html>")
            .append_header("content-type", "text/html"),
    )
    .await;
    let page = result.expect("the attempt cap must hand back the refused page as content");
    assert!(
        page.html.contains("Release notes"),
        "the caller must get the origin's page, got {}",
        page.html
    );
    assert_eq!(requests, 1, "the page must be fetched once");
    assert_eq!(
        delta, 0,
        "a refused response the attempt cap returns as content must not count as a WAF block"
    );

    // (i) Without the cap, the same refusal stands once the loop moves on: the origin's page and
    // the bypass tier's page are both refused, so two responses count two blocks.
    let mut escalating_config = config();
    escalating_config.dispatch = Some(DispatchProfile {
        retry_policy: Some(Arc::new(RefuseEverySuccess)),
        bypass: Some(Arc::new(FixedBypass)),
        strategy: EscalationStrategy::BypassOnly,
        ..DispatchProfile::default()
    });
    let (result, delta, requests) = scrape_and_count_with(
        &counter,
        escalating_config,
        ResponseTemplate::new(200)
            .set_body_string("<html><body><h1>Release notes</h1></body></html>")
            .append_header("content-type", "text/html"),
    )
    .await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { ref vendor, .. }) if vendor == "policy"),
        "the retry policy must refuse the bypass tier's page too, got {result:?}"
    );
    assert_eq!(requests, 1, "the origin must be fetched once");
    assert_eq!(
        delta, 2,
        "the refused origin page and the refused bypass page must count one block each"
    );

    // (j) A successful response a retry policy retries is not refused: the retried response and
    // the one the caller gets count nothing.
    let mut retry_config = config();
    retry_config.dispatch = Some(DispatchProfile {
        retry_policy: Some(Arc::new(RetryFirstSuccess)),
        ..DispatchProfile::default()
    });
    let (result, delta, requests) = scrape_and_count_with(
        &counter,
        retry_config,
        ResponseTemplate::new(200)
            .set_body_string("<html><body><h1>Release notes</h1></body></html>")
            .append_header("content-type", "text/html"),
    )
    .await;
    assert!(result.is_ok(), "the retried 200 must be returned, got {result:?}");
    assert_eq!(requests, 2, "the retry policy must fetch the page twice");
    assert_eq!(delta, 0, "a retried response must not count as a WAF block");
}
