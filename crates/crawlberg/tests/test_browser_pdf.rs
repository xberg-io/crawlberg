//! ~keep A PDF response must be classified before Chrome waits for a page load (crawlberg#609).
#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{BrowserConfig, BrowserMode, BrowserWait, CrawlConfig, create_engine, scrape};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PDF: &str = concat!(
    "%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n",
    "2 0 obj<</Type/Pages/Kids[]/Count 0>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n",
);

async fn assert_pdf_response(content_type: &str, wait: BrowserWait, redirected: bool, status: u16) {
    let site = MockServer::start().await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/document"))
        .respond_with(ResponseTemplate::new(status).set_body_raw(PDF, content_type))
        .mount(&site)
        .await;
    Mock::given(wiremock::matchers::path("/redirect"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/document"))
        .mount(&site)
        .await;
    let config = |mode| CrawlConfig {
        browser: BrowserConfig {
            mode,
            wait: wait.clone(),
            wait_selector: Some("#must-not-wait-for-a-pdf-viewer".to_owned()),
            extra_wait: Some(Duration::from_secs(12)),
            chrome_path: std::env::var_os("CRAWLBERG_TEST_CHROME_PATH").map(Into::into),
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        download_documents: false,
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let route = if redirected { "redirect" } else { "document" };
    let url = format!("{}/{route}", site.uri());
    let http = create_engine(Some(config(BrowserMode::Never))).expect("HTTP engine");
    let expected = scrape(&http, &url).await.expect("HTTP PDF classification");
    assert_eq!(expected.status_code, status);
    assert!(expected.is_pdf);
    let browser = create_engine(Some(config(BrowserMode::Always))).expect("browser engine");
    let actual = tokio::time::timeout(Duration::from_secs(8), scrape(&browser, &url))
        .await
        .expect("PDF classification must finish well before the 20-second navigation timeout")
        .expect("browser PDF classification");
    assert_eq!(actual.status_code, expected.status_code);
    assert_eq!(actual.is_pdf, expected.is_pdf);
    assert_eq!(actual.content_type, expected.content_type);
    assert!(actual.markdown.is_none());
    assert_eq!(actual.final_url, expected.final_url);
    assert!(actual.downloaded_document.is_none());
}

#[tokio::test]
async fn should_report_pdf_headers_before_the_browser_navigation_timeout() {
    assert_pdf_response("application/pdf", BrowserWait::NetworkIdle, false, 200).await;
}

#[tokio::test]
async fn should_skip_selector_wait_for_a_pdf_response_with_mixed_case_mime() {
    assert_pdf_response("Application/PDF; charset=binary", BrowserWait::Selector, false, 200).await;
}

#[tokio::test]
async fn should_report_the_pdf_response_after_an_http_redirect() {
    assert_pdf_response("application/pdf", BrowserWait::NetworkIdle, true, 200).await;
}

#[tokio::test]
async fn should_preserve_the_original_pdf_response_status() {
    assert_pdf_response("application/pdf", BrowserWait::NetworkIdle, false, 206).await;
}
