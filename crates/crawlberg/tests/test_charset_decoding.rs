//! Regression tests: `crawl()` must apply the charset it detects, not just report it.
//!
//! `scrape_from_crawl_response` (used by `scrape()`) always correctly re-decoded the
//! body with the detected charset. `crawl()`'s per-page extraction computed the same
//! `detected_charset` but only threaded it through as a reported field, never
//! re-decoding — so non-UTF-8 pages (Windows-1252, Shift_JIS, UTF-16, …) were silently
//! corrupted into U+FFFD replacement characters in `html`, `metadata`, `links`, and
//! `markdown`, while `detected_charset` accurately reported the encoding the crawl
//! failed to apply.

use std::sync::OnceLock;

use crawlberg::{BrowserMode, CrawlConfig, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

/// Opts into the SSRF policy's private-network allowance so wiremock's
/// 127.0.0.1 servers are reachable. Without this, `create_engine`'s SSRF
/// check rejects the loopback URL before the charset-decoding path under
/// test is ever reached.
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

fn engine_with_config(mut config: CrawlConfig) -> crawlberg::CrawlEngineHandle {
    config.browser.mode = BrowserMode::Never;
    allow_private_network();
    create_engine(Some(config)).expect("engine build must not fail")
}

/// `crawl()` must decode a Windows-1252 page's `<title>` using the charset it detects
/// from the `Content-Type` header, not silently corrupt every non-ASCII byte into
/// U+FFFD while still *reporting* `detected_charset: Some("windows-1252")`.
#[tokio::test]
async fn crawl_applies_detected_windows_1252_charset() {
    let mock = MockServer::start().await;

    let title = "café €100 — déjà vu";
    let (encoded_title, _, had_errors) = encoding_rs::WINDOWS_1252.encode(title);
    assert!(!had_errors, "test fixture title must be representable in Windows-1252");
    let mut html_bytes = Vec::new();
    html_bytes.extend_from_slice(b"<html><head><title>");
    html_bytes.extend_from_slice(&encoded_title);
    html_bytes.extend_from_slice(b"</title></head><body>hello</body></html>");

    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html_bytes, "text/html; charset=windows-1252"))
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig::default());
    let url = format!("{}/page", mock.uri());
    let result = crawl(&handle, &url).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        1,
        "must have crawled exactly one page, got {:?}",
        result.pages
    );
    let page = &result.pages[0];

    assert_eq!(
        page.detected_charset.as_deref(),
        Some("windows-1252"),
        "detected_charset must report windows-1252, got {:?}",
        page.detected_charset
    );
    assert_eq!(
        page.metadata.title.as_deref(),
        Some(title),
        "title must be decoded exactly using the detected charset, got {:?}",
        page.metadata.title
    );
    assert!(
        !page.html.contains('\u{FFFD}'),
        "page.html must not contain U+FFFD replacement characters, got: {}",
        page.html
    );
}

/// Same regression for Shift_JIS — a variable-width, non-ASCII-superset encoding where
/// a naive UTF-8-lossy decode corrupts every non-ASCII character, unlike Windows-1252
/// where only accented/symbol bytes are affected.
#[tokio::test]
async fn crawl_applies_detected_shift_jis_charset() {
    let mock = MockServer::start().await;

    let title = "日本語のテスト";
    let (encoded_title, _, had_errors) = encoding_rs::SHIFT_JIS.encode(title);
    assert!(!had_errors, "test fixture title must be representable in Shift_JIS");
    let mut html_bytes = Vec::new();
    html_bytes.extend_from_slice(b"<html><head><title>");
    html_bytes.extend_from_slice(&encoded_title);
    html_bytes.extend_from_slice(b"</title></head><body>hello</body></html>");

    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html_bytes, "text/html; charset=shift_jis"))
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig::default());
    let url = format!("{}/page", mock.uri());
    let result = crawl(&handle, &url).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        1,
        "must have crawled exactly one page, got {:?}",
        result.pages
    );
    let page = &result.pages[0];

    assert_eq!(
        page.detected_charset.as_deref(),
        Some("shift_jis"),
        "detected_charset must report shift_jis, got {:?}",
        page.detected_charset
    );
    assert_eq!(
        page.metadata.title.as_deref(),
        Some(title),
        "title must be decoded exactly using the detected charset, got {:?}",
        page.metadata.title
    );
}

/// Encode `s` as raw UTF-16LE code units (no BOM). `encoding_rs::UTF_16LE::encode`
/// cannot be used for this: per the WHATWG encoding standard, UTF-16 has no defined
/// *output* encoding, so `Encoding::encode` silently substitutes UTF-8 for it — this
/// test needs genuine UTF-16LE bytes to reproduce the BOM-detection bug.
fn utf16le_bytes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2);
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// A UTF-16LE page (signaled only by its byte-order mark, no `charset=` anywhere) must
/// decode correctly. This is the specific knock-on bug: BOM detection previously ran
/// on an already-UTF-8-lossy `&str`, and the FF FE marker is not valid UTF-8 on its
/// own, so it was destroyed (replaced with U+FFFD) before detection ever saw it.
#[tokio::test]
async fn crawl_applies_detected_utf16le_bom_charset() {
    let mock = MockServer::start().await;

    let title = "hello world";
    let html = format!("<html><head><title>{title}</title></head><body>hello</body></html>");
    let mut html_bytes: Vec<u8> = vec![0xFF, 0xFE];
    html_bytes.extend_from_slice(&utf16le_bytes(&html));

    Mock::given(method("GET"))
        .and(path("/page"))
        // ~keep No charset in Content-Type: the BOM is the only signal available.
        .respond_with(ResponseTemplate::new(200).set_body_raw(html_bytes, "text/html"))
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig::default());
    let url = format!("{}/page", mock.uri());
    let result = crawl(&handle, &url).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        1,
        "must have crawled exactly one page, got {:?}",
        result.pages
    );
    let page = &result.pages[0];

    assert_eq!(
        page.detected_charset.as_deref(),
        Some("utf-16le"),
        "detected_charset must report utf-16le from the BOM, got {:?}",
        page.detected_charset
    );
    assert_eq!(
        page.metadata.title.as_deref(),
        Some(title),
        "title must be decoded exactly using the BOM-detected charset, got {:?}",
        page.metadata.title
    );
}
