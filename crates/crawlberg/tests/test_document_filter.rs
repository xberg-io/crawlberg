//! Coverage for xberg-io/crawlberg#95: a byte-aware document-materialization predicate.
//!
//! The predicate is set on the engine, so every path that builds a `DownloadedDocument` must
//! honour it. `crawl()` builds one in `engine::page_result`; `scrape()` and the wasm crawl loop
//! build one in `scrape::scrape_from_crawl_response`. Both are exercised here.

use std::sync::Arc;

use crawlberg::{CrawlConfig, CrawlEngineBuilder, CrawlEngineHandle, DownloadedDocument, StaticProxyProvider, crawl};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Which predicate, if any, the engine under test carries.
#[derive(Clone, Copy)]
enum Filter {
    /// No predicate: the built-in `document_mime_types` decision stands alone.
    None,
    /// Replaces the built-in decision with a byte check on the body.
    BytesOnly,
    /// Widens the built-in decision with a byte check, using its third argument.
    DeclaredMimeOrBytes,
}

/// A mock server that serves `body` with `content_type` at `/document`, and an engine pointed at
/// it whose only accepted document MIME type is `application/pdf`.
async fn engine_for(content_type: &str, body: &[u8], filter: Filter) -> (MockServer, CrawlEngineHandle) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/document"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.to_vec())
                .append_header("content-type", content_type),
        )
        .mount(&server)
        .await;

    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.respect_robots_txt = false;
    config.max_depth = Some(0);
    config.max_pages = Some(1);
    config.document_mime_types = vec!["application/pdf".to_owned()];

    let mut builder = CrawlEngineBuilder::new()
        .config(config)
        .with_proxy_provider(Arc::new(StaticProxyProvider::empty()));
    builder = match filter {
        Filter::None => builder,
        Filter::BytesOnly => builder.document_filter(|declared_mime_type, bytes: &[u8], _by_declared_mime| {
            bytes.starts_with(b"%PDF") || (declared_mime_type == "application/json" && bytes.starts_with(b"{"))
        }),
        Filter::DeclaredMimeOrBytes => builder.document_filter(|_declared, bytes: &[u8], by_declared_mime| {
            by_declared_mime || bytes.starts_with(b"%PDF")
        }),
    };
    let handle = CrawlEngineHandle::from_engine(builder.build().expect("engine builds"));
    (server, handle)
}

/// Crawl the single served page and return its document record, if any.
async fn crawl_body(content_type: &str, body: &[u8], filter: Filter) -> Option<DownloadedDocument> {
    let (server, handle) = engine_for(content_type, body, filter).await;
    let result = crawl(&handle, &format!("{}/document", server.uri()))
        .await
        .expect("crawl succeeds");
    assert_eq!(result.pages.len(), 1, "the seed must be observed");
    result
        .pages
        .into_iter()
        .next()
        .and_then(|page| page.downloaded_document)
}

/// Scrape the single served page and return its document record, if any.
async fn scrape_body(content_type: &str, body: &[u8], filter: Filter) -> Option<DownloadedDocument> {
    let (server, handle) = engine_for(content_type, body, filter).await;
    crawlberg::scrape(&handle, &format!("{}/document", server.uri()))
        .await
        .expect("scrape succeeds")
        .downloaded_document
}

#[tokio::test]
async fn should_accept_pdf_bytes_despite_text_plain_header_with_filter() {
    let document = crawl_body("text/plain; charset=utf-8", b"%PDF-1.7 body", Filter::BytesOnly)
        .await
        .expect("byte-aware policy accepts PDF");
    assert_eq!(&*document.mime_type, "text/plain");
    assert_eq!(document.content.as_slice(), b"%PDF-1.7 body");
}

#[tokio::test]
async fn should_accept_explicit_json_alongside_pdf_with_filter() {
    let document = crawl_body("application/json", br#"{"ok":true}"#, Filter::BytesOnly)
        .await
        .expect("byte-aware policy accepts JSON");
    assert_eq!(&*document.mime_type, "application/json");
    assert_eq!(document.content.as_slice(), br#"{"ok":true}"#);
}

#[tokio::test]
async fn should_reject_disallowed_bytes_with_filter() {
    let document = crawl_body("application/pdf", b"not a PDF", Filter::BytesOnly).await;
    assert!(
        document.is_none(),
        "byte-aware policy rejects disallowed bytes even under an allowed header"
    );
}

/// Pins the unconfigured default only: with no predicate the `document_mime_types` decision is
/// the whole policy.
///
/// ~keep This passes with the byte-aware feature entirely reverted, by construction -- it
/// ~keep configures no predicate. It is a guard against the feature leaking into the default
/// ~keep path, not evidence that the feature works; the `_with_filter` cases above are that.
#[tokio::test]
async fn should_leave_the_declared_mime_decision_alone_when_no_filter_is_configured() {
    assert!(
        crawl_body("text/plain", b"%PDF-1.7 body", Filter::None).await.is_none(),
        "an unaccepted declared MIME type must stay rejected with no predicate configured"
    );
    assert!(
        crawl_body("application/pdf", b"%PDF-1.7 body", Filter::None)
            .await
            .is_some(),
        "an accepted declared MIME type must stay accepted with no predicate configured"
    );
}

/// The predicate's third argument carries the built-in decision, so a caller can widen it
/// instead of reimplementing `document_mime_types` matching.
#[tokio::test]
async fn should_pass_the_declared_mime_decision_to_the_filter_so_it_can_widen_it() {
    let widened = crawl_body("text/plain", b"%PDF-1.7 body", Filter::DeclaredMimeOrBytes).await;
    assert!(
        widened.is_some(),
        "the byte check must admit a PDF body served as text/plain"
    );
    let by_declared_mime = crawl_body("application/pdf", b"not a PDF", Filter::DeclaredMimeOrBytes).await;
    assert!(
        by_declared_mime.is_some(),
        "the third argument must be true for an accepted declared MIME type, so the predicate \
         can keep the built-in decision without reimplementing it"
    );
}

/// `scrape()` builds its document record in `scrape_from_crawl_response`, which took no predicate
/// before, so a configured `document_filter` was silently ignored on this path.
#[tokio::test]
async fn should_apply_the_filter_on_a_plain_scrape() {
    let document = scrape_body("text/plain; charset=utf-8", b"%PDF-1.7 body", Filter::BytesOnly)
        .await
        .expect("a scrape must honour the engine's document filter");
    assert_eq!(&*document.mime_type, "text/plain");
    assert_eq!(document.content.as_slice(), b"%PDF-1.7 body");
}

/// The other direction on the `scrape()` path: the predicate must be able to reject a body whose
/// declared MIME type the built-in decision accepts.
#[tokio::test]
async fn should_reject_disallowed_bytes_on_a_plain_scrape() {
    let document = scrape_body("application/pdf", b"not a PDF", Filter::BytesOnly).await;
    assert!(
        document.is_none(),
        "a scrape must let the engine's document filter reject an allowed declared MIME type"
    );
}
