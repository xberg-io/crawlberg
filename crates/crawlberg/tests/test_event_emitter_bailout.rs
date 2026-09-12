//! An `EventEmitter` consumer must hear about a crawl that stops before its loop begins.
//!
//! `EventSink` and `EventEmitter` are separate traits on separate engine fields, and
//! `sink.emit()` does not reach an emitter. Every pre-loop bail-out — seed network failure,
//! seed HTTP error, robots.txt unreachable, seed disallowed — reported through the stream and
//! the sink but called neither `on_error` nor `on_complete`, so a callback-driven consumer saw
//! no failure *and* no completion: a failed crawl looked like a hung one.
//!
//! Nothing else in the repo exercises `EventEmitter`, so the recording emitter below is the
//! first coverage of `CrawlEngineBuilder::event_emitter` at all.

use std::sync::{Arc, Mutex};

use crawlberg::traits::{CompleteEvent, ErrorEvent, EventEmitter, PageEvent};
use crawlberg::{CrawlConfig, CrawlEngine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Every callback the engine made, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Callback {
    Page { url: String },
    Error { url: String, error: String },
    Complete { pages_crawled: usize },
    Discovered { url: String },
}

/// ~keep Shared through an `Arc` because `event_emitter()` takes the emitter by value and
/// ~keep wraps it itself, so a test cannot hold the emitter to read it back afterwards.
#[derive(Debug, Clone, Default)]
struct RecordingEmitter {
    calls: Arc<Mutex<Vec<Callback>>>,
}

impl RecordingEmitter {
    fn calls(&self) -> Vec<Callback> {
        self.calls.lock().expect("callback log must not be poisoned").clone()
    }

    fn errors(&self) -> Vec<(String, String)> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Callback::Error { url, error } => Some((url, error)),
                _ => None,
            })
            .collect()
    }

    fn completions(&self) -> Vec<usize> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Callback::Complete { pages_crawled } => Some(pages_crawled),
                _ => None,
            })
            .collect()
    }

    fn push(&self, call: Callback) {
        self.calls.lock().expect("callback log must not be poisoned").push(call);
    }
}

#[async_trait::async_trait]
impl EventEmitter for RecordingEmitter {
    async fn on_page(&self, event: &PageEvent) {
        self.push(Callback::Page { url: event.url.clone() });
    }

    async fn on_error(&self, event: &ErrorEvent) {
        self.push(Callback::Error {
            url: event.url.clone(),
            error: event.error.clone(),
        });
    }

    async fn on_complete(&self, event: &CompleteEvent) {
        self.push(Callback::Complete {
            pages_crawled: event.pages_crawled,
        });
    }

    async fn on_discovered(&self, url: &str, _depth: usize) {
        self.push(Callback::Discovered { url: url.to_owned() });
    }
}

fn config(respect_robots: bool) -> CrawlConfig {
    CrawlConfig::builder()
        .respect_robots_txt(respect_robots)
        .allow_private_networks(true)
        .max_pages(5)
        .build()
}

async fn crawl_recording(config: CrawlConfig, seed: &str) -> RecordingEmitter {
    let emitter = RecordingEmitter::default();
    let engine = CrawlEngine::builder()
        .config(config)
        .event_emitter(emitter.clone())
        .build()
        .expect("engine builds");
    let _ = engine.crawl(seed).await;
    emitter
}

#[tokio::test]
async fn should_report_an_unreachable_robots_txt_to_the_event_emitter() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>seed</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let emitter = crawl_recording(config(true), &mock.uri()).await;

    let errors = emitter.errors();
    assert_eq!(
        errors.len(),
        1,
        "a fail-closed crawl must report exactly one error to the emitter, got {errors:?}"
    );
    assert!(
        errors[0].1.contains("robots_unreachable"),
        "the emitter must receive the fail-closed reason, got {:?}",
        errors[0].1
    );
    assert_eq!(
        emitter.completions(),
        vec![0],
        "the emitter must also see the crawl complete with no pages, or a failure is \
         indistinguishable from a hang; got {:?}",
        emitter.calls()
    );
}

#[tokio::test]
async fn should_report_a_seed_its_robots_txt_disallows_to_the_event_emitter() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /\n"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>seed</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let emitter = crawl_recording(config(true), &mock.uri()).await;

    let errors = emitter.errors();
    assert_eq!(
        errors.len(),
        1,
        "a disallowed seed must reach the emitter, got {:?}",
        emitter.calls()
    );
    assert!(
        errors[0].1.contains("disallows"),
        "the reason must say the seed was disallowed, got {:?}",
        errors[0].1
    );
    assert_eq!(emitter.completions(), vec![0]);
}

#[tokio::test]
async fn should_still_report_a_successful_crawl_to_the_event_emitter() {
    // ~keep Guards the happy path: the bail-out fix must not double-report or displace the
    // ~keep completion the normal path already emits.
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>seed</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let emitter = crawl_recording(config(true), &mock.uri()).await;

    assert!(
        emitter.errors().is_empty(),
        "a successful crawl reports no error, got {:?}",
        emitter.errors()
    );
    assert_eq!(
        emitter.completions().len(),
        1,
        "exactly one completion, got {:?}",
        emitter.calls()
    );
    assert_eq!(
        emitter.completions()[0],
        1,
        "the completion must count the page that was crawled"
    );
}
