//! Warnings the browser path logs, counted by a process-wide tracing subscriber: one per request
//! the SSRF policy refuses, up to a bound per page, then one that reports the count; and none for
//! a Chrome an `interact` session launched, which it closes rather than leaving to be killed when
//! the last reference to it drops (chromiumoxide warns "Browser was not closed manually" then).
//!
//! The tests run one at a time: a browser another test drops would count against this one.
//!
//! Requires a real Chrome binary; skipped (not failed) when Chrome is unavailable.

#![cfg(feature = "browser")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, HostMatcher, PageAction, create_engine,
    interact, scrape,
};
use tokio_stream::StreamExt;
use wiremock::matchers::{any, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

const NOT_CLOSED: &str = "Browser was not closed manually";
const REFUSED: &str = "the SSRF policy refused a request the page sent";
const REFUSED_MORE: &str = "the SSRF policy refused more requests the page sent; only the first were logged";

/// How many refusals of one page are logged one by one; the crate's bound.
const LOGGED_REFUSALS: usize = 5;

/// What the subscriber counted: browsers dropped while running, the URL of every refusal logged
/// one by one, and the count each summary reported.
struct Counter {
    not_closed: AtomicUsize,
    refused: Mutex<Vec<String>>,
    summaries: Mutex<Vec<usize>>,
}

/// The one counter of this process, installed as the global subscriber on first use.
fn counter() -> &'static Counter {
    static COUNTER: OnceLock<&'static Counter> = OnceLock::new();
    COUNTER.get_or_init(|| {
        let counter: &'static Counter = Box::leak(Box::new(Counter {
            not_closed: AtomicUsize::new(0),
            refused: Mutex::new(Vec::new()),
            summaries: Mutex::new(Vec::new()),
        }));
        tracing::subscriber::set_global_default(Subscriber(counter))
            .expect("the counting subscriber must be the only one");
        counter
    })
}

struct Subscriber(&'static Counter);

#[derive(Default)]
struct Fields {
    message: String,
    url: String,
    refused: String,
}

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "url" => self.url = format!("{value:?}"),
            "refused" => self.refused = format!("{value:?}"),
            _ => {}
        }
    }
}

impl tracing::Subscriber for Subscriber {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        if fields.message.contains(NOT_CLOSED) {
            self.0.not_closed.fetch_add(1, Ordering::SeqCst);
        }
        if fields.message.contains(REFUSED) {
            self.0.refused.lock().expect("refused lock").push(fields.url);
        }
        if fields.message.contains(REFUSED_MORE) {
            let count = fields.refused.parse().expect("the summary names a count");
            self.0.summaries.lock().expect("summaries lock").push(count);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn chrome_config(config: CrawlConfig) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..config
    }
}

async fn site(body: &str) -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html"))
        .mount(&site)
        .await;
    site
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn interact_closes_the_browser_it_launched() {
    let test_name = "interact_closes_the_browser_it_launched";
    let counter = counter();

    // ~keep Positive control: the counter sees the warning for a browser dropped while running.
    let dir = std::env::temp_dir().join(format!("crawlberg-{test_name}-{}", std::process::id()));
    let launched = match chromiumoxide::browser::BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(&dir)
        .build()
    {
        Ok(config) => chromiumoxide::Browser::launch(config).await.map_err(|e| e.to_string()),
        Err(error) => Err(error),
    };
    let (browser, mut handler) = match launched {
        Ok(launched) => launched,
        Err(error) => {
            announce_chrome_skip(test_name, &error);
            return;
        }
    };
    let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
    drop(browser);
    handler.abort();
    // ~keep A browser an earlier test of this binary let go can warn late; wait that out.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let seen_by_control = counter.not_closed.swap(0, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(&dir);

    let site = site("<p>start</p>").await;
    let config = chrome_config(CrawlConfig::builder().allow_private_networks(true).build());
    let engine = create_engine(Some(config)).expect("engine must build");
    let actions = vec![PageAction::ExecuteJs {
        script: "return 1".to_owned(),
    }];
    match interact(&engine, &site.uri(), actions).await {
        Ok(_) => {}
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: interact must succeed: {error:?}"),
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        seen_by_control >= 1,
        "{test_name}: the counter must see a browser dropped while running"
    );
    assert_eq!(
        counter.not_closed.load(Ordering::SeqCst),
        0,
        "{test_name}: interact must close the browser it launched, not drop it running"
    );
}

/// A request the SSRF policy refuses is logged as a warning naming its address.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn scrape_logs_a_warning_for_a_refused_request() {
    let test_name = "scrape_logs_a_warning_for_a_refused_request";
    let counter = counter();
    let denied = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&denied)
        .await;
    let refused = format!("http://127.0.0.1:{}/logged", denied.address().port());
    let site = site(&format!("<p>start</p><img src={refused:?}>")).await;
    let seed = format!("http://localhost:{}/", site.address().port());
    let config = chrome_config(
        CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .build(),
    );
    let engine = create_engine(Some(config)).expect("engine must build");
    match scrape(&engine, &seed).await {
        Ok(_) => {}
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: scrape must succeed: {error:?}"),
    }
    let logged = counter.refused.lock().expect("refused lock").clone();
    assert!(
        logged.iter().any(|url| url == &refused),
        "{test_name}: the refusal of {refused} must be logged, got {logged:?}"
    );
}

/// The native backend logs a refusal the same way.
#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn native_scrape_logs_a_warning_for_a_refused_request() {
    let test_name = "native_scrape_logs_a_warning_for_a_refused_request";
    let counter = counter();
    let denied = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&denied)
        .await;
    let refused = format!("http://127.0.0.1:{}/native.js", denied.address().port());
    let site = site(&format!("<p>start</p><script src={refused:?}></script>")).await;
    let seed = format!("http://localhost:{}/", site.address().port());
    let mut config = chrome_config(
        CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .build(),
    );
    config.browser.backend = BrowserBackend::Native;
    let engine = create_engine(Some(config)).expect("engine must build");
    scrape(&engine, &seed)
        .await
        .unwrap_or_else(|error| panic!("{test_name}: scrape must succeed: {error:?}"));
    let logged = counter.refused.lock().expect("refused lock").clone();
    assert!(
        logged.iter().any(|url| url == &refused),
        "{test_name}: the refusal of {refused} must be logged, got {logged:?}"
    );
}

/// Assert the flood logged `LOGGED_REFUSALS` warnings one by one and one summary that counts
/// every refusal, given the counts before the scrape and the addresses the result lists.
fn assert_bounded(test_name: &str, counter: &Counter, logged_before: usize, summaries_before: usize, listed: usize) {
    let logged = counter.refused.lock().expect("refused lock").len() - logged_before;
    let summaries = counter.summaries.lock().expect("summaries lock")[summaries_before..].to_vec();
    assert!(
        listed > LOGGED_REFUSALS,
        "{test_name}: the page must send more refused requests than are logged, listed {listed}"
    );
    assert_eq!(
        logged, LOGGED_REFUSALS,
        "{test_name}: only the first refusals are logged one by one"
    );
    assert_eq!(
        summaries,
        [listed],
        "{test_name}: one summary must report every refusal"
    );
}

/// A page that sends many refused requests logs the first few, then one warning with the count.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn scrape_logs_the_first_refusals_and_then_one_count() {
    let test_name = "scrape_logs_the_first_refusals_and_then_one_count";
    let counter = counter();
    let denied = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&denied)
        .await;
    let refused = format!("http://127.0.0.1:{}/flood", denied.address().port());
    let site = site(&format!(
        "<p>start</p><script>for (let i = 0; i < 12; i++) fetch({refused:?} + '?' + i, {{ mode: 'no-cors' }}).catch(() => {{}});</script>"
    ))
    .await;
    let seed = format!("http://localhost:{}/", site.address().port());
    let mut config = chrome_config(
        CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .build(),
    );
    config.browser.extra_wait = Some(Duration::from_millis(500));
    let engine = create_engine(Some(config)).expect("engine must build");
    let logged_before = counter.refused.lock().expect("refused lock").len();
    let summaries_before = counter.summaries.lock().expect("summaries lock").len();
    let result = match scrape(&engine, &seed).await {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: scrape must succeed: {error:?}"),
    };
    assert_bounded(
        test_name,
        counter,
        logged_before,
        summaries_before,
        result.ssrf_refused_urls.len(),
    );
}

/// The native backend bounds the warnings the same way.
#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn native_scrape_logs_the_first_refusals_and_then_one_count() {
    let test_name = "native_scrape_logs_the_first_refusals_and_then_one_count";
    let counter = counter();
    let denied = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&denied)
        .await;
    let scripts: String = (0..12)
        .map(|i| {
            format!(
                "<script src=\"http://127.0.0.1:{}/flood{i}.js\"></script>",
                denied.address().port()
            )
        })
        .collect();
    let site = site(&format!("<p>start</p>{scripts}")).await;
    let seed = format!("http://localhost:{}/", site.address().port());
    let mut config = chrome_config(
        CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .build(),
    );
    config.browser.backend = BrowserBackend::Native;
    let engine = create_engine(Some(config)).expect("engine must build");
    let logged_before = counter.refused.lock().expect("refused lock").len();
    let summaries_before = counter.summaries.lock().expect("summaries lock").len();
    let result = scrape(&engine, &seed)
        .await
        .unwrap_or_else(|error| panic!("{test_name}: scrape must succeed: {error:?}"));
    assert_bounded(
        test_name,
        counter,
        logged_before,
        summaries_before,
        result.ssrf_refused_urls.len(),
    );
}
