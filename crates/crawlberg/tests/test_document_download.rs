//! Integration tests: the multi-page crawl loop materializes discovered
//! non-HTML documents (PDF, …) into `CrawlPageResult.downloaded_document`,
//! while plain HTML pages leave that field `None`.

use std::sync::OnceLock;

use crawlberg::{CrawlConfig, batch_crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal PDF payload — the leading `%PDF-` magic plus a trailing `%%EOF`.
const PDF_BYTES: &[u8] = b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF";

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

/// Opts into the SSRF policy's private-network allowance so wiremock's
/// 127.0.0.1 servers are reachable. Without this, the engine's SSRF check
/// rejects the loopback URL before the document-download behaviour under test runs.
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

#[tokio::test]
async fn crawl_loop_downloads_linked_pdf_document() {
    allow_private_network();
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
    allow_private_network();
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
    allow_private_network();
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

#[tokio::test]
async fn crawl_loop_truncates_oversized_document_but_reports_true_size() {
    allow_private_network();
    let mock = MockServer::start().await;

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
        max_depth: Some(0),
        download_documents: true,
        document_max_size: Some(4),
        ..Default::default()
    };
    let handle = create_engine(Some(config)).unwrap();

    let results = batch_crawl(&handle, vec![format!("{}/paper.pdf", mock.uri())])
        .await
        .expect("batch_crawl should succeed");

    let crawl = results.results[0].result.as_ref().expect("crawl should succeed");
    let doc = crawl.pages[0]
        .downloaded_document
        .as_ref()
        .expect("an over-cap PDF must still produce a truncated downloaded_document");

    assert_eq!(doc.content.len(), 4, "content must be capped at document_max_size");
    assert_eq!(
        doc.size,
        PDF_BYTES.len(),
        "size must report the true original length, not the truncated content length"
    );
    assert!(doc.truncated, "an over-cap document must be flagged truncated");
}

#[tokio::test]
async fn crawl_loop_bounds_the_network_read_for_an_oversized_document_at_fetch_time() {
    allow_private_network();
    let mock = MockServer::start().await;

    // ~keep Large enough that "fully materialized, then truncated" (the pre-fix
    // behavior) and "bounded at fetch time" (this test's assertion) are trivially
    // distinguishable by `doc.size` alone: the served body is ~1000x document_max_size.
    let oversized_body = vec![b'A'; 1_000_000];

    Mock::given(method("GET"))
        .and(path("/huge.pdf"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(oversized_body.clone())
                .append_header("content-type", "application/pdf"),
        )
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        max_depth: Some(0),
        download_documents: true,
        document_max_size: Some(1024),
        // ~keep max_body_size deliberately left at its default `None` (unbounded) — this
        // is exactly the scenario where, without the fetch-time bound in
        // `engine::crawl_loop::run_crawl_loop`, the full 1MB body would be read into
        // memory regardless of `document_max_size`.
        ..Default::default()
    };
    let handle = create_engine(Some(config)).unwrap();

    let results = batch_crawl(&handle, vec![format!("{}/huge.pdf", mock.uri())])
        .await
        .expect("batch_crawl should succeed");

    let crawl = results.results[0].result.as_ref().expect("crawl should succeed");
    let doc = crawl.pages[0]
        .downloaded_document
        .as_ref()
        .expect("an oversized PDF must still produce a truncated downloaded_document");

    assert_eq!(doc.content.len(), 1024, "content must be capped at document_max_size");
    assert!(doc.truncated, "an over-cap document must be flagged truncated");
    assert!(
        doc.size < oversized_body.len(),
        "size ({}) must be far below the full served body ({}) — proving the network \
         read itself was bounded at fetch time, not just truncated after full materialization",
        doc.size,
        oversized_body.len()
    );
}

#[tokio::test]
async fn crawl_loop_streams_document_to_output_dir() {
    allow_private_network();
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/paper.pdf"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(PDF_BYTES)
                .append_header("content-type", "application/pdf"),
        )
        .mount(&mock)
        .await;

    let output_dir = std::env::temp_dir().join(format!("crawlberg-document-output-dir-test-{}", std::process::id()));

    let config = CrawlConfig {
        max_depth: Some(0),
        download_documents: true,
        document_output_dir: Some(output_dir.clone()),
        ..Default::default()
    };
    let handle = create_engine(Some(config)).unwrap();

    let results = batch_crawl(&handle, vec![format!("{}/paper.pdf", mock.uri())])
        .await
        .expect("batch_crawl should succeed");

    let crawl = results.results[0].result.as_ref().expect("crawl should succeed");
    let doc = crawl.pages[0]
        .downloaded_document
        .as_ref()
        .expect("a PDF must still produce a downloaded_document when streaming to disk");

    assert!(
        doc.content.is_empty(),
        "content must be cleared in-memory once streamed to document_output_dir"
    );
    let path = doc.content_path.as_deref().expect("content_path must be populated");
    let written = std::fs::read(path).expect("the streamed file must exist on disk");
    assert_eq!(
        written, PDF_BYTES,
        "the streamed file must hold the full document bytes"
    );

    std::fs::remove_dir_all(&output_dir).ok();
}
