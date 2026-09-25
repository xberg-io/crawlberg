//! Regression coverage for crawlberg#60: `allow_subdomains` had no effect because every
//! cross-host link — including a followable subdomain — was classified `LinkType::External`
//! by `classify_link` and dropped by a link-type gate that ran *before* the domain-scope
//! check that actually knows about `allow_subdomains`. The same gate also dropped every
//! cross-host link when `stay_on_domain` was `false`, even though that setting means no host
//! restriction applies at all.
//!
//! ~keep The "rejected" cases below use a syntactically valid but unregistered hostname
//! (`*.example.invalid`, reserved by RFC 2606). No DNS lookup ever happens for them: a link
//! rejected by scope is dropped in `collect_link_candidates` before SSRF validation (which is
//! the step that resolves DNS), so these tests need no network access at all.
//!
//! ~keep The "admitted" cases need a real, reachable second host to prove the crawl actually
//! follows the link (not just that it isn't rejected). They use `*.localhost`, which RFC 6761
//! §6.3 requires every conformant resolver to resolve to the loopback address without any
//! network traffic — unlike a public DNS trick (e.g. nip.io), no external DNS is queried. This
//! is still a resolver-behavior dependency in principle; if a CI resolver stack does not honor
//! RFC 6761 for `*.localhost`, these two tests (and only these two) would need isolating
//! further. See the fix's report for this caveat.

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
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="https://sub.example.invalid/child">sub</a></body></html>"#,
    )
    .await;

    let base = mock.uri();
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
}

/// An unrelated host must never be followed while `stay_on_domain` is true, regardless of
/// `allow_subdomains`.
#[tokio::test]
async fn should_reject_unrelated_host_even_when_allow_subdomains_is_true() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="https://other.example.invalid/child">out</a></body></html>"#,
    )
    .await;

    let base = mock.uri();
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
        &format!(r#"<html><body><a href="http://unrelated.invalid:{port}/child">out</a></body></html>"#),
    )
    .await;

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
}
