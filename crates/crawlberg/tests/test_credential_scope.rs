//! Regression coverage for crawlberg#387/#388/#390: configured `AuthConfig` credentials
//! were attached to every request a crawl made, with no check that the request's host was
//! the one they were configured for. A crawl that follows a subdomain (`allow_subdomains`)
//! or is redirected off the seed host sent the caller's Bearer token or Basic credentials
//! to that other host too.
//!
//! ~keep Uses a fake, obviously-not-real token (`FAKE_TOKEN`) and asserts only on its
//! presence/absence via a header matcher — never logs or prints the header value itself.
//!
//! ~keep `through_fixture`'s single `MockServer` stands in for two distinct hosts (the seed
//! and its subdomain) by proxying every request to it regardless of target host; mocks
//! match on path plus a header matcher, mirroring `test_subdomain_scope.rs`'s technique for
//! making a genuinely different host reachable without touching DNS or `/etc/hosts`.

use crawlberg::{AuthConfig, CrawlConfig, CrawlEngine, ProxyConfig};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const FAKE_TOKEN: &str = "test-fixture-bearer-token-not-a-real-secret";

/// Matches a request that does NOT carry an `Authorization` header — the mirror image of
/// `wiremock::matchers::header_exists`, needed so a mock can distinguish "credentials were
/// withheld" from "credentials were sent" on the very same path.
struct NoAuthorizationHeader;

impl Match for NoAuthorizationHeader {
    fn matches(&self, request: &Request) -> bool {
        !request.headers.contains_key("authorization")
    }
}

fn engine_with(config: CrawlConfig) -> CrawlEngine {
    CrawlEngine::builder()
        .config(config)
        .build()
        .expect("engine must build")
}

/// Route every request through the fixture server, so a fake subdomain host is reachable
/// without DNS. See `test_subdomain_scope.rs` for the same technique in full detail.
fn through_fixture(mock: &MockServer, config: CrawlConfig) -> CrawlConfig {
    CrawlConfig {
        ssrf: crawlberg::SsrfPolicy {
            deny_private: false,
            allowlist: vec![crawlberg::HostMatcher::suffix("localhost")],
            ..crawlberg::SsrfPolicy::default()
        },
        proxy: Some(ProxyConfig {
            url: mock.uri(),
            username: None,
            password: None,
        }),
        ..config
    }
}

/// Mount `at`, requiring the `Authorization` header, expecting exactly `expected` requests.
async fn mount_requiring_auth_header(mock: &MockServer, at: &str, body: &str, expected: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .and(header_exists("authorization"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .expect(expected)
        .mount(mock)
        .await;
}

/// Mount `at`, requiring the ABSENCE of the `Authorization` header, expecting exactly
/// `expected` requests.
async fn mount_requiring_no_auth_header(mock: &MockServer, at: &str, body: &str, expected: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .and(NoAuthorizationHeader)
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .expect(expected)
        .mount(mock)
        .await;
}

/// #387: a crawl that follows a subdomain link (`allow_subdomains: true`) must keep
/// configured credentials on the seed host and withhold them from the subdomain, even
/// though the subdomain page is genuinely fetched (it is in the crawl's scope; only the
/// credentials are out of the subdomain's scope).
#[tokio::test]
async fn crawl_withholds_configured_credentials_from_a_subdomain_it_follows() {
    let mock = MockServer::start().await;
    let port = mock.address().port();

    mount_requiring_auth_header(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://sub.foo.localhost:{port}/child">sub</a></body></html>"#),
        1,
    )
    .await;
    mount_requiring_no_auth_header(&mock, "/", "unreachable", 0).await;

    mount_requiring_no_auth_header(&mock, "/child", "<html><body>leaf</body></html>", 1).await;
    mount_requiring_auth_header(&mock, "/child", "unreachable", 0).await;

    let base = format!("http://foo.localhost:{port}");
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            allow_subdomains: true,
            respect_robots_txt: false,
            auth: Some(AuthConfig::Bearer {
                token: FAKE_TOKEN.to_owned(),
            }),
            ..CrawlConfig::default()
        },
    ));

    let result = engine.crawl(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        2,
        "both the seed and its subdomain must be fetched, got pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
    drop(mock);
}
