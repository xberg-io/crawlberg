//! A page whose conversion to Markdown fails is an error for that page, never a page with no
//! Markdown.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crawlberg::traits::{CrawlCache, CrawlStats, CrawlStore};
use crawlberg::{
    CachedPage, CrawlConfig, CrawlEngine, CrawlError, CrawlEvent, CrawlPageResult, ScrapeResult, batch_crawl,
    batch_scrape, crawl, crawl_stream, create_engine, scrape,
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

/// Serve `body` at `at` with the content type `text/html`.
///
/// The body is set with its type in one call. `set_body_string` sends `text/plain` whatever header
/// is added to it, and a `text/plain` body that does not start with a tag is not read for links.
async fn mount_html(mock: &MockServer, at: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_owned(), "text/html"))
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
        .respond_with(ResponseTemplate::new(200).set_body_raw(REFUSED_PAGE.to_owned(), "text/html"))
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
            .unwrap_or_else(|| panic!("the batch reports {url}"))
    };
    let converted = of(&good)
        .result
        .as_ref()
        .expect("the converted seed has a crawl result");
    assert_eq!(converted.pages.len(), 1, "the converted seed is a page");
    assert!(of(&good).error.is_none(), "the converted seed has no error");
    // ~keep A batch item whose crawl has an error carries the error and no crawl result.
    assert!(of(&refused).result.is_none(), "the refused seed has no crawl result");
    let error = of(&refused)
        .error
        .as_deref()
        .expect("the refused seed is the error of its batch item");
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
    mount_html(&mock, "/", REFUSED_PAGE).await;
    let engine = create_engine(Some(config(1))).expect("the engine builds");

    let result = crawl(&engine, &mock.uri()).await.expect("the crawl completes");

    assert!(result.pages.is_empty(), "a refused seed is not a page");
    let error = result
        .error
        .as_deref()
        .expect("a refused seed is the error of the crawl");
    assert_failed_conversion(error, &mock.uri());
}

/// A page that cannot be converted gives the crawl no links: the page behind it is never requested.
/// The page behind the converted child is the control: the crawl does follow links at that depth.
#[tokio::test]
async fn a_crawl_follows_no_link_of_a_page_that_cannot_be_converted() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/refused">refused</a> <a href="/good">good</a></body></html>"#,
    )
    .await;
    mount_html(
        &mock,
        "/refused",
        "PK\u{3}\u{4}<html><body><a href=\"/behind-refused\">behind</a></body></html>",
    )
    .await;
    mount_html(
        &mock,
        "/good",
        r#"<html><body><a href="/behind-good">behind</a></body></html>"#,
    )
    .await;
    mount_html(
        &mock,
        "/behind-refused",
        "<html><body><p>Behind the refused page.</p></body></html>",
    )
    .await;
    mount_html(
        &mock,
        "/behind-good",
        "<html><body><p>Behind the good page.</p></body></html>",
    )
    .await;
    let engine = create_engine(Some(config(2))).expect("the engine builds");

    crawl(&engine, &mock.uri()).await.expect("the crawl completes");

    let mut requested: Vec<String> = mock
        .received_requests()
        .await
        .expect("the mock records requests")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    requested.sort_unstable();
    assert_eq!(
        requested,
        vec!["/", "/behind-good", "/good", "/refused"],
        "the crawl requests the page behind the converted child, not the page behind the refused child"
    );
}

/// Types of a page as a server writes them: in any case, with a parameter, with white space.
const PAGE_TYPES_AS_SERVERS_WRITE_THEM: [(&str, &str); 4] = [
    ("/xhtml", "APPLICATION/XHTML+XML"),
    ("/html", "Text/HTML; Charset=UTF-8"),
    ("/spaces", " text/html "),
    ("/plain", "TEXT/PLAIN"),
];

/// A seed that links to [`REFUSED_PAGE`] once for each of [`PAGE_TYPES_AS_SERVERS_WRITE_THEM`].
async fn site_with_refused_pages_of_each_written_type() -> MockServer {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><p>Seed page.</p><a href="/xhtml">1</a> <a href="/html">2</a> <a href="/spaces">3</a> <a href="/plain">4</a></body></html>"#,
    )
    .await;
    for (at, mime) in PAGE_TYPES_AS_SERVERS_WRITE_THEM {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_raw(REFUSED_PAGE, mime))
            .mount(&mock)
            .await;
    }
    mock
}

#[tokio::test]
async fn a_scrape_reads_the_type_of_a_page_in_any_case_and_with_parameters() {
    let mock = site_with_refused_pages_of_each_written_type().await;
    let engine = create_engine(Some(config(0))).expect("the engine builds");

    for (at, mime) in PAGE_TYPES_AS_SERVERS_WRITE_THEM {
        let page = format!("{}{at}", mock.uri());
        let error = match scrape(&engine, &page).await {
            Ok(result) => panic!(
                "a {mime:?} page that cannot be converted is not a result, got Markdown: {:?}",
                result.markdown.map(|markdown| markdown.content)
            ),
            Err(error) => error,
        };
        assert!(
            matches!(error, CrawlError::ConversionFailed { .. }),
            "{mime:?}: {error:?}"
        );
        assert_failed_conversion(&error.to_string(), &page);
    }
}

#[tokio::test]
async fn a_crawl_reads_the_type_of_a_page_in_any_case_and_with_parameters() {
    let mock = site_with_refused_pages_of_each_written_type().await;
    let engine = create_engine(Some(config(1))).expect("the engine builds");

    let stream = crawl_stream(&engine, &mock.uri()).await.expect("the stream starts");
    let events: Vec<CrawlEvent> = tokio::time::timeout(
        STREAM_TIMEOUT,
        stream.map(|event| event.expect("a stream item")).collect(),
    )
    .await
    .expect("the stream ends");

    for (at, mime) in PAGE_TYPES_AS_SERVERS_WRITE_THEM {
        let page = format!("{}{at}", mock.uri());
        let error = events
            .iter()
            .find_map(|event| match event {
                CrawlEvent::Error { url, error } if *url == page => Some(error.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the {mime:?} page has an error event: {events:?}"));
        assert_failed_conversion(error, &page);
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CrawlEvent::Complete { pages_crawled: 1 })),
        "the seed is the one page of the crawl: {events:?}"
    );
}

/// A document that starts as HTML does and that the converter refuses: control bytes follow the
/// markup. The refusal is the converter's own input check, so it does not depend on a defect.
fn refused_html_document() -> Vec<u8> {
    let mut bytes = b"<!doctype html><html><body><p>This is not a page.</p>".to_vec();
    bytes.extend([7u8; 400]);
    bytes
}

/// A body that starts as HTML is a page whatever type it is served with: the scrape and the crawl
/// read its links and metadata as HTML, so a failed conversion of it is the error of a page.
#[tokio::test]
async fn a_body_that_starts_as_html_is_a_page_whatever_type_it_is_served_with() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><p>Seed page.</p><a href="/json">json</a> <a href="/app">app</a></body></html>"#,
    )
    .await;
    let served = [("/json", "application/json"), ("/app", "application/java-archive")];
    for (at, mime) in served {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_raw(refused_html_document(), mime))
            .mount(&mock)
            .await;
    }
    let engine = create_engine(Some(config(1))).expect("the engine builds");

    for (at, mime) in served {
        let page = format!("{}{at}", mock.uri());
        let error = match scrape(&engine, &page).await {
            Ok(result) => panic!(
                "a refused HTML body served as {mime} is not a result, got Markdown: {:?}",
                result.markdown.map(|markdown| markdown.content)
            ),
            Err(error) => error,
        };
        assert!(
            matches!(error, CrawlError::ConversionFailed { .. }),
            "{mime}: {error:?}"
        );
        let text = error.to_string();
        assert!(
            text.starts_with(FAILED_CONVERSION) && text.contains(&page) && text.contains("binary data"),
            "{mime}: the error names the page and the cause, got: {text}"
        );
    }

    let result = crawl(&engine, &mock.uri()).await.expect("the crawl completes");
    assert_eq!(
        result.pages.len(),
        1,
        "the seed is the one page; each HTML body that cannot be converted is left out"
    );
    assert!(result.error.is_none(), "a failed child is not the error of the crawl");
}

/// A body the converter refuses: the signature of a zip archive, then bytes that are not text.
fn archive_bytes() -> Vec<u8> {
    let mut bytes = b"PK\x03\x04".to_vec();
    bytes.extend([7u8; 200]);
    bytes
}

/// Types that no built-in list of binary types names, so the response is not skipped.
const TYPES_THAT_ARE_NOT_PAGES: [(&str, &str); 2] = [("/app", "application/java-archive"), ("/font", "font/woff2")];

/// A seed that links to one archive body for each of [`TYPES_THAT_ARE_NOT_PAGES`].
async fn site_with_bodies_that_are_not_pages() -> MockServer {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><p>Seed page.</p><a href="/app">app</a> <a href="/font">font</a></body></html>"#,
    )
    .await;
    for (at, mime) in TYPES_THAT_ARE_NOT_PAGES {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_raw(archive_bytes(), mime))
            .mount(&mock)
            .await;
    }
    mock
}

/// The configuration that downloads a response of each `mime_types` as a document.
fn downloads(max_depth: usize, mime_types: &[&str]) -> CrawlConfig {
    CrawlConfig {
        download_documents: true,
        document_mime_types: mime_types.iter().map(|mime| (*mime).to_owned()).collect(),
        ..config(max_depth)
    }
}

#[tokio::test]
async fn a_scrape_of_a_body_that_is_not_text_or_html_is_a_result_with_no_markdown() {
    let mock = site_with_bodies_that_are_not_pages().await;
    let engine = create_engine(Some(config(0))).expect("the engine builds");

    for (at, mime) in TYPES_THAT_ARE_NOT_PAGES {
        let result = scrape(&engine, &format!("{}{at}", mock.uri()))
            .await
            .unwrap_or_else(|error| panic!("a {mime} body is not a page that failed, got: {error}"));
        assert_eq!(result.status_code, 200, "{mime}");
        assert!(
            result.markdown.is_none(),
            "a {mime} body the converter refuses has no Markdown"
        );
        assert!(!result.was_skipped, "{mime} is in no built-in list of binary types");
    }
}

#[tokio::test]
async fn a_scrape_keeps_the_document_it_downloads_when_the_body_cannot_be_converted() {
    let mock = site_with_bodies_that_are_not_pages().await;
    mount_html(&mock, "/refused", REFUSED_PAGE).await;
    let wanted = downloads(0, &["application/java-archive", "text/html"]);
    let engine = create_engine(Some(wanted)).expect("the engine builds");

    // ~keep The second row is a page by its type: the download alone keeps it from the error.
    for (at, mime, bytes) in [
        ("/app", "application/java-archive", archive_bytes()),
        ("/refused", "text/html", REFUSED_PAGE.as_bytes().to_vec()),
    ] {
        let result = scrape(&engine, &format!("{}{at}", mock.uri()))
            .await
            .unwrap_or_else(|error| panic!("a downloaded {mime} document is the result, got: {error}"));
        let document = result
            .downloaded_document
            .unwrap_or_else(|| panic!("the {mime} response is downloaded as a document"));
        assert_eq!(document.mime_type, mime);
        assert_eq!(
            document.content, bytes,
            "the document holds the bytes of the {mime} response"
        );
        assert!(result.markdown.is_none(), "the {mime} body has no Markdown");
    }
}

#[tokio::test]
async fn a_batch_scrape_reports_a_body_that_is_not_text_or_html_as_a_result() {
    let mock = site_with_bodies_that_are_not_pages().await;
    let engine = create_engine(Some(config(0))).expect("the engine builds");
    let urls: Vec<String> = TYPES_THAT_ARE_NOT_PAGES
        .iter()
        .map(|(at, _)| format!("{}{at}", mock.uri()))
        .collect();

    let results = batch_scrape(&engine, urls).await.expect("the batch runs");

    assert_eq!(results.results.len(), 2, "one item for each address");
    for item in &results.results {
        assert!(item.error.is_none(), "{} is not an error: {:?}", item.url, item.error);
        let result = item.result.as_ref().expect("the item has a result");
        assert!(result.markdown.is_none(), "{} has no Markdown", item.url);
    }
}

/// The crawl reports the seed and both bodies as pages, with no error event and no failed page.
#[tokio::test]
async fn a_crawl_reports_a_body_that_is_not_text_or_html_as_a_page_with_no_markdown() {
    let mock = site_with_bodies_that_are_not_pages().await;
    let engine = create_engine(Some(config(1))).expect("the engine builds");

    let result = crawl(&engine, &mock.uri()).await.expect("the crawl completes");
    assert!(result.error.is_none(), "got: {:?}", result.error);
    assert_eq!(result.pages.len(), 3, "the seed and the two bodies are pages");
    for (at, mime) in TYPES_THAT_ARE_NOT_PAGES {
        let page = result
            .pages
            .iter()
            .find(|page| page.url.ends_with(at))
            .unwrap_or_else(|| panic!("the {mime} body is a page of the crawl"));
        assert!(page.markdown.is_none(), "the {mime} body has no Markdown");
        assert!(!page.was_skipped, "{mime} is in no built-in list of binary types");
    }

    let stream = crawl_stream(&engine, &mock.uri()).await.expect("the stream starts");
    let events: Vec<CrawlEvent> = tokio::time::timeout(
        STREAM_TIMEOUT,
        stream.map(|event| event.expect("a stream item")).collect(),
    )
    .await
    .expect("the stream ends");
    assert!(
        !events.iter().any(|event| matches!(event, CrawlEvent::Error { .. })),
        "the stream has no error event: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CrawlEvent::Complete { pages_crawled: 3 })),
        "the stream completes with three pages: {events:?}"
    );
}

#[tokio::test]
async fn a_crawl_keeps_the_document_it_downloads_when_the_body_cannot_be_converted() {
    let mock = site_with_bodies_that_are_not_pages().await;
    let wanted = downloads(1, &["application/java-archive"]);
    let engine = create_engine(Some(wanted)).expect("the engine builds");

    let result = crawl(&engine, &mock.uri()).await.expect("the crawl completes");

    assert!(result.error.is_none(), "got: {:?}", result.error);
    let page = result
        .pages
        .iter()
        .find(|page| page.url.ends_with("/app"))
        .expect("the archive is a page of the crawl");
    let document = page
        .downloaded_document
        .as_ref()
        .expect("the archive is downloaded as a document");
    assert_eq!(
        document.content,
        archive_bytes(),
        "the document holds the bytes of the response"
    );
    assert!(page.markdown.is_none(), "the archive has no Markdown");
}

/// A page by its type that the crawl downloads as a document is a page of the crawl, not an error.
#[tokio::test]
async fn a_crawl_keeps_a_page_it_downloads_as_a_document_when_the_page_cannot_be_converted() {
    let mock = site_with_a_refused_child().await;
    let engine = create_engine(Some(downloads(1, &["text/html"]))).expect("the engine builds");
    let refused = format!("{}/refused", mock.uri());

    let result = crawl(&engine, &mock.uri()).await.expect("the crawl completes");

    let page = result
        .pages
        .iter()
        .find(|page| page.url == refused)
        .expect("the downloaded page is a page of the crawl");
    let document = page
        .downloaded_document
        .as_ref()
        .expect("the page is downloaded as a document");
    assert_eq!(document.content, REFUSED_PAGE.as_bytes(), "the document holds the page");
    assert!(page.markdown.is_none(), "the page has no Markdown");
}

/// What a store was given: the errors, and the statistics of the finished crawl.
#[derive(Default)]
struct Stored {
    errors: Mutex<Vec<(String, String)>>,
    stats: Mutex<Option<CrawlStats>>,
}

struct RecordingStore(Arc<Stored>);

#[async_trait]
impl CrawlStore for RecordingStore {
    async fn store_page(&self, _url: &str, _result: &ScrapeResult) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn store_crawl_page(&self, _url: &str, _result: &CrawlPageResult) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn store_error(&self, url: &str, error: &CrawlError) -> Result<(), CrawlError> {
        self.0
            .errors
            .lock()
            .expect("the store lock")
            .push((url.to_owned(), error.to_string()));
        Ok(())
    }

    async fn on_complete(&self, stats: &CrawlStats) -> Result<(), CrawlError> {
        *self.0.stats.lock().expect("the store lock") = Some(stats.clone());
        Ok(())
    }
}

#[tokio::test]
async fn a_crawl_counts_the_page_as_failed_and_gives_its_error_to_the_store() {
    let mock = site_with_a_refused_child().await;
    let stored = Arc::new(Stored::default());
    let engine = CrawlEngine::builder()
        .config(config(1))
        .store(RecordingStore(Arc::clone(&stored)))
        .build()
        .expect("the engine builds");
    let refused = format!("{}/refused", mock.uri());

    engine.crawl(&mock.uri()).await.expect("the crawl completes");

    let stats = stored
        .stats
        .lock()
        .expect("the store lock")
        .clone()
        .expect("the store saw the crawl complete");
    assert_eq!(
        stats.pages_failed, 1,
        "the refused child is the one failed page: {stats:?}"
    );
    let errors = stored.errors.lock().expect("the store lock").clone();
    assert_eq!(errors.len(), 1, "one stored error, for the refused child: {errors:?}");
    assert_eq!(errors[0].0, refused, "the stored error is for the refused child");
    assert_failed_conversion(&errors[0].1, &refused);
}
