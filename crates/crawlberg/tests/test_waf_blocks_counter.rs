//! `crawl_waf_blocks_total` counts responses refused as a WAF block, once each.
//!
//! ~keep This file is its own test binary with a single test on purpose. The counter is a
//! process-wide OTel instrument that binds to the global meter provider the first time the
//! metric registry is touched, so the in-memory provider must be installed before any fetch in
//! the process, and no other test may fetch concurrently or the deltas read here would include
//! its blocks.

use crawlberg::{BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};
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

/// Serve `template` at `/` on a fresh server, scrape it once, and return the result together
/// with the counter delta and the number of requests the server received.
async fn scrape_and_count(
    counter: &BlockCounter,
    template: ResponseTemplate,
) -> (Result<crawlberg::ScrapeResult, CrawlError>, u64, usize) {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(template)
        .mount(&mock)
        .await;
    let handle = create_engine(Some(config())).expect("create_engine with the test config");

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
}
