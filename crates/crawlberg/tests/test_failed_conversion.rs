//! A page whose conversion to Markdown fails is an error for that page, never a page with no
//! Markdown.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use crawlberg::traits::CrawlCache;
use crawlberg::{
    CachedPage, CrawlConfig, CrawlEngine, CrawlError, CrawlEvent, batch_crawl, batch_scrape, crawl, crawl_stream,
    create_engine, scrape,
};
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// How the text of every failed conversion starts: the stable tag of its error class, which a
/// caller that holds only the text (a stream event, a batch item) reads to know the class.
const FAILED_CONVERSION: &str = "conversion_failed: could not convert ";

/// A page served as HTML that the converter refuses: it starts with the signature of a zip
/// archive. The refusal is the converter's own input check, so it does not depend on a defect.
const REFUSED_PAGE: &str = "PK\u{3}\u{4}<html><body><p>This is not a page.</p></body></html>";

/// The cause the converter gives for [`REFUSED_PAGE`].
const REFUSED_CAUSE: &str = "zip archive";

const STREAM_TIMEOUT: Duration = Duration::from_secs(30);

fn config(max_depth: usize) -> CrawlConfig {
    CrawlConfig {
        max_depth: Some(max_depth),
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

async fn mount_html(mock: &MockServer, at: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

/// A seed that links to a page the converter refuses and to a page it converts.
async fn site_with_a_refused_child() -> MockServer {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><p>Seed page.</p><a href="/refused">refused</a> <a href="/good">good</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/refused", REFUSED_PAGE).await;
    mount_html(&mock, "/good", "<html><body><p>Good page.</p></body></html>").await;
    mock
}

fn assert_failed_conversion(error: &str, page: &str) {
    assert!(
        error.starts_with(FAILED_CONVERSION) && error.contains(REFUSED_CAUSE),
        "the error starts with the tag of a failed conversion and says why, got: {error}"
    );
    assert!(error.contains(page), "the error names the page {page}, got: {error}");
}

#[tokio::test]
async fn a_scrape_of_a_page_that_cannot_be_converted_returns_the_error() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/refused", REFUSED_PAGE).await;
    mount_html(&mock, "/good", "<html><body><p>Good page.</p></body></html>").await;
    let engine = create_engine(Some(config(0))).expect("the engine builds");
    let refused = format!("{}/refused", mock.uri());

    let good = scrape(&engine, &format!("{}/good", mock.uri()))
        .await
        .expect("a page the converter accepts is scraped");
    let markdown = good.markdown.expect("a converted page has Markdown");
    assert!(markdown.content.contains("Good page."), "got: {}", markdown.content);

    let error = scrape(&engine, &refused)
        .await
        .expect_err("a page that cannot be converted is not a result");
    assert!(matches!(error, CrawlError::ConversionFailed { .. }), "got: {error:?}");
    assert_failed_conversion(&error.to_string(), &refused);
}

/// A response cache that keeps every page in memory and never expires one.
#[derive(Default)]
struct MemoryCache {
    pages: Mutex<HashMap<String, CachedPage>>,
}

#[async_trait]
impl CrawlCache for MemoryCache {
    async fn get(&self, key: &str) -> Result<Option<CachedPage>, CrawlError> {
        Ok(self.pages.lock().expect("the cache lock").get(key).cloned())
    }

    async fn set(&self, key: &str, page: &CachedPage) -> Result<(), CrawlError> {
        self.pages
            .lock()
            .expect("the cache lock")
            .insert(key.to_owned(), page.clone());
        Ok(())
    }

    async fn has(&self, key: &str) -> Result<bool, CrawlError> {
        Ok(self.pages.lock().expect("the cache lock").contains_key(key))
    }
}

/// The second scrape of a page is served from the response cache: the server sees one request.
/// The page fails as it did the first time.
#[tokio::test]
async fn a_scrape_served_from_the_cache_returns_the_error_again() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/refused"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(REFUSED_PAGE)
                .append_header("content-type", "text/html"),
        )
        .expect(1)
        .mount(&mock)
        .await;
    let engine = CrawlEngine::builder()
        .config(config(0))
        .cache(MemoryCache::default())
        .build()
        .expect("the engine builds");
    let refused = format!("{}/refused", mock.uri());

    for attempt in ["first", "second"] {
        let error = engine
            .scrape(&refused)
            .await
            .expect_err("a page that cannot be converted is not a result");
        assert!(
            matches!(error, CrawlError::ConversionFailed { .. }),
            "the {attempt} scrape, got: {error:?}"
        );
        assert_failed_conversion(&error.to_string(), &refused);
    }
}

#[tokio::test]
async fn a_batch_crawl_reports_the_seed_that_cannot_be_converted() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/refused", REFUSED_PAGE).await;
    mount_html(&mock, "/good", "<html><body><p>Good page.</p></body></html>").await;
    let engine = create_engine(Some(config(0))).expect("the engine builds");
    let refused = format!("{}/refused", mock.uri());
    let good = format!("{}/good", mock.uri());

    let results = batch_crawl(&engine, vec![good.clone(), refused.clone()])
        .await
        .expect("the batch runs");

    let of = |url: &str| {
        results
            .results
            .iter()
            .find(|item| item.url == url)
            .and_then(|item| item.result.as_ref())
            .unwrap_or_else(|| panic!("the batch has a crawl for {url}"))
    };
    assert_eq!(of(&good).pages.len(), 1, "the converted seed is a page");
    assert!(of(&good).error.is_none(), "the converted seed has no error");
    assert!(of(&refused).pages.is_empty(), "the refused seed is not a page");
    let error = of(&refused)
        .error
        .as_deref()
        .expect("the refused seed is the error of its crawl");
    assert_failed_conversion(error, &refused);
}

#[tokio::test]
async fn a_batch_scrape_reports_the_page_that_cannot_be_converted() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/refused", REFUSED_PAGE).await;
    mount_html(&mock, "/good", "<html><body><p>Good page.</p></body></html>").await;
    let engine = create_engine(Some(config(0))).expect("the engine builds");
    let refused = format!("{}/refused", mock.uri());
    let good = format!("{}/good", mock.uri());

    let results = batch_scrape(&engine, vec![good.clone(), refused.clone()])
        .await
        .expect("the batch runs");

    let of = |url: &str| {
        results
            .results
            .iter()
            .find(|item| item.url == url)
            .unwrap_or_else(|| panic!("the batch reports {url}"))
    };
    assert!(of(&good).result.is_some(), "the converted page is a result");
    assert!(of(&refused).result.is_none(), "the refused page is not a result");
    let error = of(&refused).error.as_deref().expect("the refused page has an error");
    assert_failed_conversion(error, &refused);
}

#[tokio::test]
async fn a_crawl_stream_sends_an_error_event_for_the_page_and_goes_on() {
    let mock = site_with_a_refused_child().await;
    let engine = create_engine(Some(config(1))).expect("the engine builds");
    let refused = format!("{}/refused", mock.uri());

    let stream = crawl_stream(&engine, &mock.uri()).await.expect("the stream starts");
    let events: Vec<CrawlEvent> = tokio::time::timeout(
        STREAM_TIMEOUT,
        stream.map(|event| event.expect("a stream item")).collect(),
    )
    .await
    .expect("the stream ends");

    let mut pages: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            CrawlEvent::Page { result } => Some(result.url.as_str()),
            _ => None,
        })
        .collect();
    pages.sort_unstable();
    let good = format!("{}/good", mock.uri());
    let seed = mock.uri();
    let seed = seed.as_str();
    assert_eq!(
        pages.iter().map(|url| url.trim_end_matches('/')).collect::<Vec<_>>(),
        vec![seed, good.as_str()],
        "the seed and the converted child are pages, the refused child is not: {events:?}"
    );

    let errors: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|event| match event {
            CrawlEvent::Error { url, error } => Some((url.as_str(), error.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1, "one error event, for the refused child: {events:?}");
    assert_eq!(errors[0].0, refused, "the error event is for the refused child");
    assert_failed_conversion(errors[0].1, &refused);

    assert!(
        events
            .iter()
            .any(|event| matches!(event, CrawlEvent::Complete { pages_crawled: 2 })),
        "the crawl completes with the two converted pages: {events:?}"
    );
}

#[tokio::test]
async fn a_crawl_leaves_out_the_page_that_cannot_be_converted() {
    let mock = site_with_a_refused_child().await;
    let engine = create_engine(Some(config(1))).expect("the engine builds");

    let result = crawl(&engine, &mock.uri()).await.expect("the crawl completes");

    let mut pages: Vec<&str> = result.pages.iter().map(|page| page.url.as_str()).collect();
    pages.sort_unstable();
    assert_eq!(pages.len(), 2, "the seed and the converted child: {pages:?}");
    assert!(pages[1].ends_with("/good"), "the converted child is a page: {pages:?}");
    assert!(
        result.pages.iter().all(|page| page.markdown.is_some()),
        "every page of the crawl has Markdown"
    );
    assert!(result.error.is_none(), "a failed child is not the error of the crawl");
}

#[tokio::test]
async fn a_crawl_whose_seed_cannot_be_converted_reports_it_as_its_error() {
    let mock = MockServer::start().await;
    // ~keep A link the crawl would follow, to show a refused page contributes no links.
    mount_html(&mock, "/", &format!(r#"{REFUSED_PAGE}<a href="/good">good</a>"#)).await;
    Mock::given(method("GET"))
        .and(path("/good"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<p>Good page.</p>"))
        .expect(0)
        .mount(&mock)
        .await;
    let engine = create_engine(Some(config(1))).expect("the engine builds");

    let result = crawl(&engine, &mock.uri()).await.expect("the crawl completes");

    assert!(result.pages.is_empty(), "a refused seed is not a page");
    let error = result
        .error
        .as_deref()
        .expect("a refused seed is the error of the crawl");
    assert_failed_conversion(error, &mock.uri());
}
