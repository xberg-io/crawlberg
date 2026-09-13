//! Regression coverage for task #33: a crawl of a URL carrying `user:pass@` userinfo, or
//! discovering a link that carries it, must never leak the raw credential into a `tracing`
//! span. Spans are shipped to logs/OTLP by default, unlike an error that might be swallowed.
//!
//! Exercises the real `CrawlEngine::crawl` entry point end to end (not just the redaction
//! helper in isolation) so the assertions cover the actual wiring in
//! `tower/tracing_layer.rs` (`crawl.page.fetch`) and `engine/crawl_loop.rs`
//! (`crawl.page.discover`) at once.

use std::sync::{Arc, Mutex};

use crawlberg::{CrawlConfig, CrawlEngine};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RAW_PASSWORD: &str = "hunter2";

/// `Visit` that records every field name/value pair, formatted with `Debug` (which is how
/// tracing dispatches both `%value` and plain `Display`/`Debug` fields).
struct FieldVisitor<'a>(&'a mut Vec<(String, String)>);

impl Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }
}

/// Minimal `tracing::Subscriber` that captures every span's and event's fields into
/// `sink`, so this test can assert on exact recorded values without a `tracing-subscriber`
/// dev-dependency.
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

    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
        event.record(&mut FieldVisitor(&mut fields));
    }

    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read (`SsrfPolicy::from_env`, reached from
// `CrawlConfig::default()`) in this binary's other tests, aborting the process on glibc
// with no failing test name.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

#[tokio::test]
async fn crawl_never_records_raw_credentials_into_a_span() {
    // ~keep #[tokio::test] defaults to a current-thread runtime, so the whole crawl
    // (including every `.await`) stays on the thread the subscriber guard was set on.
    let mock = MockServer::start().await;
    let authority = mock.uri().trim_start_matches("http://").to_owned();
    let seed_url = format!("http://user:{RAW_PASSWORD}@{authority}/");
    let next_url = format!("http://user:{RAW_PASSWORD}@{authority}/next");

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!(r#"<html><body><a href="{next_url}">next</a></body></html>"#))
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/next"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>leaf page</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let engine = CrawlEngine::builder()
        .config(allow_private_config())
        .build()
        .expect("engine must build");

    let sink: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(CapturingSubscriber { sink: sink.clone() });

    let result = engine.crawl(&seed_url).await.expect("crawl must succeed");
    assert!(
        result.pages.len() >= 2,
        "expected the crawl to reach both the seed and the discovered page, got {} pages",
        result.pages.len()
    );

    let recorded = sink.lock().expect("sink mutex must not be poisoned");
    assert!(
        !recorded.is_empty(),
        "expected the capturing subscriber to observe at least one span field"
    );

    // ~keep Scoped to the fields `tower/tracing_layer.rs` (`url.full` on `crawl.page.fetch`)
    // and `engine/crawl_loop.rs` (`url.full`/`crawl.parent_url` on `crawl.page.discover`)
    // are responsible for redacting. `engine/mod.rs`'s unrelated `crawlberg::dispatch` event
    // records a plain `url` field that is out of scope here.
    const REDACTED_FIELD_NAMES: [&str; 2] = ["url.full", "crawl.parent_url"];
    let redacted_fields: Vec<&(String, String)> = recorded
        .iter()
        .filter(|(name, _)| REDACTED_FIELD_NAMES.contains(&name.as_str()))
        .collect();
    assert!(
        !redacted_fields.is_empty(),
        "expected at least one of {REDACTED_FIELD_NAMES:?} to be recorded, got {recorded:?}"
    );

    let leaking: Vec<&&(String, String)> = redacted_fields
        .iter()
        .filter(|(_, value)| value.contains(RAW_PASSWORD))
        .collect();
    assert!(
        leaking.is_empty(),
        "no {REDACTED_FIELD_NAMES:?} field may contain the raw password '{RAW_PASSWORD}', but found: {leaking:?}"
    );

    let url_full_values: Vec<&str> = redacted_fields
        .iter()
        .filter(|(name, _)| name == "url.full")
        .map(|(_, value)| value.as_str())
        .collect();
    assert!(
        !url_full_values.is_empty(),
        "expected at least one 'url.full' span field to be recorded, got {recorded:?}"
    );
    assert!(
        url_full_values.iter().any(|v| v.contains("***:***@")),
        "expected a redacted 'url.full' value (***:***@) among {url_full_values:?}"
    );
}
