//! `crawl_waf_blocks_total` counts a 403 a browser rendered with a WAF fingerprint once, as the
//! HTTP fetch counts the one it refuses, and a plain rendered 403 not at all.
//!
//! ~keep This file is its own test binary with a single test on purpose. The counter is a
//! process-wide OTel instrument that binds to the global meter provider the first time the
//! metric registry is touched, so the in-memory provider must be installed before any fetch in
//! the process, and no other test may fetch concurrently or the deltas read here would include
//! its blocks.
//!
//! Requires a real Chrome binary for the Chromiumoxide backend; skipped (not failed) when
//! Chrome is unavailable, matching the other browser tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

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

#[tokio::test]
async fn a_rendered_waf_403_counts_one_block_and_a_plain_rendered_403_none() {
    let test_name = "a_rendered_waf_403_counts_one_block_and_a_plain_rendered_403_none";
    let counter = BlockCounter::install();
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/waf"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-datadome", "protected")
                .set_body_raw("<html><body>blocked</body></html>", "text/html"),
        )
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/plain"))
        .respond_with(ResponseTemplate::new(403).set_body_raw("<html><body>no entry</body></html>", "text/html"))
        .mount(&site)
        .await;
    let mut config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    config.retry_count = 0;
    let engine = create_engine(Some(config)).expect("create_engine with the test config");

    let before = counter.total();
    let waf = scrape(&engine, &format!("{}/waf", site.uri())).await;
    let waf_delta = counter.total() - before;
    if let Err(CrawlError::BrowserError { message, .. }) = &waf
        && is_missing_chrome_message(message)
    {
        announce_chrome_skip(test_name, message);
        return;
    }
    assert!(
        matches!(&waf, Err(CrawlError::WafBlocked { vendor, .. }) if vendor == "datadome"),
        "the rendered DataDome 403 must be refused as a WAF block, got {waf:?}"
    );
    assert_eq!(waf_delta, 1, "a rendered WAF 403 must count exactly one WAF block");

    let before = counter.total();
    let plain = scrape(&engine, &format!("{}/plain", site.uri())).await;
    let plain_delta = counter.total() - before;
    assert!(
        matches!(&plain, Err(CrawlError::Forbidden { .. })),
        "the plain rendered 403 must be a forbidden error, got {plain:?}"
    );
    assert_eq!(plain_delta, 0, "a plain rendered 403 must not count as a WAF block");

    let requests = site.received_requests().await.expect("request recording is on");
    let waf_requests = requests.iter().filter(|request| request.url.path() == "/waf").count();
    assert_eq!(waf_requests, 1, "the rendered WAF 403 must be fetched once");
}
