//! Native-only tests for the sequential (wasm) crawl loop.
//!
//! ~keep A sibling file reached through `#[path]` rather than an inline `mod tests`: the loop
//! and its coverage together pushed `wasm_crawl.rs` past the 1000-line limit, and the tests are
//! the part that keeps growing. `#[cfg(any(target_arch = "wasm32", test))]` on the parent means
//! this runs under a plain `cargo test`, with no wasm toolchain.

use super::*;
use crate::types::CrawlConfig;
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
/// a rejected link's target was never fetched, rather than inferring rejection from page count
/// alone — an unresolvable host makes "not fetched" ambiguous between "scope rejected it" and
/// "DNS failed" (proven by mutation: hardwiring `allow_subdomains: true` left these green).
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

/// Root links to `/a`, `/b`, `/c` in that order; each child links to one grandchild.
async fn branching_site() -> MockServer {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/a">A</a><a href="/b">B</a><a href="/c">C</a></body></html>"#,
    )
    .await;
    for child in ["a", "b", "c"] {
        mount_html(
            &mock,
            &format!("/{child}"),
            &format!(r#"<html><body><a href="/{child}1">{child}1</a></body></html>"#),
        )
        .await;
        mount_html(
            &mock,
            &format!("/{child}1"),
            &format!("<html><body>leaf {child}1</body></html>"),
        )
        .await;
    }
    mock
}

fn engine_with(config: CrawlConfig) -> CrawlEngine {
    CrawlEngine::builder()
        .config(config)
        .build()
        .expect("engine must build")
}

fn permissive(config: CrawlConfig) -> CrawlConfig {
    CrawlConfig {
        ssrf: crate::net::SsrfPolicy {
            deny_private: false,
            ..crate::net::SsrfPolicy::default()
        },
        ..config
    }
}

/// Route every request through the fixture server, used as a plain HTTP proxy, so these tests
/// never ask the system resolver for a `*.localhost` name.
///
/// ~keep macOS resolves `localhost` but not its subdomains, and the SSRF pre-check resolves every
/// host it does not allowlist, even with `deny_private` off. The proxy carries each request to the
/// fixture server by address, and the `localhost` suffix allowlist entry lets the pre-check permit
/// those names without a lookup. The fixture server matches on the path alone, so every host name
/// reaches the same mocks, and a rejected link's `.expect(0)` mock would see the request if the
/// scope gate ever let it through.
/// ~keep Setting `proxy` also suppresses the `PolicyResolver` DNS pinning that `build_client`
/// ~keep otherwise installs (`http/client.rs`, gated on `proxy_provider.is_none() &&
/// ~keep proxy.is_none()`), because hyper then resolves the proxy host rather than the target.
/// ~keep So these tests no longer exercise the SSRF DNS-pinning path they used to; the
/// ~keep allowlisted `validate_url` pre-check above is the only SSRF enforcement left in them.
/// ~keep Coverage for the pinning itself lives in `build_client`'s own tests
/// ~keep (`build_client_enforces_the_ssrf_policy_during_dns_resolution` and
/// ~keep `build_client_skips_the_policy_resolver_when_a_proxy_is_configured`).
fn through_fixture(mock: &MockServer, config: CrawlConfig) -> CrawlConfig {
    CrawlConfig {
        ssrf: crate::net::SsrfPolicy {
            deny_private: false,
            allowlist: vec![crate::net::HostMatcher::suffix("localhost")],
            ..crate::net::SsrfPolicy::default()
        },
        proxy: Some(crate::types::ProxyConfig {
            url: mock.uri(),
            username: None,
            password: None,
        }),
        ..config
    }
}

fn visited(result: &CrawlResult, base: &str) -> Vec<String> {
    result
        .pages
        .iter()
        .map(|page| match page.url.strip_prefix(base).unwrap_or(&page.url) {
            "" => "/".to_owned(),
            rest => rest.to_owned(),
        })
        .collect()
}

/// The sequential loop visits one depth level before the next, in document order.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_visits_breadth_first() {
    let mock = branching_site().await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(2),
        max_pages: Some(4),
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned(), "/a".to_owned(), "/b".to_owned(), "/c".to_owned()],
        "the whole depth-1 level must be visited before any depth-2 page"
    );
}

/// `max_depth` bounds how far links are followed, not just how many pages are kept.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_stops_following_links_at_max_depth() {
    let mock = branching_site().await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned(), "/a".to_owned(), "/b".to_owned(), "/c".to_owned()],
        "no grandchild may be reached at max_depth = 1"
    );
}

/// `max_links_per_page` caps how many links one page may enqueue.
///
/// ~keep The cap counts links actually *enqueued*, not raw anchors examined; a page
/// ~keep whose first anchors are external must still discover the eligible ones behind
/// ~keep them.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_caps_links_enqueued_per_page() {
    let mock = branching_site().await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        max_links_per_page: Some(2),
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned(), "/a".to_owned(), "/b".to_owned()],
        "only the first two eligible links of the root may be enqueued"
    );
}

/// An excluded path is filtered out before it is fetched.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_drops_excluded_paths() {
    let mock = branching_site().await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        exclude_paths: vec!["^/b$".to_owned()],
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned(), "/a".to_owned(), "/c".to_owned()],
        "`/b` matches exclude_paths and must never be fetched"
    );
}

/// A pattern matching only the query string does not exclude a link by default:
/// `exclude_paths` matches `path()` alone unless `path_patterns_match_query` is set.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_ignores_query_in_exclude_paths_by_default() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<html><body><a href="/blog?p=42">Post</a></body></html>"#).await;
    mount_html(&mock, "/blog", "<html><body>post</body></html>").await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        exclude_paths: vec![r"\?p=\d+".to_owned()],
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned(), "/blog?p=42".to_owned()],
        "path-only matching must not see the query string, so /blog?p=42 must still be fetched"
    );
}

/// With `path_patterns_match_query` on, a query-only exclude pattern now matches.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_excludes_by_query_when_match_query_is_enabled() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<html><body><a href="/blog?p=42">Post</a></body></html>"#).await;
    mount_html(&mock, "/blog", "<html><body>post</body></html>").await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        exclude_paths: vec![r"\?p=\d+".to_owned()],
        path_patterns_match_query: true,
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned()],
        "with path_patterns_match_query on, /blog?p=42 must be excluded"
    );
}

/// The dedup key drops the query by default, so `?id=1` and `?id=2` collapse to one page.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_collapses_distinct_queries_by_default() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/item?id=1">1</a><a href="/item?id=2">2</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/item", "<html><body>item</body></html>").await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        2,
        "/item?id=1 and /item?id=2 must collapse to a single dedup key by default, got: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
}

/// With `dedup_include_query` on, distinct queries are fetched as distinct pages.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_fetches_both_queries_when_dedup_include_query_is_enabled() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/item?id=1">1</a><a href="/item?id=2">2</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/item", "<html><body>item</body></html>").await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        dedup_include_query: true,
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        3,
        "/item?id=1 and /item?id=2 must both be fetched as distinct pages, got: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
}

/// Tracking parameters are stripped from the fetched and reported URL, not just the
/// dedup key.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_strips_tracking_params_from_fetched_and_reported_url() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/promo?utm_source=newsletter">Promo</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/promo", "<html><body>promo</body></html>").await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        strip_tracking_params: true,
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert!(
        urls.iter().any(|u| u.ends_with("/promo")),
        "/promo?utm_source=newsletter must be fetched and reported as /promo, got: {urls:?}"
    );
    assert!(
        !urls.iter().any(|u| u.contains("utm_source")),
        "utm_source must not survive into the reported URL, got: {urls:?}"
    );
}

/// Regression coverage for crawlberg#60 on the sequential (wasm) loop: a subdomain link
/// must be followed when `allow_subdomains` is true.
///
/// ~keep Uses `*.localhost`, not a fabricated hostname: this positive case needs a real,
/// reachable second host to prove the link is actually followed rather than merely not
/// rejected. `through_fixture` routes it to the fixture server, so no resolver is involved,
/// unlike a public-DNS trick such as nip.io.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_follows_subdomain_link_when_allow_subdomains_is_true() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://sub.localhost:{port}/a">A</a></body></html>"#),
    )
    .await;
    mount_html(&mock, "/a", "<html><body>a</body></html>").await;
    let base = format!("http://localhost:{port}");
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            allow_subdomains: true,
            ..CrawlConfig::default()
        },
    ));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        2,
        "a subdomain link must be followed when allow_subdomains is true, got pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
}

/// The same subdomain link must NOT be followed when `allow_subdomains` is false.
///
/// ~keep Uses a reachable host with `.expect(0)` on the child path, not a fabricated `.invalid`
/// one: an unreachable host makes "no page fetched" ambiguous between "scope rejected it" and
/// "the fetch failed anyway", so it cannot tell a working gate from a gutted one. The name ends
/// in `localhost` to match `through_fixture`'s suffix allowlist, not because the OS resolves it
/// -- the proxy is what makes it reachable.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_rejects_subdomain_link_when_allow_subdomains_is_false() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://sub.foo.localhost:{port}/a">A</a></body></html>"#),
    )
    .await;
    mount_html_expecting(&mock, "/a", "<html><body>a</body></html>", 0).await;
    let base = format!("http://foo.localhost:{port}");
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            allow_subdomains: false,
            ..CrawlConfig::default()
        },
    ));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        1,
        "a subdomain link must not be followed when allow_subdomains is false, got pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
    // ~keep The `.expect(0)` on /a is the real assertion; it is verified on drop.
    drop(mock);
}

/// An unrelated host is never enqueued by a default-configured crawl.
///
/// ~keep This pins the additive contract of the crawlberg#60 fix. Uses a reachable sibling host
/// with `.expect(0)` on the child path, not an unresolvable `.invalid` one: "no page fetched" is
/// ambiguous between "scope rejected it" and "the fetch failed anyway" for a host that cannot be
/// reached. The name ends in `localhost` to match `through_fixture`'s suffix allowlist, not
/// because the OS resolves it. `stay_on_domain` is not an input -- see
/// `link_scope::host_in_scope` and crawlberg#72.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_rejects_an_unrelated_host_by_default() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://bar.localhost:{port}/a">A</a></body></html>"#),
    )
    .await;
    mount_html_expecting(&mock, "/a", "<html><body>a</body></html>", 0).await;
    let base = format!("http://localhost:{port}");
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        },
    ));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        1,
        "an unrelated host must not be followed, got pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
    drop(mock);
}

/// An off-host link is never enqueued: the seed host and, with `allow_subdomains`, its
/// subdomains are the only hosts a crawl follows. ~keep `stay_on_domain` is NOT what
/// enforces this and never has -- see `link_scope::host_in_scope` and crawlberg#72.
///
/// ~keep Uses a reachable sibling host with `.expect(0)`, not a real external domain: fetching
/// an actual off-box host if the gate were broken would make this test flaky and
/// network-dependent instead of failing deterministically. `through_fixture`'s proxy is what
/// makes the sibling reachable, and its suffix allowlist is why the name ends in `localhost`.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_stays_on_the_seed_host() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(
            r#"<html><body><a href="http://elsewhere.localhost:{port}/x">out</a><a href="/a">A</a></body></html>"#
        ),
    )
    .await;
    mount_html(&mock, "/a", "<html><body>a</body></html>").await;
    mount_html_expecting(&mock, "/x", "<html><body>x</body></html>", 0).await;
    let base = format!("http://localhost:{port}");
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        },
    ));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned(), "/a".to_owned()],
        "an off-host link must not be enqueued"
    );
    drop(mock);
}

/// A seed that fails is reported through `CrawlResult::error`; a child that fails is not.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_reports_a_seed_failure_but_not_a_child_failure() {
    let seed_down = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&seed_down)
        .await;
    let engine = engine_with(permissive(CrawlConfig::default()));
    let result = engine
        .crawl_sequential(&seed_down.uri())
        .await
        .expect("a failing seed is still a completed crawl");
    assert!(result.pages.is_empty(), "a failing seed produces no pages");
    assert!(
        result.error.is_some(),
        "a depth-0 failure must surface as CrawlResult::error"
    );

    let child_down = MockServer::start().await;
    mount_html(&child_down, "/", r#"<html><body><a href="/a">A</a></body></html>"#).await;
    Mock::given(method("GET"))
        .and(path("/a"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&child_down)
        .await;
    let base = child_down.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        ..CrawlConfig::default()
    }));
    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");
    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned()],
        "the failing child contributes no page"
    );
    assert!(
        result.error.is_none(),
        "a failure below depth 0 must not become the crawl's error"
    );
}

/// A seed that redirects is counted once and reported under its post-redirect URL.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_counts_a_seed_redirect() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/landing"))
        .mount(&mock)
        .await;
    mount_html(&mock, "/landing", "<html><body>landed</body></html>").await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(0),
        ..CrawlConfig::default()
    }));

    let result = engine.crawl_sequential(&base).await.expect("crawl must succeed");

    assert_eq!(result.redirect_count, 1, "the seed hop must be counted once");
    assert_eq!(
        result.final_url,
        format!("{base}/landing"),
        "final_url must be the post-redirect URL"
    );
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
/// sequential crawl: `stay_on_domain` defaults to `false`, and `host_in_scope` treats
/// `Document` links as exempt from the host restriction in that case (`link_scope.rs`).
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_follows_a_cross_host_document_link_by_default() {
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
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        },
    ));

    engine.crawl_sequential(&base).await.expect("crawl must succeed");

    // ~keep The `.expect(1)` on /report.pdf is the real assertion; it is verified on drop.
    drop(mock);
}

/// The same cross-host document link must NOT be requested once `stay_on_domain` is `true`.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_rejects_a_cross_host_document_link_when_stay_on_domain_is_true() {
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
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            stay_on_domain: true,
            ..CrawlConfig::default()
        },
    ));

    engine.crawl_sequential(&base).await.expect("crawl must succeed");

    drop(mock);
}
