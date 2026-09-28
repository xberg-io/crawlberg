//! A caller's address with a credential and no host never reaches a tracing field.
//!
//! `user:hunter2@evil.example/path` parses as scheme `user` with no host, so it carries no
//! userinfo to strip. Each entry point is driven with such addresses, and every span field and
//! event field is recorded, so a raw seed in an engine span or a dispatch event is caught.

use std::sync::{Arc, Mutex};

use crawlberg::{CrawlConfig, CrawlEngine, CrawlEvent, PageAction};
use tokio_stream::StreamExt;

/// Every span and event field recorded while the subscriber is the default.
#[derive(Clone, Default)]
struct FieldCapture(Arc<Mutex<Vec<String>>>);

struct FieldVisitor<'a>(&'a mut Vec<String>, String);

impl tracing::field::Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{} {}={value:?}", self.1, field.name()));
    }
}

impl FieldCapture {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().expect("capture mutex must not be poisoned"))
    }
}

impl tracing::Subscriber for FieldCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = self.0.lock().expect("capture mutex must not be poisoned");
        let tag = format!("span {}", attrs.metadata().name());
        attrs.record(&mut FieldVisitor(&mut fields, tag));
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        let mut fields = self.0.lock().expect("capture mutex must not be poisoned");
        values.record(&mut FieldVisitor(&mut fields, "span-record".to_owned()));
    }

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = self.0.lock().expect("capture mutex must not be poisoned");
        let tag = format!("event {}", event.metadata().target());
        event.record(&mut FieldVisitor(&mut fields, tag));
    }

    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// The #399 addresses and the secret text each one must never show.
const HOSTLESS_ROWS: [(&str, &[&str]); 6] = [
    ("user:hunter2@evil.example/path", &["hunter2"]),
    ("user:hunter2@evil.example:8080/path", &["hunter2"]),
    ("user:hunt%40er2@evil.example/path", &["hunt%40er2", "er2@"]),
    ("user:hunt/er2@evil.example/path", &["hunt/er2"]),
    ("user:hunt#er2@evil.example/path", &["hunt#er2", "er2@"]),
    ("user:hunt?er2@evil.example/path", &["hunt?er2", "er2@"]),
];

const ENTRY_POINTS: [&str; 7] = [
    "scrape",
    "crawl",
    "map",
    "crawl_stream",
    "interact",
    "batch_scrape",
    "batch_crawl",
];

/// Drive `address` through `entry_point` and return everything the caller gets back.
async fn run(engine: &CrawlEngine, entry_point: &str, address: &str) -> String {
    match entry_point {
        "scrape" => format!("{:?}", engine.scrape(address).await.map(|page| page.final_url)),
        "crawl" => format!("{:?}", engine.crawl(address).await.map(|result| result.error)),
        "map" => format!("{:?}", engine.map(address).await.map(|result| result.urls.len())),
        "crawl_stream" => {
            let events: Vec<CrawlEvent> = engine.crawl_stream(address).collect().await;
            format!("{events:?}")
        }
        "interact" => format!(
            "{:?}",
            engine.interact(address, &[PageAction::Scrape]).await.map(|_| ())
        ),
        "batch_scrape" => {
            let results = engine.batch_scrape(&[address]).await;
            format!(
                "{:?}",
                results
                    .iter()
                    .map(|(key, r)| (key, r.as_ref().err()))
                    .collect::<Vec<_>>()
            )
        }
        "batch_crawl" => {
            let results = engine.batch_crawl(&[address]).await;
            format!(
                "{:?}",
                results
                    .iter()
                    .map(|(key, r)| (key, r.as_ref().err()))
                    .collect::<Vec<_>>()
            )
        }
        other => unreachable!("unknown entry point {other}"),
    }
}

#[tokio::test]
async fn a_hostless_address_with_a_credential_never_reaches_a_tracing_field() {
    let capture = FieldCapture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let engine = CrawlEngine::builder()
        .config(CrawlConfig::builder().allow_private_networks(false).build())
        .build()
        .expect("engine builds");

    let mut leaks = Vec::new();
    let mut runs = 0usize;
    for (address, secrets) in HOSTLESS_ROWS {
        for entry_point in ENTRY_POINTS {
            let returned = run(&engine, entry_point, address).await;
            runs += 1;
            if !returned.contains("(unparseable URL)") {
                leaks.push(format!("{entry_point} {address}: not refused at admission: {returned}"));
            }
            let fields = capture.take();
            for text in std::iter::once(&returned).chain(fields.iter()) {
                for secret in secrets {
                    if text.contains(secret) {
                        leaks.push(format!("{entry_point} {address}: {text}"));
                    }
                }
            }
        }
    }
    assert_eq!(runs, HOSTLESS_ROWS.len() * ENTRY_POINTS.len());
    assert!(leaks.is_empty(), "{} leaks:\n{}", leaks.len(), leaks.join("\n"));
}

/// The capture sees the engine span field for an admitted seed, so an empty capture above
/// is not a subscriber that recorded nothing.
#[tokio::test]
async fn the_capture_records_the_engine_span_of_an_admitted_seed() {
    let capture = FieldCapture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let engine = CrawlEngine::builder()
        .config(CrawlConfig::builder().allow_private_networks(false).build())
        .build()
        .expect("engine builds");

    let _ = engine.scrape("foo://alice:hunter2@example.com/").await;
    let fields = capture.take();
    assert!(
        fields
            .iter()
            .any(|field| field == "span crawl.engine.scrape url.full=foo://example.com/"),
        "expected the scrape span with the admitted seed, got {fields:?}"
    );
    assert!(
        fields.iter().all(|field| !field.contains("hunter2")),
        "the password reached a field: {fields:?}"
    );
}
