//! Regression coverage for crawlberg#60: `allow_subdomains` had no effect because every
//! cross-host link — including a followable subdomain — was classified `LinkType::External`
//! by `classify_link` and dropped by a link-type gate that ran *before* the domain-scope
//! check that actually knows about `allow_subdomains`.
//!
//! ~keep `stay_on_domain` governs `LinkType::Document` links only (a cross-host `.pdf` etc.)
//! and defaults to `false`, meaning such links ARE followed by default — see the doc comment
//! on `host_in_scope` in `link_scope.rs`. It is not a no-op and must not be treated as one;
//! `should_follow_a_cross_host_document_link_by_default` and
//! `should_reject_a_cross_host_document_link_when_stay_on_domain_is_true` below cover it at
//! the loop level.
//!
//! ~keep The "rejected" cases below now use a real, reachable `*.localhost` host for the
//! rejected link, not an unresolvable `.invalid` one. An unresolvable host made "not fetched"
//! ambiguous between "the scope gate rejected it" and "DNS failed" — both render identically
//! as an absent page, so the assertion could not tell a working gate from a gutted one
//! (proven by mutation: hardwiring `allow_subdomains: true` at both call sites left every test
//! in this file green). RFC 6761 §6.3 requires every conformant resolver to resolve
//! `*.localhost` to the loopback address without any network traffic, so reachability does not
//! cost real DNS. Each rejected endpoint is mounted with `.expect(0)`, verified on drop, so the
//! test fails if the request is ever sent — the request-count check, not `pages.len()` alone,
//! is what proves the gate fired.

use crawlberg::{CrawlConfig, CrawlEngine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mount_html(mock: &MockServer, at: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

/// Mount `at`, asserting on drop that it is requested exactly `expected` times. Used to prove
/// a rejected link's target was never fetched, rather than inferring rejection from page count.
async fn mount_html_expecting(mock: &MockServer, at: &str, body: &str, expected: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .expect(expected)
        .mount(mock)
        .await;
}

fn engine_with(config: CrawlConfig) -> CrawlEngine {
    CrawlEngine::builder()
        .config(config)
        .build()
        .expect("engine must build")
}

fn permissive(config: CrawlConfig) -> CrawlConfig {
    CrawlConfig {
        ssrf: crawlberg::SsrfPolicy {
            deny_private: false,
            ..crawlberg::SsrfPolicy::default()
        },
        ..config
    }
}

/// A subdomain link must be followed when `stay_on_domain` and `allow_subdomains` are both
/// true — the exact setting crawlberg#60 reported as inert.
#[tokio::test]
async fn should_follow_subdomain_link_when_allow_subdomains_is_true() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://sub.localhost:{port}/child">sub</a></body></html>"#),
    )
    .await;
    mount_html(&mock, "/child", "<html><body>leaf</body></html>").await;

    let base = format!("http://localhost:{port}");
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        allow_subdomains: true,
        respect_robots_txt: false,
        ..CrawlConfig::default()
    }));

    let result = engine.crawl(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        2,
        "the subdomain link must be followed when allow_subdomains is true, got pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
}

/// The same subdomain link must NOT be followed when `allow_subdomains` is false.
#[tokio::test]
async fn should_reject_subdomain_link_when_allow_subdomains_is_false() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://sub.foo.localhost:{port}/child">sub</a></body></html>"#),
    )
    .await;
    mount_html_expecting(&mock, "/child", "<html><body>leaf</body></html>", 0).await;

    let base = format!("http://foo.localhost:{port}");
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        allow_subdomains: false,
        respect_robots_txt: false,
        ..CrawlConfig::default()
    }));

    let result = engine.crawl(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        1,
        "a subdomain link must not be followed when allow_subdomains is false, got pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
    // ~keep The `.expect(0)` on /child is the real assertion; it is verified on drop and
    // fails if the scope gate ever lets the request through.
    drop(mock);
}

/// An unrelated host must never be followed while `stay_on_domain` is true, regardless of
/// `allow_subdomains`.
#[tokio::test]
async fn should_reject_unrelated_host_even_when_allow_subdomains_is_true() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://bar.localhost:{port}/child">out</a></body></html>"#),
    )
    .await;
    mount_html_expecting(&mock, "/child", "<html><body>leaf</body></html>", 0).await;

    let base = format!("http://foo.localhost:{port}");
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        allow_subdomains: true,
        respect_robots_txt: false,
        ..CrawlConfig::default()
    }));

    let result = engine.crawl(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        1,
        "an unrelated host must never be followed, got pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
    drop(mock);
}

/// An unrelated host is never enqueued by a default-configured crawl.
///
/// ~keep Pins the additive contract of the crawlberg#60 fix: fixing `allow_subdomains` must not
/// widen a default crawl past the seed host and its subdomains. The host is deliberately
/// unresolvable (`.invalid`, RFC 2606) because scope rejects it before SSRF resolves anything,
/// so this needs no DNS. See crawlberg#72 for the `stay_on_domain` question.
#[tokio::test]
async fn should_reject_an_unrelated_host_by_default() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://bar.localhost:{port}/child">out</a></body></html>"#),
    )
    .await;
    mount_html_expecting(&mock, "/child", "<html><body>leaf</body></html>", 0).await;

    let base = format!("http://localhost:{port}");
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        respect_robots_txt: false,
        ..CrawlConfig::default()
    }));

    let result = engine.crawl(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        1,
        "an unrelated host must not be followed, got pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
    drop(mock);
}

async fn mount_pdf(mock: &MockServer, at: &str, expected: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"%PDF-1.4".to_vec())
                .append_header("content-type", "application/pdf"),
        )
        .expect(expected)
        .mount(mock)
        .await;
}

/// A cross-host document link (`.pdf`) on the seed page IS requested by a default-configured
/// crawl: `stay_on_domain` defaults to `false`, and `host_in_scope` treats `Document` links as
/// exempt from the host restriction in that case (`link_scope.rs`).
#[tokio::test]
async fn should_follow_a_cross_host_document_link_by_default() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://other.localhost:{port}/report.pdf">pdf</a></body></html>"#),
    )
    .await;
    mount_pdf(&mock, "/report.pdf", 1).await;

    let base = format!("http://localhost:{port}");
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        respect_robots_txt: false,
        ..CrawlConfig::default()
    }));

    engine.crawl(&base).await.expect("crawl must succeed");

    // ~keep The `.expect(1)` on /report.pdf is the real assertion; it is verified on drop.
    drop(mock);
}

/// The same cross-host document link must NOT be requested once `stay_on_domain` is `true`.
#[tokio::test]
async fn should_reject_a_cross_host_document_link_when_stay_on_domain_is_true() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://other.localhost:{port}/report.pdf">pdf</a></body></html>"#),
    )
    .await;
    mount_pdf(&mock, "/report.pdf", 0).await;

    let base = format!("http://localhost:{port}");
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        respect_robots_txt: false,
        stay_on_domain: true,
        ..CrawlConfig::default()
    }));

    engine.crawl(&base).await.expect("crawl must succeed");

    drop(mock);
}
