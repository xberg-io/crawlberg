//! Integration tests verifying that `scrape()` follows HTTP 3xx redirects,
//! Refresh headers, and meta-refresh directives, and that network errors
//! encountered during a redirect chain propagate with the correct tag.

use crawlberg::{CrawlConfig, CrawlError, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build an engine with tight timeouts and a small redirect limit.
fn engine_with_config(config: CrawlConfig) -> crawlberg::CrawlEngineHandle {
    create_engine(Some(config)).expect("engine build must not fail")
}

fn default_engine() -> crawlberg::CrawlEngineHandle {
    engine_with_config(CrawlConfig::builder().allow_private_networks(true).build())
}

/// scrape() on a 302-redirecting URL must transparently follow to the final
/// page and return its content with status 200.
#[tokio::test]
async fn scrape_follows_302_to_200() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/final")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/final"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Final page</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = default_engine();
    let url = format!("{}/start", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(
        result.is_ok(),
        "scrape should succeed after redirect: {:?}",
        result.err()
    );
    let page = result.unwrap();
    assert_eq!(page.status_code, 200, "final status must be 200");
    assert!(
        page.html.contains("Final page"),
        "response body should be from the final URL"
    );
}

/// scrape() must follow a chain of two 302 responses before reaching a 200.
#[tokio::test]
async fn scrape_follows_chain_302_302_200() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/hop1"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/hop2")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/hop2"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/destination")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/destination"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Destination</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = default_engine();
    let url = format!("{}/hop1", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(
        result.is_ok(),
        "scrape should succeed after redirect chain: {:?}",
        result.err()
    );
    let page = result.unwrap();
    assert_eq!(page.status_code, 200);
    assert!(page.html.contains("Destination"));
}

/// When the redirect chain forms a cycle (A→B→A), scrape() must stop without
/// looping and return the most recent 3xx response (not raise).
#[tokio::test]
async fn scrape_stops_on_redirect_cycle() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/a"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/b")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/b"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/a")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = default_engine();
    let url = format!("{}/a", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(
        result.is_ok(),
        "scrape must not raise on a redirect cycle: {:?}",
        result.err()
    );
    let page = result.unwrap();
    assert_eq!(page.status_code, 302, "final status must be the unfollowed 3xx");
}

/// When a redirect chain exceeds `max_redirects`, scrape() must stop and
/// return the last 3xx response (not raise).
#[tokio::test]
async fn scrape_stops_at_max_redirects() {
    let mock = MockServer::start().await;

    for i in 1..=3usize {
        Mock::given(method("GET"))
            .and(path(format!("/r{i}")))
            .respond_with(
                ResponseTemplate::new(302)
                    .append_header("location", format!("/r{}", i + 1))
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/r4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>OK</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let config = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_redirects: 2,
        ..config
    };
    let handle = engine_with_config(config);
    let url = format!("{}/r1", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(
        result.is_ok(),
        "scrape must not raise when max_redirects is exceeded: {:?}",
        result.err()
    );
    let page = result.unwrap();
    assert_eq!(page.status_code, 302, "final status must be the last unfollowed 3xx");
}

/// scrape() must populate `final_url` with the post-redirect URL, not the
/// original request URL. Closes GitHub issue #12 (partial: final_url field).
#[tokio::test]
async fn scrape_final_url_reflects_redirect_target() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/source"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/target")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/target"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Target</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = default_engine();
    let url = format!("{}/source", mock.uri());
    let result = scrape(&handle, &url).await.expect("scrape must succeed");

    assert!(
        result.final_url.contains("/target"),
        "final_url must contain '/target', got: {}",
        result.final_url
    );
    assert_eq!(result.status_code, 200);
}

/// scrape() on a page with no redirects must populate `final_url` equal to
/// the requested URL.
#[tokio::test]
async fn scrape_final_url_matches_request_url_when_no_redirect() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Page</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = default_engine();
    let url = format!("{}/page", mock.uri());
    let result = scrape(&handle, &url).await.expect("scrape must succeed");

    assert!(
        result.final_url.contains("/page"),
        "final_url must contain '/page', got: {}",
        result.final_url
    );
}

/// When a redirect points to an unreachable host (connection refused), scrape()
/// must propagate a `[network:connection]` error rather than silently succeeding.
#[tokio::test]
async fn scrape_propagates_network_error_through_redirect() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "http://127.0.0.1:1/unreachable")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let config = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        request_timeout: std::time::Duration::from_millis(500),
        ..config
    };
    let handle = engine_with_config(config);
    let url = format!("{}/", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(
        result.is_err(),
        "scrape must propagate network error from redirect target"
    );
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("[network:connection]"),
        "error must contain [network:connection], got: '{msg}'"
    );
    assert!(
        matches!(err, CrawlError::Connection(_)),
        "error must be CrawlError::Connection, got: {err:?}"
    );
}
