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

/// Admit `url` and run the sequential crawl on it, as `CrawlEngine::crawl` does on wasm32.
async fn crawl_admitted(engine: &CrawlEngine, url: &str) -> Result<CrawlResult, CrawlError> {
    let (engine, seed) = engine.admit(url)?;
    engine.crawl_sequential(&seed).await
}

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned()],
        "with path_patterns_match_query on, /blog?p=42 must be excluded"
    );
}

/// With `path_patterns_match_url` on, a pattern anchored on scheme and host matches.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_excludes_by_full_url_when_match_url_is_enabled() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<html><body><a href="/private/x">x</a></body></html>"#).await;
    mount_html(&mock, "/private/x", "<html><body>private</body></html>").await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        exclude_paths: vec![r"^https?://127\.0\.0\.1:\d+/private/".to_owned()],
        path_patterns_match_url: true,
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned()],
        "with path_patterns_match_url on, a host-anchored pattern must exclude /private/x"
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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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
    let result = crawl_admitted(&engine, &seed_down.uri())
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
    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");
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

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    crawl_admitted(&engine, &base).await.expect("crawl must succeed");

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

    crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    drop(mock);
}

/// With robots respected, a `nofollow` page's links are never requested, a `rel="nofollow"`
/// link is still followed, and a noindex page is still crawled and marked.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_honours_nofollow_when_respecting_robots() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/meta">meta</a><a href="/nf" rel="nofollow">nf</a></body></html>"#,
    )
    .await;
    mount_html(
        &mock,
        "/meta",
        r#"<html><head><meta name="robots" content="noindex, nofollow"></head>
<body><a href="/child">child</a></body></html>"#,
    )
    .await;
    mount_html_expecting(&mock, "/nf", "<html><body>nf</body></html>", 1).await;
    mount_html_expecting(&mock, "/child", "<html><body>child</body></html>", 0).await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(2),
        max_pages: Some(50),
        respect_robots_txt: true,
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned(), "/meta".to_owned(), "/nf".to_owned()]
    );
    assert!(result.pages[1].noindex_detected && result.pages[1].nofollow_detected);
    drop(mock);
}

/// The sequential loop reads each page through `CrawlEngine::scrape`, the same entry point
/// `scrape()` uses, so a meta tag named for crawlberg's own product token (not only the generic
/// `robots` name) must bind a page here too.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_honours_a_meta_tag_named_for_our_own_user_agent() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;
    mount_html(
        &mock,
        "/",
        r#"<html><head><meta name="crawlberg" content="noindex"></head><body>x</body></html>"#,
    )
    .await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        respect_robots_txt: true,
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert!(
        result.pages[0].noindex_detected,
        "a meta tag naming our own product token must be honoured by the sequential crawl loop"
    );
    drop(mock);
}

/// With robots not respected, the same links are all followed.
///
/// ~keep A guard, not evidence the fix works: `CrawlConfig::default()` leaves
/// ~keep `respect_robots_txt` false, so the suppression conjunct is `!(false && _)` and this
/// ~keep passes with the production change reverted. It exists to catch a mis-implementation that
/// ~keep applied nofollow unconditionally on the sequential loop, which would silently narrow
/// ~keep every default-configured crawl. Its assertions are the two `.expect(1)` mounts, verified
/// ~keep on `drop(mock)`. Keep it; do not read it as coverage of the fix.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_follows_nofollow_links_when_not_respecting_robots() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><head><meta name="robots" content="nofollow"></head>
<body><a href="/child">child</a><a href="/nf" rel="nofollow">nf</a></body></html>"#,
    )
    .await;
    mount_html_expecting(&mock, "/child", "<html><body>child</body></html>", 1).await;
    mount_html_expecting(&mock, "/nf", "<html><body>nf</body></html>", 1).await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(50),
        ..CrawlConfig::default()
    }));

    crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    drop(mock);
}

/// Serve `body` at `robots.txt` for the sequential crawl's seed origin.
async fn mount_robots(mock: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_owned()))
        .mount(mock)
        .await;
}

/// crawlberg#483: a `user_agents` rotation list decides the agent robots.txt is judged for,
/// so a site that disallows only the rotated agent is not crawled.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn wasm_sequential_crawl_rotates_the_configured_user_agent() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: AgentB\nDisallow: /\n").await;
    mount_html_expecting(&mock, "/", "<html><body>root</body></html>", 0).await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_pages: Some(10),
        respect_robots_txt: true,
        user_agent: Some("AgentA".to_owned()),
        user_agents: vec!["AgentB".to_owned()],
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        0,
        "robots.txt disallows the rotated agent, so no page may be fetched"
    );
    drop(mock);
}

/// Each page gets the next agent in the rotation, robots.txt is judged for that agent, and
/// the page request sends that same agent.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn wasm_sequential_crawl_picks_and_sends_the_agent_per_page() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: AgentB\nDisallow: /\n").await;
    for (at, body) in [
        (
            "/",
            r#"<html><body><a href="/next">n</a><a href="/third">t</a></body></html>"#,
        ),
        ("/third", "<html><body>third</body></html>"),
    ] {
        Mock::given(method("GET"))
            .and(path(at))
            .and(wiremock::matchers::header("user-agent", "AgentA"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body.to_owned())
                    .append_header("content-type", "text/html"),
            )
            .expect(1)
            .mount(&mock)
            .await;
    }
    mount_html_expecting(&mock, "/next", "<html><body>next</body></html>", 0).await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(10),
        respect_robots_txt: true,
        user_agents: vec!["AgentA".to_owned(), "AgentB".to_owned()],
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned(), "/third".to_owned()],
        "the second page is judged for AgentB and refused; the first and third go out as AgentA"
    );
    drop(mock);
}

/// Whether each request `mock` received for `at` carried `header`, in order.
async fn header_sent_to(mock: &MockServer, at: &str, header: &str) -> Vec<bool> {
    let requests = mock.received_requests().await.expect("request recording must be on");
    requests
        .iter()
        .filter(|request| request.url.path() == at)
        .map(|request| request.headers.contains_key(header))
        .collect()
}

fn bearer(config: CrawlConfig) -> CrawlConfig {
    CrawlConfig {
        auth: Some(crate::types::AuthConfig::Bearer {
            token: "test-fixture-bearer-token-not-a-real-secret".to_owned(),
        }),
        ..config
    }
}

/// A subdomain page the crawl follows is fetched without the credentials configured for the
/// seed host: the loop keeps the seed's credential scope for every frontier entry.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_withholds_credentials_from_a_subdomain_it_follows() {
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
        bearer(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            allow_subdomains: true,
            ..CrawlConfig::default()
        }),
    ));

    crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        header_sent_to(&mock, "/", "authorization").await,
        vec![true],
        "the seed host must get the configured credentials"
    );
    assert_eq!(
        header_sent_to(&mock, "/a", "authorization").await,
        vec![false],
        "the subdomain page must be fetched once, without the seed host's credentials"
    );
}

/// A cross-host document link, which a default crawl follows, is fetched without the
/// credentials configured for the seed host.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_withholds_credentials_from_a_cross_host_document() {
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
        bearer(CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        }),
    ));

    crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        header_sent_to(&mock, "/", "authorization").await,
        vec![true],
        "the seed host must get the configured credentials"
    );
    assert_eq!(
        header_sent_to(&mock, "/report.pdf", "authorization").await,
        vec![false],
        "the document on another host must be fetched once, without the seed host's credentials"
    );
}

/// `custom_headers` follow the same scope as `auth`: a subdomain page the crawl follows is
/// fetched without the headers configured for the seed host.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_withholds_custom_headers_from_a_subdomain_it_follows() {
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
            custom_headers: [("x-api-key".to_owned(), "fixture-value".to_owned())].into(),
            ..CrawlConfig::default()
        },
    ));

    crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        header_sent_to(&mock, "/", "x-api-key").await,
        vec![true],
        "the seed host must get the configured custom headers"
    );
    assert_eq!(
        header_sent_to(&mock, "/a", "x-api-key").await,
        vec![false],
        "the subdomain page must be fetched once, without the seed host's custom headers"
    );
}

/// A seed URL with `user:password@` gets Basic credentials on the seed host, for the seed
/// and for every page on that host the crawl follows.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_sends_seed_url_credentials_to_the_seed_host() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(&mock, "/", r#"<html><body><a href="/b">B</a></body></html>"#).await;
    mount_html(&mock, "/b", "<html><body>b</body></html>").await;
    let (user, password) = ("fixture-user", "fixture-password");
    let base = format!("http://{user}:{password}@localhost:{port}");
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        },
    ));

    crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        header_sent_to(&mock, "/", "authorization").await,
        vec![true],
        "the seed must get the credentials from its own URL"
    );
    assert_eq!(
        header_sent_to(&mock, "/b", "authorization").await,
        vec![true],
        "a page on the seed host must get the seed URL's credentials"
    );
}

/// Every page of a sequential crawl opens one `crawl.engine.scrape` span that records the
/// page's URL.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_opens_one_scrape_span_per_page() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(&mock, "/", r#"<html><body><a href="/b">B</a></body></html>"#).await;
    mount_html(&mock, "/b", "<html><body>b</body></html>").await;
    let base = format!("http://localhost:{port}");
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            ..CrawlConfig::default()
        },
    ));
    let captured = std::sync::Arc::new(crate::engine::tests::FieldCapture::default());

    let guard = tracing::subscriber::set_default(crate::engine::tests::CapturingSubscriber(captured.clone()));
    crawl_admitted(&engine, &base).await.expect("crawl must succeed");
    drop(guard);

    assert_eq!(
        captured.values("crawl.engine.scrape", "url.full"),
        vec![base.clone(), format!("{base}/b")],
        "each page must open one crawl.engine.scrape span with its URL"
    );
}

/// An unreachable robots.txt for the seed (HTTP 500) ends the whole crawl with the
/// `robots_unreachable` result, and the seed page is never fetched.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_stops_when_the_seed_robots_txt_is_unreachable() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;
    mount_html_expecting(&mock, "/", "<html><body>root</body></html>", 0).await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_pages: Some(10),
        respect_robots_txt: true,
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert!(result.pages.is_empty(), "no page may be fetched");
    assert!(result.was_skipped, "an unreachable seed robots.txt must skip the crawl");
    let error = result.error.clone().unwrap_or_default();
    assert!(
        error.starts_with("robots_unreachable"),
        "the crawl error must name the unreachable robots.txt, got {error:?}"
    );
    drop(mock);
}

/// A link that `exclude_paths` removes costs no rotation tick: the next page gets the agent
/// the excluded link would otherwise have taken.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_excluded_link_does_not_advance_the_rotation() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: AgentB\nDisallow: /\n").await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/skip">s</a><a href="/next">n</a></body></html>"#,
    )
    .await;
    mount_html_expecting(&mock, "/skip", "<html><body>skip</body></html>", 0).await;
    mount_html_expecting(&mock, "/next", "<html><body>next</body></html>", 0).await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(10),
        respect_robots_txt: true,
        exclude_paths: vec!["^/skip$".to_owned()],
        user_agents: vec!["AgentA".to_owned(), "AgentB".to_owned()],
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        visited(&result, &base),
        vec!["/".to_owned()],
        "/next must get AgentB, which robots.txt refuses; the excluded /skip takes no agent"
    );
    drop(mock);
}

/// The loop reads robots.txt once per origin and agent: three pages after the seed under two
/// rotating agents cost the loop two robots.txt requests. Each page's scrape reads it once
/// more for its `is_allowed` report, so the fixture sees 2 + 4.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_reads_robots_txt_once_per_origin_and_agent() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: *\nAllow: /\n").await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/b">b</a><a href="/c">c</a><a href="/d">d</a></body></html>"#,
    )
    .await;
    for at in ["/b", "/c", "/d"] {
        mount_html(&mock, at, "<html><body>x</body></html>").await;
    }
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(10),
        respect_robots_txt: true,
        user_agents: vec!["AgentA".to_owned(), "AgentB".to_owned()],
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(result.pages.len(), 4, "every page is allowed");
    let robots_requests = mock
        .received_requests()
        .await
        .expect("request recording must be on")
        .iter()
        .filter(|request| request.url.path() == "/robots.txt")
        .count();
    assert_eq!(
        robots_requests, 6,
        "the loop must read robots.txt once per agent (2), plus one read per page scrape (4)"
    );
    drop(mock);
}

/// A page on another origin is judged against that origin's own robots.txt, not the seed's.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_judges_a_second_origin_by_its_own_robots_txt() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .and(wiremock::matchers::header(
            "host",
            format!("sub.localhost:{port}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /\n"))
        .with_priority(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .with_priority(5)
        .mount(&mock)
        .await;
    mount_html(
        &mock,
        "/",
        &format!(r#"<html><body><a href="http://sub.localhost:{port}/a">A</a></body></html>"#),
    )
    .await;
    mount_html_expecting(&mock, "/a", "<html><body>a</body></html>", 0).await;
    let base = format!("http://localhost:{port}");
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            max_pages: Some(50),
            allow_subdomains: true,
            respect_robots_txt: true,
            ..CrawlConfig::default()
        },
    ));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.iter().map(|page| page.url.as_str()).collect::<Vec<_>>(),
        vec![base.as_str()],
        "the subdomain's own robots.txt refuses /a"
    );
    drop(mock);
}

/// With `respect_robots_txt` off, the loop never requests robots.txt, and the page still goes
/// out under the rotation agent.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn sequential_crawl_skips_robots_txt_when_not_respecting_it() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /\n"))
        .expect(0)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(wiremock::matchers::header("user-agent", "AgentZ"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>r</body></html>")
                .append_header("content-type", "text/html"),
        )
        .expect(1)
        .mount(&mock)
        .await;
    let base = mock.uri();
    let engine = engine_with(permissive(CrawlConfig {
        max_pages: Some(10),
        respect_robots_txt: false,
        user_agent: Some("Configured".to_owned()),
        user_agents: vec!["AgentZ".to_owned()],
        ..CrawlConfig::default()
    }));

    let result = crawl_admitted(&engine, &base).await.expect("crawl must succeed");

    assert_eq!(result.pages.len(), 1, "the page must be fetched as AgentZ");
    drop(mock);
}

/// The wasm page fetch sends a pinned agent as its one `user-agent` line, in place of the
/// configured default, and reports that agent as the one it sent.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn wasm_page_fetch_sends_the_pinned_agent_once() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", "<html><body>root</body></html>").await;
    let engine = engine_with(permissive(CrawlConfig {
        user_agent: Some("Configured".to_owned()),
        ..CrawlConfig::default()
    }));

    let (_, response, _) = engine
        .wasm_fetch_for_scrape(&mock.uri(), Some("Pinned"))
        .await
        .expect("fetch must succeed");

    let requests = mock.received_requests().await.expect("request recording must be on");
    let sent: Vec<Vec<String>> = requests
        .iter()
        .map(|request| {
            request
                .headers
                .get_all("user-agent")
                .iter()
                .map(|value| value.to_str().unwrap_or_default().to_owned())
                .collect()
        })
        .collect();
    assert_eq!(
        sent,
        vec![vec!["Pinned".to_owned()]],
        "one request, with the pinned agent as its only user-agent line"
    );
    assert_eq!(response.sent_user_agent.as_deref(), Some("Pinned"));
}

/// The wasm page fetch keeps the page rule: a sitemap whose URL says "blocked", served by
/// Cloudflare, is refused when it is scraped as a page (crawlberg#515 reads it only for `map`).
#[tokio::test]
async fn wasm_page_fetch_refuses_a_small_cloudflare_body_that_says_blocked() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/xml")
                .append_header("server", "cloudflare")
                .set_body_string(
                    "<urlset><url><loc>https://example.com/blog/why-we-blocked-the-old-api</loc></url></urlset>",
                ),
        )
        .mount(&mock)
        .await;
    let engine = engine_with(permissive(CrawlConfig::default()));

    let result = engine.wasm_fetch_for_scrape(&mock.uri(), None).await;

    assert!(
        matches!(result, Err(CrawlError::WafBlocked { ref vendor, .. }) if vendor == "cloudflare"),
        "a scraped page keeps the page rule, got {:?}",
        result.map(|(url, _, _)| url)
    );
}

/// The wasm page fetch passes `RefreshRedirects::Ignore`, so a `<meta http-equiv="refresh">` on
/// the page it fetches is not a hop it takes; it returns that page's own response.
#[tokio::test]
#[serial_test::serial(engine_tracing_callsites)]
async fn wasm_page_fetch_does_not_follow_a_meta_refresh() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><head><meta http-equiv="refresh" content="0; url=/next"></head><body></body></html>"#,
    )
    .await;
    mount_html(&mock, "/next", "<html><body>next</body></html>").await;
    let engine = engine_with(permissive(CrawlConfig::default()));

    let (final_url, _, _) = engine
        .wasm_fetch_for_scrape(&format!("{}/", mock.uri()), None)
        .await
        .expect("fetch must succeed");

    let next = mock
        .received_requests()
        .await
        .expect("request recording must be on")
        .iter()
        .filter(|r| r.url.path() == "/next")
        .count();
    assert_eq!(next, 0, "the wasm page fetch followed a refresh; final_url={final_url}");
}
