use std::sync::Arc;

use crawlberg::{CrawlConfig, CrawlEngineBuilder, CrawlEngineHandle, DownloadedDocument, StaticProxyProvider, crawl};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn crawl_body(content_type: &str, body: &[u8], use_filter: bool) -> Option<DownloadedDocument> {
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
    if use_filter {
        builder = builder.document_filter(|declared_mime_type, bytes: &[u8]| {
            bytes.starts_with(b"%PDF") || (declared_mime_type == "application/json" && bytes.starts_with(b"{"))
        });
    }
    let handle = CrawlEngineHandle::from_engine(builder.build().expect("engine builds"));
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

#[tokio::test]
async fn should_accept_pdf_bytes_despite_text_plain_header_with_filter() {
    let document = crawl_body("text/plain; charset=utf-8", b"%PDF-1.7 body", true)
        .await
        .expect("byte-aware policy accepts PDF");
    assert_eq!(&*document.mime_type, "text/plain");
    assert_eq!(document.content.as_slice(), b"%PDF-1.7 body");
}

#[tokio::test]
async fn should_accept_explicit_json_alongside_pdf_with_filter() {
    let document = crawl_body("application/json", br#"{"ok":true}"#, true)
        .await
        .expect("byte-aware policy accepts JSON");
    assert_eq!(&*document.mime_type, "application/json");
    assert_eq!(document.content.as_slice(), br#"{"ok":true}"#);
}

#[tokio::test]
async fn should_reject_disallowed_bytes_with_filter() {
    let document = crawl_body("application/pdf", b"not a PDF", true).await;
    assert!(
        document.is_none(),
        "byte-aware policy rejects disallowed bytes even under an allowed header"
    );
}

#[tokio::test]
async fn should_preserve_declared_mime_filter_without_hook() {
    assert!(crawl_body("text/plain", b"%PDF-1.7 body", false).await.is_none());
    assert!(crawl_body("application/pdf", b"%PDF-1.7 body", false).await.is_some());
}
