//! Integration tests: the multi-page crawl loop materializes discovered
//! non-HTML documents (PDF, …) into `CrawlPageResult.downloaded_document`,
//! while plain HTML pages leave that field `None`.

use crawlberg::{CrawlConfig, batch_crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal PDF payload — the leading `%PDF-` magic plus a trailing `%%EOF`.
const PDF_BYTES: &[u8] = b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF";

#[tokio::test]
async fn crawl_loop_downloads_linked_pdf_document() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/index.html"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><a href=\"/paper.pdf\">paper</a></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/paper.pdf"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(PDF_BYTES)
                .append_header("content-type", "application/pdf"),
        )
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(10),
        download_documents: true,
        ..Default::default()
    };
    let handle = create_engine(Some(config)).unwrap();

    let results = batch_crawl(&handle, vec![format!("{}/index.html", mock.uri())])
        .await
        .expect("batch_crawl should succeed");

    let crawl = results.results[0].result.as_ref().expect("seed crawl should succeed");

    let pdf_page = crawl
        .pages
        .iter()
        .find(|p| p.url.ends_with("/paper.pdf"))
        .expect("the linked PDF should have been crawled as a page");

    assert!(pdf_page.is_pdf, "the PDF page should be flagged is_pdf");
    let doc = pdf_page
        .downloaded_document
        .as_ref()
        .expect("the crawl loop must populate downloaded_document for a PDF");
    assert_eq!(
        doc.content.as_slice(),
        PDF_BYTES,
        "downloaded bytes must match the served PDF"
    );
    assert_eq!(&*doc.mime_type, "application/pdf");
    assert_eq!(doc.size, PDF_BYTES.len());
    assert_eq!(doc.filename.as_deref(), Some("paper.pdf"));
}

#[tokio::test]
async fn crawl_loop_leaves_html_pages_without_downloaded_document() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/page.html"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>plain text page</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        max_depth: Some(0),
        download_documents: true,
        ..Default::default()
    };
    let handle = create_engine(Some(config)).unwrap();

    let results = batch_crawl(&handle, vec![format!("{}/page.html", mock.uri())])
        .await
        .expect("batch_crawl should succeed");

    let crawl = results.results[0].result.as_ref().expect("crawl should succeed");
    assert_eq!(crawl.pages.len(), 1);
    assert!(
        crawl.pages[0].downloaded_document.is_none(),
        "a plain HTML page must not produce a downloaded_document"
    );
}

#[tokio::test]
async fn crawl_loop_skips_document_download_when_disabled() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/index.html"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><a href=\"/paper.pdf\">paper</a></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/paper.pdf"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(PDF_BYTES)
                .append_header("content-type", "application/pdf"),
        )
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(10),
        download_documents: false,
        ..Default::default()
    };
    let handle = create_engine(Some(config)).unwrap();

    let results = batch_crawl(&handle, vec![format!("{}/index.html", mock.uri())])
        .await
        .expect("batch_crawl should succeed");

    let crawl = results.results[0].result.as_ref().expect("crawl should succeed");
    let pdf_page = crawl
        .pages
        .iter()
        .find(|p| p.url.ends_with("/paper.pdf"))
        .expect("the linked PDF should still be crawled as a page");
    assert!(
        pdf_page.downloaded_document.is_none(),
        "download_documents=false must leave downloaded_document None"
    );
}
