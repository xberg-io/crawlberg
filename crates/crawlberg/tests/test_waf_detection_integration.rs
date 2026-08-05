//! Integration tests for WAF/bot-protection detection via actual HTTP responses.

use std::sync::OnceLock;

use crawlberg::{BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

/// Opts into the SSRF policy's private-network allowance so wiremock's
/// 127.0.0.1 servers are reachable. Without this, `create_engine`'s SSRF
/// check rejects the loopback URL before the WAF detection logic under
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

fn no_browser_config() -> CrawlConfig {
    allow_private_network();
    let mut config = CrawlConfig::default();
    config.browser.mode = BrowserMode::Never;
    config
}

async fn assert_waf_blocked(body: &str, headers: Vec<(&str, &str)>) {
    let mock = MockServer::start().await;

    let mut resp = ResponseTemplate::new(403);
    for (k, v) in headers {
        resp = resp.append_header(k, v);
    }
    resp = resp.set_body_string(body);

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(resp)
        .mount(&mock)
        .await;

    let handle = create_engine(Some(no_browser_config()))
        .expect("create_engine with no-browser config should succeed in integration test");
    let result = scrape(&handle, &mock.uri()).await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { .. })),
        "expected WafBlocked, got: {:?}",
        result
    );
}

#[tokio::test]
async fn test_cloudflare_detection() {
    assert_waf_blocked(
        "<html>cf-browser-verification challenge-form</html>",
        vec![("content-type", "text/html"), ("server", "cloudflare")],
    )
    .await;
}

#[tokio::test]
async fn test_imperva_detection() {
    assert_waf_blocked(
        "<html>Powered by Incapsula _incap_ses_</html>",
        vec![("content-type", "text/html")],
    )
    .await;
}

#[tokio::test]
async fn test_datadome_detection() {
    assert_waf_blocked(
        "<html><script src=\"dd.js\"></script>datadome</html>",
        vec![("content-type", "text/html")],
    )
    .await;
}

#[tokio::test]
async fn test_akamai_detection() {
    assert_waf_blocked(
        "<html>Access Denied</html>",
        vec![("content-type", "text/html"), ("server", "AkamaiGHost")],
    )
    .await;
}

/// Cloudflare's "Just a moment..." JS challenge ships with **200 OK** on
/// modern accounts, not 403. The body contains `cf-chl-` / `__cf_chl_`
/// fingerprints. The detector must classify this as `WafBlocked` so the
/// engine's `BrowserMode::Auto` fallback can retry via headless Chrome —
/// otherwise the challenge HTML is fed downstream as if it were content.
#[tokio::test]
async fn test_cloudflare_2xx_interstitial_detected() {
    let mock = MockServer::start().await;
    let challenge = r#"
        <!DOCTYPE html>
        <html><head><title>Just a moment...</title></head>
        <body>
            <noscript>This site requires JavaScript</noscript>
            <script src="/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1?ray=abc"></script>
            <div id="cf-chl-widget-abc"></div>
        </body></html>
    "#;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(challenge)
                .append_header("content-type", "text/html")
                .append_header("server", "cloudflare"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(no_browser_config()))
        .expect("create_engine with no-browser config should succeed in integration test");
    let result = scrape(&handle, &mock.uri()).await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { .. })),
        "2xx Cloudflare interstitial must be WafBlocked, got: {:?}",
        result
    );
}

/// DataDome JS challenge serves with 200 OK and an `x-datadome` header.
/// The header alone is enough to classify regardless of body content.
#[tokio::test]
async fn test_datadome_2xx_header_detected() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><script src=\"https://js.datadome.co/tags.js\"></script></html>")
                .append_header("content-type", "text/html")
                .append_header("x-datadome", "blocked"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(no_browser_config()))
        .expect("create_engine with no-browser config should succeed in integration test");
    let result = scrape(&handle, &mock.uri()).await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { .. })),
        "2xx with x-datadome header must be WafBlocked, got: {:?}",
        result
    );
}

/// PerimeterX challenge serves with 200 OK and `x-px-*` response headers.
#[tokio::test]
async fn test_perimeterx_2xx_header_detected() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html>px-captcha</html>")
                .append_header("content-type", "text/html")
                .append_header("x-px-block", "1"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(no_browser_config()))
        .expect("create_engine with no-browser config should succeed in integration test");
    let result = scrape(&handle, &mock.uri()).await;
    assert!(
        matches!(result, Err(CrawlError::WafBlocked { .. })),
        "2xx with x-px-* header must be WafBlocked, got: {:?}",
        result
    );
}

/// A legitimate page mentioning a WAF vendor in prose (e.g. a blog post
/// about Cloudflare) MUST NOT be classified as blocked. The page is well
/// over CHALLENGE_BODY_LIMIT and contains no JS challenge fingerprint.
#[tokio::test]
async fn test_2xx_legitimate_long_page_not_flagged() {
    let mock = MockServer::start().await;
    let mut body = String::from("<html><body><h1>How Cloudflare and DataDome work</h1>");
    body.push_str(&"<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit. </p>".repeat(4000));
    body.push_str("</body></html>");
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(&body)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(no_browser_config()))
        .expect("create_engine with no-browser config should succeed in integration test");
    let result = scrape(&handle, &mock.uri()).await;
    assert!(
        result.is_ok(),
        "legitimate long page mentioning vendors must NOT be blocked, got: {:?}",
        result
    );
}

/// DataDome 2xx interstitial as actually served by leboncoin.fr: a ~774-byte HTML
/// page with no `x-datadome` header, just an inline `var dd={...}` config object
/// referencing `geo.captcha-delivery.com` plus a `ct.captcha-delivery.com` script
/// src. Captured 2026-05-26 from staging API. Must be detected as DataDome.
#[tokio::test]
async fn test_datadome_2xx_body_leboncoin_detected() {
    let body = "<html lang=\"en\"><head><title>leboncoin.fr</title></head>\
                <body style=\"margin:0\"><p id=\"cmsg\">Please enable JS and disable any ad blocker</p>\
                <script data-cfasync=\"false\">var dd={'rt':'i','cid':'AHrlqAAAAAMAuqpSG8sDb8IAV7yaGA==',\
                'hsh':'05B30BD9055986BD2EE8F5A199D973','host':'geo.captcha-delivery.com'}</script>\
                <script data-cfasync=\"false\" src=\"https://ct.captcha-delivery.com/i.js\"></script>\
                </body></html>";

    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(no_browser_config()))
        .expect("create_engine with no-browser config should succeed in integration test");
    let result = scrape(&handle, &mock.uri()).await;
    let Err(CrawlError::WafBlocked { vendor, .. }) = result else {
        unreachable!("expected WafBlocked {{ datadome }}, got: {result:?}");
    };
    assert!(
        vendor.contains("datadome"),
        "expected vendor=datadome in WafBlocked vendor, got: {vendor}"
    );
}

/// A small (~1.5 KB) benign HTML page with no WAF tokens must not be classified
/// as a challenge. Guards against the broader captcha-delivery.com / ddjskey
/// fingerprints producing false positives on harmless pages.
#[tokio::test]
async fn test_small_benign_body_not_false_positive() {
    let mut body = String::from("<html><head><title>Hello</title></head><body><h1>Welcome</h1>");
    body.push_str(&"<p>This page is intentionally short and ordinary.</p>".repeat(20));
    body.push_str("</body></html>");
    assert!(
        body.len() < 4096,
        "test fixture must be small enough to exercise the small-body path: {} bytes",
        body.len()
    );

    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(&body)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(no_browser_config()))
        .expect("create_engine with no-browser config should succeed in integration test");
    let result = scrape(&handle, &mock.uri()).await;
    assert!(
        result.is_ok(),
        "small benign body must NOT be classified as WafBlocked, got: {:?}",
        result
    );
}

#[tokio::test]
async fn test_plain_403_is_not_waf() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_string("<html>Forbidden</html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(no_browser_config()))
        .expect("create_engine with no-browser config should succeed in integration test");
    let result = scrape(&handle, &mock.uri()).await;
    assert!(
        matches!(result, Err(CrawlError::Forbidden(_))),
        "plain 403 should be Forbidden, not WafBlocked: {:?}",
        result
    );
}
