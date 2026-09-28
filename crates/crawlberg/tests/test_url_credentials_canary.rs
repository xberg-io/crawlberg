//! A caller's `user:pass@` never rides inside the engine.
//!
//! Every public entry point gets a seed URL carrying a canary password. The engine must take
//! the userinfo off at admission and send it only as an `Authorization: Basic` header to the
//! seed's host. The tests assert three things for each entry point:
//! - no tracing field, error, serialized result or stream event holds the canary;
//! - every URL the `Frontier`, `CrawlStore`, `CrawlCache`, `EventEmitter` and `EventSink`
//!   receive has no userinfo;
//! - the seed host received the header on pages, robots.txt and the sitemap, and the other
//!   host never did; the configured custom headers follow the same host rule.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine as _;
use crawlberg::traits::{
    CompleteEvent, CrawlCache, CrawlStats, CrawlStore, ErrorEvent, EventEmitter, Frontier, FrontierEntry, PageEvent,
};
use crawlberg::{
    AuthConfig, CachedPage, CrawlConfig, CrawlEngine, CrawlError, CrawlEvent, CrawlPageResult, EventSink,
    InMemoryFrontier, ScrapeResult,
};
use serial_test::serial;
use tokio_stream::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const USER: &str = "canary";
const PASSWORD: &str = "CANARY-PW-7f3a";
/// The userinfo a page writes into its own URLs, which is not the caller's.
const PAGE_USERINFO: &str = "page:PAGE-PW-91c2@";

/// Every string any recording seam observed, and every URL it was handed.
#[derive(Default)]
struct Observed {
    texts: Mutex<Vec<String>>,
    urls: Mutex<Vec<String>>,
}

impl Observed {
    fn text(&self, text: impl Into<String>) {
        self.texts.lock().expect("lock").push(text.into());
    }

    fn url(&self, url: &str) {
        self.urls.lock().expect("lock").push(url.to_owned());
        self.text(url);
    }
}

// ---- tracing capture ------------------------------------------------------------------

struct Visitor<'a>(&'a Observed);

impl tracing::field::Visit for Visitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.text(format!("{}={value:?}", field.name()));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.text(format!("{}={value}", field.name()));
    }
}

struct Capture(Arc<Observed>);

impl tracing::Subscriber for Capture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        span.record(&mut Visitor(&self.0));
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        values.record(&mut Visitor(&self.0));
    }
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut Visitor(&self.0));
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

// ---- recording seams ------------------------------------------------------------------

struct RecordingFrontier {
    inner: Arc<dyn Frontier>,
    observed: Arc<Observed>,
}

#[async_trait]
impl Frontier for RecordingFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        self.observed.url(&entry.url);
        self.inner.push(entry).await
    }
    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        self.inner.pop().await
    }
    async fn len(&self) -> Result<usize, CrawlError> {
        self.inner.len().await
    }
    async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
        self.observed.url(url);
        self.inner.is_seen(url).await
    }
    async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
        self.observed.url(url);
        self.inner.mark_seen(url).await
    }
    fn isolated(&self) -> Option<Arc<dyn Frontier>> {
        Some(Arc::new(RecordingFrontier {
            inner: Arc::new(InMemoryFrontier::new()),
            observed: Arc::clone(&self.observed),
        }))
    }
}

struct RecordingStore(Arc<Observed>);

#[async_trait]
impl CrawlStore for RecordingStore {
    async fn store_page(&self, url: &str, result: &ScrapeResult) -> Result<(), CrawlError> {
        self.0.url(url);
        self.0.text(serde_json::to_string(result).expect("result serializes"));
        Ok(())
    }
    async fn store_crawl_page(&self, url: &str, result: &CrawlPageResult) -> Result<(), CrawlError> {
        self.0.url(url);
        self.0.text(serde_json::to_string(result).expect("result serializes"));
        Ok(())
    }
    async fn store_error(&self, url: &str, error: &CrawlError) -> Result<(), CrawlError> {
        self.0.url(url);
        self.0.text(format!("{error} {error:?}"));
        Ok(())
    }
    async fn on_complete(&self, stats: &CrawlStats) -> Result<(), CrawlError> {
        self.0.text(format!("{stats:?}"));
        Ok(())
    }
}

/// An in-memory cache, shared between engines by cloning.
#[derive(Clone)]
struct RecordingCache {
    entries: Arc<Mutex<HashMap<String, CachedPage>>>,
    sets: Arc<Mutex<Vec<String>>>,
    observed: Arc<Observed>,
}

impl RecordingCache {
    fn new(observed: Arc<Observed>) -> Self {
        Self {
            entries: Arc::default(),
            sets: Arc::default(),
            observed,
        }
    }
}

#[async_trait]
impl CrawlCache for RecordingCache {
    async fn get(&self, key: &str) -> Result<Option<CachedPage>, CrawlError> {
        self.observed.url(key);
        Ok(self.entries.lock().expect("lock").get(key).cloned())
    }
    async fn set(&self, key: &str, page: &CachedPage) -> Result<(), CrawlError> {
        self.observed.url(key);
        self.observed.url(&page.url);
        self.sets.lock().expect("lock").push(key.to_owned());
        self.entries.lock().expect("lock").insert(key.to_owned(), page.clone());
        Ok(())
    }
    async fn has(&self, key: &str) -> Result<bool, CrawlError> {
        self.observed.url(key);
        Ok(self.entries.lock().expect("lock").contains_key(key))
    }
}

struct RecordingEmitter(Arc<Observed>);

#[async_trait]
impl EventEmitter for RecordingEmitter {
    async fn on_page(&self, event: &PageEvent) {
        self.0.text(format!("{event:?}"));
    }
    async fn on_error(&self, event: &ErrorEvent) {
        self.0.text(format!("{event:?}"));
    }
    async fn on_complete(&self, event: &CompleteEvent) {
        self.0.text(format!("{event:?}"));
    }
    async fn on_discovered(&self, url: &str, _depth: usize) {
        self.0.url(url);
    }
}

struct RecordingSink(Arc<Observed>);

#[async_trait]
impl EventSink for RecordingSink {
    async fn emit(&self, event: CrawlEvent) {
        if let CrawlEvent::Error { ref url, .. } = event {
            self.0.url(url);
        }
        self.0.text(serde_json::to_string(&event).expect("event serializes"));
    }
}

// ---- the site -------------------------------------------------------------------------

/// The seed host (`127.0.0.1`) and a second host (`localhost`) the seed links to.
struct Site {
    seed_host: MockServer,
    other_host: MockServer,
}

impl Site {
    /// `127.0.0.1:<port>`.
    fn seed_authority(&self) -> String {
        self.seed_host.uri().trim_start_matches("http://").to_owned()
    }

    /// The seed URL with the canary userinfo.
    fn credentialed(&self, at: &str) -> String {
        format!("http://{USER}:{PASSWORD}@{}{at}", self.seed_authority())
    }

    /// A seed-host URL as a page writes it, with the page's own userinfo.
    fn page_supplied(&self, at: &str) -> String {
        format!("http://{PAGE_USERINFO}{}{at}", self.seed_authority())
    }

    /// The seed URL as admission leaves it.
    fn clean(&self, at: &str) -> String {
        format!("http://{}{at}", self.seed_authority())
    }

    /// The second host, addressed as `localhost` so it is a different host from the seed.
    fn other(&self, at: &str) -> String {
        let port = self.other_host.address().port();
        format!("http://localhost:{port}{at}")
    }
}

fn html(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_string(body)
        .insert_header("content-type", "text/html")
}

async fn serve(mock: &MockServer, at: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .mount(mock)
        .await;
}

async fn site() -> Site {
    let site = Site {
        seed_host: MockServer::start().await,
        other_host: MockServer::start().await,
    };
    let seed = &site.seed_host;
    let robots = format!(
        "User-agent: *\nAllow: /\nSitemap: {}\n",
        site.page_supplied("/sitemap.xml")
    );
    serve(seed, "/robots.txt", ResponseTemplate::new(200).set_body_string(robots)).await;
    let sitemap = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>{}</loc></url></urlset>"#,
        site.page_supplied("/from-sitemap")
    );
    serve(
        seed,
        "/sitemap.xml",
        ResponseTemplate::new(200)
            .set_body_string(sitemap)
            .insert_header("content-type", "application/xml"),
    )
    .await;
    let root = format!(
        r#"<html><body>
        <a href="relative">relative</a>
        <a href="{absolute}">absolute with userinfo</a>
        <a href="/redirect">redirect</a>
        <a href="/missing">missing</a>
        <a href="{other_document}">a document on another host</a>
        <img src="{image}">
        <img src="{other_image}">
        </body></html>"#,
        absolute = site.page_supplied("/absolute"),
        image = site.page_supplied("/image.png"),
        other_image = site.other("/pic.png"),
        other_document = site.other("/report.pdf"),
    );
    serve(seed, "/", html(root)).await;
    for leaf in ["/relative", "/absolute", "/landed", "/from-sitemap"] {
        serve(seed, leaf, html(format!("<html><body>{leaf}</body></html>"))).await;
    }
    serve(
        seed,
        "/redirect",
        ResponseTemplate::new(302).insert_header("location", site.page_supplied("/landed").as_str()),
    )
    .await;
    serve(seed, "/missing", ResponseTemplate::new(404)).await;
    serve(
        seed,
        "/to-other",
        ResponseTemplate::new(302).insert_header("location", site.other("/elsewhere").as_str()),
    )
    .await;
    let png = ResponseTemplate::new(200)
        .set_body_bytes(b"\x89PNG\r\n".to_vec())
        .insert_header("content-type", "image/png");
    serve(seed, "/image.png", png.clone()).await;
    serve(&site.other_host, "/pic.png", png).await;
    serve(
        &site.other_host,
        "/report.pdf",
        ResponseTemplate::new(200)
            .set_body_bytes(b"%PDF-1.4\n".to_vec())
            .insert_header("content-type", "application/pdf"),
    )
    .await;
    serve(
        &site.other_host,
        "/elsewhere",
        html("<html><body>elsewhere</body></html>".to_owned()),
    )
    .await;
    site
}

/// Page links stay on the seed host, so a crawl reaches the other host through a document link.
fn config() -> CrawlConfig {
    CrawlConfig {
        max_depth: Some(2),
        respect_robots_txt: true,
        download_assets: true,
        follow_document_urls: true,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

struct Harness {
    engine: CrawlEngine,
    observed: Arc<Observed>,
}

fn harness(config: CrawlConfig) -> Harness {
    let observed = Arc::new(Observed::default());
    let engine = CrawlEngine::builder()
        .config(config)
        .frontier(RecordingFrontier {
            inner: Arc::new(InMemoryFrontier::new()),
            observed: Arc::clone(&observed),
        })
        .store(RecordingStore(Arc::clone(&observed)))
        .cache(RecordingCache::new(Arc::clone(&observed)))
        .event_emitter(RecordingEmitter(Arc::clone(&observed)))
        .event_sink(RecordingSink(Arc::clone(&observed)))
        .build()
        .expect("engine must build");
    Harness { engine, observed }
}

/// Run `operation` with every tracing span and event captured into `observed`.
async fn traced<F: std::future::Future>(observed: &Arc<Observed>, operation: F) -> F::Output {
    let _guard = tracing::subscriber::set_default(Capture(Arc::clone(observed)));
    operation.await
}

// ---- assertions -----------------------------------------------------------------------

fn basic_header() -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{PASSWORD}"));
    format!("Basic {encoded}")
}

/// The `Authorization` header each request to `mock` for `at` carried.
async fn authorization_on(mock: &MockServer, at: &str) -> Vec<Option<String>> {
    mock.received_requests()
        .await
        .expect("request recording is on")
        .into_iter()
        .filter(|request| request.url.path() == at)
        .map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        })
        .collect()
}

/// `at` on the seed host was requested, and every request carried the caller's Basic header.
async fn assert_authenticated(site: &Site, at: &str) {
    let seen = authorization_on(&site.seed_host, at).await;
    assert!(!seen.is_empty(), "{at} on the seed host must have been requested");
    assert!(
        seen.iter()
            .all(|header| header.as_deref() == Some(basic_header().as_str())),
        "every request for {at} on the seed host must carry the caller's Basic header, got {seen:?}"
    );
}

/// The other host was requested, and never with an `Authorization` header.
async fn assert_other_host_unauthenticated(site: &Site) {
    let requests = site
        .other_host
        .received_requests()
        .await
        .expect("request recording is on");
    assert!(!requests.is_empty(), "the crawl must have reached the other host");
    for request in requests {
        assert!(
            request.headers.get("authorization").is_none(),
            "the other host must never receive the caller's credentials, got {:?} on {}",
            request.headers.get("authorization"),
            request.url
        );
    }
}

/// No observed text holds the canary; the seed host's clean address shows up somewhere.
fn assert_no_canary(site: &Site, observed: &Observed, output: &str) {
    let texts = observed.texts.lock().expect("lock");
    let leaks: Vec<&String> = texts.iter().filter(|text| text.contains(PASSWORD)).collect();
    assert!(leaks.is_empty(), "the canary password leaked into {leaks:#?}");
    assert!(
        !output.contains(PASSWORD),
        "the canary password leaked into the output: {output}"
    );
    assert!(
        texts.iter().any(|text| text.contains(&site.seed_authority())),
        "the capture must have seen the seed host, or the absence above proves nothing"
    );
    assert!(
        output.contains(&site.seed_authority()),
        "the output must name the seed host, or the absence above proves nothing: {output}"
    );
}

/// Every URL a seam received parses and has no userinfo. A scrape has no frontier or store, and a
/// credentialed scrape bypasses the cache, so only a crawl is expected to feed them.
fn assert_no_userinfo_reached_a_seam(observed: &Observed, expect_urls: bool) {
    let urls = observed.urls.lock().expect("lock");
    if expect_urls {
        assert!(
            !urls.is_empty(),
            "the seams must have received URLs, or the check below proves nothing"
        );
    }
    for url in urls.iter() {
        if let Ok(parsed) = url::Url::parse(url) {
            assert!(
                parsed.username().is_empty() && parsed.password().is_none(),
                "a seam received a URL with userinfo: {url}"
            );
        }
        assert!(!url.contains(USER), "a seam received the canary user: {url}");
    }
}

// ---- the entry points -----------------------------------------------------------------

#[tokio::test]
#[serial(url_credentials_canary)]
async fn scrape_admits_the_seed_and_authenticates_only_the_seed_host() {
    let site = site().await;
    let harness = harness(config());

    let result = traced(&harness.observed, harness.engine.scrape(&site.credentialed("/"))).await;
    let result = result.expect("scrape must succeed");
    let output = serde_json::to_string(&result).expect("result serializes");

    assert_eq!(result.final_url, site.clean("/"));
    assert!(
        result.links.iter().any(|link| link.url == site.clean("/absolute")),
        "a page-supplied link loses its userinfo: {:?}",
        result.links
    );
    assert!(result.images.iter().any(|image| image.url == site.clean("/image.png")));
    let markdown = serde_json::to_string(&result.markdown).expect("markdown serializes");
    assert!(
        markdown.contains(&site.clean("/absolute")) && !markdown.contains("PAGE-PW"),
        "a markdown link target loses the page's userinfo: {markdown}"
    );
    assert_no_canary(&site, &harness.observed, &output);
    assert_no_userinfo_reached_a_seam(&harness.observed, false);
    assert_authenticated(&site, "/").await;
    assert_authenticated(&site, "/image.png").await;
    assert_other_host_unauthenticated(&site).await;
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_redirect_to_another_host_drops_the_credentials() {
    let site = site().await;
    let harness = harness(config());

    let result = harness
        .engine
        .scrape(&site.credentialed("/to-other"))
        .await
        .expect("scrape must succeed");

    assert_eq!(result.final_url, site.other("/elsewhere"));
    assert_authenticated(&site, "/to-other").await;
    assert_other_host_unauthenticated(&site).await;
}

// ---- custom headers ---------------------------------------------------------------------

const CUSTOM_HEADER: &str = "x-scope-canary";
const CUSTOM_VALUE: &str = "seed-host-only";

/// The canary config plus a custom header and a custom `Authorization` the URL credential replaces.
fn config_with_custom_headers() -> CrawlConfig {
    CrawlConfig {
        custom_headers: HashMap::from([
            (CUSTOM_HEADER.to_owned(), CUSTOM_VALUE.to_owned()),
            ("authorization".to_owned(), "Bearer custom-header-token".to_owned()),
        ]),
        ..config()
    }
}

/// Every request for `at` on the seed host carried the custom header once, and exactly one
/// `Authorization` value: the caller's Basic credential, not the custom one.
async fn assert_custom_headers_on_seed(site: &Site, at: &str) {
    let requests: Vec<_> = site
        .seed_host
        .received_requests()
        .await
        .expect("request recording is on")
        .into_iter()
        .filter(|request| request.url.path() == at)
        .collect();
    assert!(!requests.is_empty(), "{at} on the seed host must have been requested");
    for request in requests {
        let custom: Vec<_> = request.headers.get_all(CUSTOM_HEADER).iter().collect();
        assert_eq!(
            custom,
            [CUSTOM_VALUE],
            "{at} on the seed host must carry the custom header once"
        );
        let authorization: Vec<_> = request
            .headers
            .get_all("authorization")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect();
        assert_eq!(
            authorization,
            [basic_header().as_str()],
            "{at} on the seed host must carry only the caller's Basic credential"
        );
    }
}

/// The other host was requested, and never with a custom header.
async fn assert_other_host_without_custom_headers(site: &Site) {
    let requests = site
        .other_host
        .received_requests()
        .await
        .expect("request recording is on");
    assert!(!requests.is_empty(), "the call must have reached the other host");
    for request in requests {
        assert!(
            request.headers.get(CUSTOM_HEADER).is_none() && request.headers.get("authorization").is_none(),
            "the other host must never receive the custom headers, got {:?} on {}",
            request.headers,
            request.url
        );
    }
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_crawl_sends_the_custom_headers_to_the_seed_host_only() {
    let site = site().await;
    let harness = harness(config_with_custom_headers());

    harness
        .engine
        .crawl(&site.credentialed("/"))
        .await
        .expect("crawl must succeed");

    for at in ["/", "/robots.txt", "/relative", "/landed"] {
        assert_custom_headers_on_seed(&site, at).await;
    }
    assert_other_host_without_custom_headers(&site).await;
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_redirect_to_another_host_drops_the_custom_headers() {
    let site = site().await;
    let harness = harness(config_with_custom_headers());

    let result = harness
        .engine
        .scrape(&site.credentialed("/to-other"))
        .await
        .expect("scrape must succeed");

    assert_eq!(result.final_url, site.other("/elsewhere"));
    assert_custom_headers_on_seed(&site, "/to-other").await;
    assert_other_host_without_custom_headers(&site).await;
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn crawl_admits_the_seed_and_authenticates_only_the_seed_host() {
    let site = site().await;
    let harness = harness(config());

    let result = traced(&harness.observed, harness.engine.crawl(&site.credentialed("/"))).await;
    let result = result.expect("crawl must succeed");
    let output = serde_json::to_string(&result).expect("result serializes");

    assert_eq!(result.final_url, site.clean("/"));
    for leaf in ["/relative", "/absolute", "/landed"] {
        assert!(
            result.pages.iter().any(|page| page.final_url == site.clean(leaf)),
            "the crawl must reach {leaf}: {:?}",
            result.pages.iter().map(|page| &page.final_url).collect::<Vec<_>>()
        );
    }
    assert_no_canary(&site, &harness.observed, &output);
    assert_no_userinfo_reached_a_seam(&harness.observed, true);
    for at in ["/", "/robots.txt", "/relative", "/absolute", "/landed"] {
        assert_authenticated(&site, at).await;
    }
    assert_other_host_unauthenticated(&site).await;
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn crawl_stream_admits_the_seed_and_authenticates_only_the_seed_host() {
    let site = site().await;
    let harness = harness(config());

    let events: Vec<CrawlEvent> = traced(
        &harness.observed,
        harness
            .engine
            .crawl_stream(&site.credentialed("/"))
            .collect::<Vec<CrawlEvent>>(),
    )
    .await;
    let output = serde_json::to_string(&events).expect("events serialize");

    assert!(
        events.iter().any(|event| matches!(event, CrawlEvent::Complete { .. })),
        "the stream must complete: {output}"
    );
    assert_no_canary(&site, &harness.observed, &output);
    assert_no_userinfo_reached_a_seam(&harness.observed, true);
    assert_authenticated(&site, "/").await;
    assert_authenticated(&site, "/robots.txt").await;
    assert_other_host_unauthenticated(&site).await;
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn batch_scrape_keys_results_by_the_admitted_url() {
    let site = site().await;
    let harness = harness(config());
    let seed = site.credentialed("/");

    let results = traced(&harness.observed, harness.engine.batch_scrape(&[seed.as_str()])).await;
    let [(key, outcome)] = results.as_slice() else {
        panic!("one URL must give one result, got {}", results.len());
    };
    let result = outcome.as_ref().expect("scrape must succeed");
    let output = format!("{key} {}", serde_json::to_string(result).expect("result serializes"));

    assert_eq!(key, &site.clean("/"), "a batch result is keyed by the admitted URL");
    assert_no_canary(&site, &harness.observed, &output);
    assert_no_userinfo_reached_a_seam(&harness.observed, false);
    assert_authenticated(&site, "/").await;
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn batch_crawl_keys_results_by_the_admitted_url() {
    let site = site().await;
    let harness = harness(config());
    let seed = site.credentialed("/");

    let results = traced(&harness.observed, harness.engine.batch_crawl(&[seed.as_str()])).await;
    let [(key, outcome)] = results.as_slice() else {
        panic!("one URL must give one result, got {}", results.len());
    };
    let result = outcome.as_ref().expect("crawl must succeed");
    let output = format!("{key} {}", serde_json::to_string(result).expect("result serializes"));

    assert_eq!(key, &site.clean("/"), "a batch result is keyed by the admitted URL");
    assert_no_canary(&site, &harness.observed, &output);
    assert_no_userinfo_reached_a_seam(&harness.observed, true);
    assert_authenticated(&site, "/").await;
    assert_authenticated(&site, "/robots.txt").await;
    assert_other_host_unauthenticated(&site).await;
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn batch_crawl_stream_admits_every_seed() {
    let site = site().await;
    let harness = harness(config());
    let seed = site.credentialed("/");

    let events: Vec<CrawlEvent> = traced(
        &harness.observed,
        harness
            .engine
            .batch_crawl_stream(&[seed.as_str()])
            .collect::<Vec<CrawlEvent>>(),
    )
    .await;
    let output = serde_json::to_string(&events).expect("events serialize");

    assert!(
        events.iter().any(|event| matches!(event, CrawlEvent::Page { .. })),
        "the stream must report pages: {output}"
    );
    assert_no_canary(&site, &harness.observed, &output);
    assert_no_userinfo_reached_a_seam(&harness.observed, true);
    assert_authenticated(&site, "/").await;
    assert_other_host_unauthenticated(&site).await;
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn map_authenticates_robots_and_the_sitemap_and_strips_sitemap_userinfo() {
    let site = site().await;
    let harness = harness(config());

    let result = traced(&harness.observed, harness.engine.map(&site.credentialed("/"))).await;
    let result = result.expect("map must succeed");
    let output = serde_json::to_string(&result).expect("result serializes");

    assert!(
        result.urls.iter().any(|entry| entry.url == site.clean("/from-sitemap")),
        "a sitemap <loc> loses its userinfo: {output}"
    );
    assert_no_canary(&site, &harness.observed, &output);
    assert_no_userinfo_reached_a_seam(&harness.observed, false);
    assert_authenticated(&site, "/robots.txt").await;
    assert_authenticated(&site, "/sitemap.xml").await;
}

#[cfg(feature = "browser-native")]
#[tokio::test]
#[serial(url_credentials_canary)]
async fn interact_on_the_native_backend_authenticates_only_the_seed_host() {
    use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, PageAction};

    let site = site().await;
    let harness = harness(CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: std::time::Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..config()
    });
    let actions = [PageAction::Wait {
        milliseconds: Some(10),
        selector: None,
    }];

    let result = traced(
        &harness.observed,
        harness.engine.interact(&site.credentialed("/"), &actions),
    )
    .await;
    let result = result.expect("interact must succeed");
    let output = serde_json::to_string(&result).expect("result serializes");

    assert_no_canary(&site, &harness.observed, &output);
    assert_authenticated(&site, "/").await;
}

// ---- configuration and errors ---------------------------------------------------------

#[tokio::test]
#[serial(url_credentials_canary)]
async fn url_credentials_together_with_configured_auth_are_a_config_error() {
    let site = site().await;
    let harness = harness(CrawlConfig {
        auth: Some(AuthConfig::Bearer {
            token: "configured".to_owned(),
        }),
        ..config()
    });

    let error = harness
        .engine
        .scrape(&site.credentialed("/"))
        .await
        .expect_err("two sources of credentials must be refused");

    assert!(matches!(error, CrawlError::InvalidConfig { .. }), "{error:?}");
    assert!(
        error.to_string().contains("auth"),
        "the error names the setting: {error}"
    );
    assert!(!format!("{error} {error:?}").contains(PASSWORD));
    let requests = site
        .seed_host
        .received_requests()
        .await
        .expect("request recording is on");
    assert!(requests.is_empty(), "nothing goes out for a refused configuration");
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn an_unparseable_seed_is_refused_without_echoing_it() {
    let harness = harness(config());
    let raw = format!("http://{USER}:{PASSWORD}@exa mple.test/");

    let error = harness
        .engine
        .scrape(&raw)
        .await
        .expect_err("an unparseable URL is refused");
    let text = format!("{error} {error:?}");
    assert!(text.contains("invalid URL"), "{text}");
    assert!(!text.contains(PASSWORD), "{text}");

    let results = harness.engine.batch_crawl(&[raw.as_str()]).await;
    let [(key, outcome)] = results.as_slice() else {
        panic!("one URL must give one result, got {}", results.len());
    };
    assert!(outcome.is_err());
    assert!(
        !key.contains(PASSWORD),
        "a batch key never echoes an unparseable URL: {key}"
    );
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn configured_auth_reaches_only_the_seed_host() {
    let site = site().await;
    let harness = harness(CrawlConfig {
        auth: Some(AuthConfig::Bearer {
            token: "configured-token".to_owned(),
        }),
        ..config()
    });

    harness
        .engine
        .scrape(&site.clean("/"))
        .await
        .expect("scrape must succeed");

    for at in ["/", "/image.png"] {
        let seen = authorization_on(&site.seed_host, at).await;
        assert!(
            !seen.is_empty()
                && seen
                    .iter()
                    .all(|header| header.as_deref() == Some("Bearer configured-token")),
            "{at} on the seed host gets the configured token: {seen:?}"
        );
    }
    assert_other_host_unauthenticated(&site).await;
}

// ---- the shared cache -----------------------------------------------------------------

/// Two engines sharing one response cache, one with credentials and one without.
async fn scrape_twice_through_one_cache(site: &Site, first: CrawlConfig, first_url: &str) -> (usize, Vec<String>) {
    let observed = Arc::new(Observed::default());
    let cache = RecordingCache::new(Arc::clone(&observed));
    let authenticated = CrawlEngine::builder()
        .config(first)
        .cache(cache.clone())
        .build()
        .expect("engine must build");
    let anonymous = CrawlEngine::builder()
        .config(config())
        .cache(cache.clone())
        .build()
        .expect("engine must build");

    authenticated
        .scrape(first_url)
        .await
        .expect("first scrape must succeed");
    let sets_after_first = cache.sets.lock().expect("lock").clone();
    anonymous
        .scrape(&site.clean("/relative"))
        .await
        .expect("second scrape must succeed");

    let requests = site
        .seed_host
        .received_requests()
        .await
        .expect("request recording is on")
        .into_iter()
        .filter(|request| request.url.path() == "/relative")
        .count();
    (requests, sets_after_first)
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_response_fetched_with_configured_auth_is_never_served_from_the_shared_cache() {
    let site = site().await;
    let with_auth = CrawlConfig {
        auth: Some(AuthConfig::Bearer {
            token: "configured-token".to_owned(),
        }),
        ..config()
    };

    let (requests, sets_after_first) = scrape_twice_through_one_cache(&site, with_auth, &site.clean("/relative")).await;

    assert!(
        sets_after_first.is_empty(),
        "an authorized response is not stored: {sets_after_first:?}"
    );
    assert_eq!(
        requests, 2,
        "the anonymous scrape must go to the network, not reuse the authorized page"
    );
}

#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_response_fetched_with_url_credentials_is_never_served_from_the_shared_cache() {
    let site = site().await;

    let (requests, sets_after_first) =
        scrape_twice_through_one_cache(&site, config(), &site.credentialed("/relative")).await;

    assert!(
        sets_after_first.is_empty(),
        "an authorized response is not stored: {sets_after_first:?}"
    );
    assert_eq!(
        requests, 2,
        "the anonymous scrape must go to the network, not reuse the authorized page"
    );
}

/// A page an anonymous scrape cached never answers a later credentialed scrape.
#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_page_cached_by_an_anonymous_scrape_is_never_served_to_a_credentialed_one() {
    let site = site().await;
    let observed = Arc::new(Observed::default());
    let cache = RecordingCache::new(Arc::clone(&observed));
    let engine = CrawlEngine::builder()
        .config(config())
        .cache(cache.clone())
        .build()
        .expect("engine must build");

    engine
        .scrape(&site.clean("/relative"))
        .await
        .expect("the anonymous scrape must succeed");
    let sets_after_anonymous = cache.sets.lock().expect("lock").clone();
    assert!(
        !sets_after_anonymous.is_empty(),
        "the anonymous response is cached, so a read could serve it"
    );
    engine
        .scrape(&site.credentialed("/relative"))
        .await
        .expect("the credentialed scrape must succeed");

    let seen = authorization_on(&site.seed_host, "/relative").await;
    assert_eq!(
        seen,
        [None, Some(basic_header())],
        "the credentialed scrape must go to the network with its credentials, not reuse the anonymous page"
    );
}

/// A robots.txt read anonymously never answers a later credentialed crawl.
#[tokio::test]
#[serial(url_credentials_canary)]
async fn an_anonymous_robots_txt_read_is_not_shared_with_a_credentialed_crawl() {
    let site = site().await;
    let engine = CrawlEngine::builder()
        .config(CrawlConfig {
            max_depth: Some(0),
            ..config()
        })
        .build()
        .expect("engine must build");

    engine
        .crawl(&site.clean("/"))
        .await
        .expect("the anonymous crawl must succeed");
    engine
        .crawl(&site.credentialed("/"))
        .await
        .expect("the credentialed crawl must succeed");

    let robots = authorization_on(&site.seed_host, "/robots.txt").await;
    assert_eq!(
        robots,
        [None, Some(basic_header())],
        "the credentialed crawl reads robots.txt itself, with its credentials"
    );
}

/// A robots.txt read with the caller's credentials never answers a later anonymous crawl.
#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_robots_txt_read_with_credentials_is_not_shared_with_an_anonymous_crawl() {
    let site = site().await;
    let engine = CrawlEngine::builder()
        .config(CrawlConfig {
            max_depth: Some(0),
            ..config()
        })
        .build()
        .expect("engine must build");

    engine
        .crawl(&site.credentialed("/"))
        .await
        .expect("the credentialed crawl must succeed");
    engine
        .crawl(&site.clean("/"))
        .await
        .expect("the anonymous crawl must succeed");

    let robots = authorization_on(&site.seed_host, "/robots.txt").await;
    assert!(
        robots
            .iter()
            .any(|header| header.as_deref() == Some(basic_header().as_str())),
        "the credentialed crawl reads robots.txt with its credentials: {robots:?}"
    );
    assert!(
        robots.iter().any(Option::is_none),
        "the anonymous crawl reads robots.txt itself, not the credentialed copy: {robots:?}"
    );
}

/// A stream reports a refused seed without echoing it.
#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_stream_error_for_an_unparseable_seed_does_not_echo_it() {
    let harness = harness(config());
    let raw = format!("http://{USER}:{PASSWORD}@exa mple.test/");

    let single: Vec<CrawlEvent> = harness.engine.crawl_stream(&raw).collect().await;
    let batch: Vec<CrawlEvent> = harness.engine.batch_crawl_stream(&[raw.as_str()]).collect().await;

    for events in [&single, &batch] {
        let output = serde_json::to_string(events).expect("events serialize");
        assert!(
            events.iter().any(|event| matches!(event, CrawlEvent::Error { .. })),
            "the refusal must be reported: {output}"
        );
        assert!(output.contains("invalid URL"), "{output}");
        assert!(!output.contains(PASSWORD), "the stream echoed the seed: {output}");
    }
}

// ---- page-supplied URLs ---------------------------------------------------------------

/// A page's link with userinfo that the SSRF policy rejects is logged without the userinfo.
#[tokio::test]
#[serial(url_credentials_canary)]
async fn a_rejected_page_link_is_logged_without_its_userinfo() {
    let mock = MockServer::start().await;
    // ~keep A document link may leave the seed host, and 10.0.0.0/8 is outside the allowlist,
    // ~keep so the policy rejects it without a DNS lookup.
    let page =
        format!(r#"<html><body><a href="http://{USER}:{PASSWORD}@10.0.0.1/report.pdf">report</a></body></html>"#);
    serve(&mock, "/", html(page)).await;
    let observed = Arc::new(Observed::default());
    let engine = CrawlEngine::builder()
        .config(
            CrawlConfig::builder()
                .ssrf_allowlist_host(crawlberg::HostMatcher::cidr("127.0.0.0/8").expect("literal CIDR is valid"))
                .build(),
        )
        .build()
        .expect("engine must build");

    traced(&observed, engine.crawl(&mock.uri()))
        .await
        .expect("the seed itself is allowlisted");

    let texts = observed.texts.lock().expect("lock");
    assert!(
        texts.iter().any(|text| text.contains("http://10.0.0.1/report.pdf")),
        "the rejected link must be logged, without its userinfo: {texts:#?}"
    );
    assert!(
        texts.iter().all(|text| !text.contains(PASSWORD)),
        "the link's password leaked: {texts:#?}"
    );
}
