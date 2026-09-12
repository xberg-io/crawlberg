//! Regression coverage: the `crawl.pages_completed` field on the `crawl.loop.iteration` span
//! must report real progress during a STREAMING crawl.
//!
//! A streaming crawl moves every page into a `CrawlEvent` and never pushes to
//! `CrawlState::pages`, so the span's original `state.pages.len()` read was permanently `0`
//! for every iteration of every streaming crawl, while the two budget checks and the stats
//! block beside it branched on `is_streaming` and were correct. Span field values are a
//! product surface here, so a silently-zero counter is a defect, not cosmetics.
//!
//! Mirrors the capturing-subscriber approach in `test_crawl_span_credential_redaction.rs`.

use std::sync::{Arc, Mutex};

use crawlberg::{CrawlConfig, CrawlEngine, CrawlEvent};
use tokio_stream::StreamExt;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PAGES_COMPLETED_FIELD: &str = "crawl.pages_completed";

struct FieldVisitor<'a>(&'a mut Vec<(String, String)>);

impl Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.push((field.name().to_owned(), value.to_string()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.push((field.name().to_owned(), value.to_string()));
    }
}

struct CapturingSubscriber {
    sink: Arc<Mutex<Vec<(String, String)>>>,
}

impl tracing::Subscriber for CapturingSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
        attrs.record(&mut FieldVisitor(&mut fields));
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, values: &Record<'_>) {
        let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
        values.record(&mut FieldVisitor(&mut fields));
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
        event.record(&mut FieldVisitor(&mut fields));
    }

    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

/// Mirrors `test_escalation.rs::allow_private_network` so wiremock's loopback server is
/// reachable past the SSRF policy.
fn allow_private_network() {
    static ALLOW_PRIVATE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

/// Serve a seed linking to three leaves, so the crawl runs several loop iterations and the
/// span is recorded more than once.
async fn mount_site(mock: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><body>
                       <a href="/a">a</a><a href="/b">b</a><a href="/c">c</a>
                       </body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;

    for leaf in ["/a", "/b", "/c"] {
        Mock::given(method("GET"))
            .and(path(leaf))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>leaf</body></html>")
                    .append_header("content-type", "text/html"),
            )
            .mount(mock)
            .await;
    }
}

fn max_recorded_pages_completed(recorded: &[(String, String)]) -> i64 {
    recorded
        .iter()
        .filter(|(name, _)| name == PAGES_COMPLETED_FIELD)
        .filter_map(|(_, value)| value.trim().parse::<i64>().ok())
        .max()
        .unwrap_or(-1)
}

#[tokio::test]
async fn should_report_real_page_progress_in_the_span_when_streaming() {
    // ~keep #[tokio::test] is a current-thread runtime, so the crawl stays on the thread the
    // subscriber guard was installed on.
    allow_private_network();

    let mock = MockServer::start().await;
    mount_site(&mock).await;

    let engine = CrawlEngine::builder()
        .config(CrawlConfig::default())
        .build()
        .expect("engine must build");

    let sink: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(CapturingSubscriber { sink: sink.clone() });

    let mut pages_streamed = 0_usize;
    let mut stream = engine.crawl_stream(&mock.uri());
    while let Some(event) = stream.next().await {
        if matches!(event, CrawlEvent::Page { .. }) {
            pages_streamed += 1;
        }
    }

    assert!(
        pages_streamed >= 2,
        "the streaming crawl must emit the seed and at least one leaf, got {pages_streamed}"
    );

    let recorded = sink.lock().expect("sink mutex must not be poisoned");
    let observed = max_recorded_pages_completed(&recorded);

    assert_ne!(
        observed, -1,
        "expected the `{PAGES_COMPLETED_FIELD}` span field to be recorded at least once"
    );
    // ~keep The bug: this field read `state.pages.len()`, which a streaming crawl never fills,
    // so it was pinned at 0 no matter how many pages were emitted.
    assert!(
        observed > 0,
        "`{PAGES_COMPLETED_FIELD}` must track streamed pages, but the highest value recorded was \
         {observed} across a crawl that streamed {pages_streamed} pages"
    );
}

#[tokio::test]
async fn should_still_report_page_progress_in_the_span_when_not_streaming() {
    // ~keep Pins the non-streaming half of the same accessor, so a future refactor cannot fix
    // streaming by breaking the buffered path.
    allow_private_network();

    let mock = MockServer::start().await;
    mount_site(&mock).await;

    let engine = CrawlEngine::builder()
        .config(CrawlConfig::default())
        .build()
        .expect("engine must build");

    let sink: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(CapturingSubscriber { sink: sink.clone() });

    let result = engine.crawl(&mock.uri()).await.expect("crawl must succeed");
    assert!(
        result.pages.len() >= 2,
        "expected the buffered crawl to return the seed and at least one leaf, got {}",
        result.pages.len()
    );

    let recorded = sink.lock().expect("sink mutex must not be poisoned");
    let observed = max_recorded_pages_completed(&recorded);
    assert!(
        observed > 0,
        "`{PAGES_COMPLETED_FIELD}` must track buffered pages, but the highest value recorded was \
         {observed} across a crawl that returned {} pages",
        result.pages.len()
    );
}
