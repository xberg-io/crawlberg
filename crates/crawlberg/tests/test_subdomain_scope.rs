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
//! in this file green). `through_fixture` routes every request to the fixture server, so
//! reachability needs no resolver at all. Each rejected endpoint is mounted with `.expect(0)`,
//! verified on drop, so the test fails if the request is ever sent — the request-count check,
//! not `pages.len()` alone, is what proves the gate fired.

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
/// ~keep otherwise installs (`http/client.rs`, only on a client that sends through no
/// ~keep proxy), because hyper then resolves the proxy host rather than the target.
/// ~keep So these tests no longer exercise the SSRF DNS-pinning path they used to; the
/// ~keep allowlisted `validate_url` pre-check above is the only SSRF enforcement left in them.
/// ~keep Coverage for the pinning itself lives in `build_client`'s own tests
/// ~keep (`build_client_enforces_the_ssrf_policy_during_dns_resolution` and
/// ~keep `build_client_skips_the_policy_resolver_when_a_proxy_is_configured`).
fn through_fixture(mock: &MockServer, config: CrawlConfig) -> CrawlConfig {
    CrawlConfig {
        ssrf: crawlberg::SsrfPolicy {
            deny_private: false,
            allowlist: vec![crawlberg::HostMatcher::suffix("localhost")],
            ..crawlberg::SsrfPolicy::default()
        },
        proxy: Some(crawlberg::ProxyConfig {
            url: mock.uri(),
            username: None,
            password: None,
        }),
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
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            allow_subdomains: true,
            respect_robots_txt: false,
            ..CrawlConfig::default()
        },
    ));

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
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            allow_subdomains: false,
            respect_robots_txt: false,
            ..CrawlConfig::default()
        },
    ));

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
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            allow_subdomains: true,
            respect_robots_txt: false,
            ..CrawlConfig::default()
        },
    ));

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
/// widen a default crawl past the seed host and its subdomains. `through_fixture` routes the
/// unrelated host to the fixture server, so its `.expect(0)` mock sees the request if scope ever
/// lets it through. See crawlberg#72 for the `stay_on_domain` question.
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
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            respect_robots_txt: false,
            ..CrawlConfig::default()
        },
    ));

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
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            respect_robots_txt: false,
            ..CrawlConfig::default()
        },
    ));

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
    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            respect_robots_txt: false,
            stay_on_domain: true,
            ..CrawlConfig::default()
        },
    ));

    engine.crawl(&base).await.expect("crawl must succeed");

    drop(mock);
}

/// crawlberg#771: with `allow_subdomains`, `custom_headers` reach an admitted subdomain too, but
/// credentials never do: neither the configured `auth` nor a credential-bearing custom header.
#[tokio::test]
async fn should_send_custom_headers_but_no_credentials_to_an_admitted_subdomain() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    mount_html(
        &mock,
        "/",
        &format!(
            r#"<html><body><a href="http://sub.localhost:{port}/child">sub</a>
               <a href="http://localhost:{port}/same">same</a></body></html>"#
        ),
    )
    .await;
    mount_html(&mock, "/child", "<html><body>sub</body></html>").await;
    mount_html(&mock, "/same", "<html><body>same</body></html>").await;

    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(1),
            allow_subdomains: true,
            respect_robots_txt: false,
            auth: Some(crawlberg::AuthConfig::Bearer {
                token: "seed-only-token".to_owned(),
            }),
            custom_headers: std::collections::HashMap::from([
                ("X-Probe".to_owned(), "sent".to_owned()),
                ("Accept".to_owned(), "text/html,probe/accept".to_owned()),
                ("Cookie".to_owned(), "sid=secret".to_owned()),
                ("Proxy-Authorization".to_owned(), "Basic c2VjcmV0".to_owned()),
                ("X-Api-Key".to_owned(), "secret".to_owned()),
            ]),
            ..CrawlConfig::default()
        },
    ));

    let result = engine
        .crawl(&format!("http://localhost:{port}"))
        .await
        .expect("crawl must succeed");
    assert_eq!(
        result.pages.len(),
        3,
        "seed, subdomain and same-host pages: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );

    let requests = mock.received_requests().await.expect("the mock records requests");
    let header = |at: &str, name: &str| {
        requests
            .iter()
            .find(|request| request.url.path() == at)
            .unwrap_or_else(|| panic!("{at} was never requested"))
            .headers
            .get(name)
            .map(|value| value.to_str().expect("ascii header").to_owned())
    };

    for at in ["/", "/same", "/child"] {
        assert_eq!(
            header(at, "x-probe").as_deref(),
            Some("sent"),
            "{at} must carry X-Probe"
        );
        assert_eq!(
            header(at, "accept").as_deref(),
            Some("text/html,probe/accept"),
            "{at} must carry Accept"
        );
    }
    assert_eq!(
        header("/same", "authorization").as_deref(),
        Some("Bearer seed-only-token")
    );
    assert_eq!(header("/same", "cookie").as_deref(), Some("sid=secret"));
    for name in ["authorization", "cookie", "proxy-authorization", "x-api-key"] {
        assert_eq!(header("/child", name), None, "{name} must never reach a subdomain");
    }
}

/// crawlberg#771 on a redirect hop: the seed redirects to an admitted subdomain, and that hop
/// carries the non-credential custom headers but none of the seed host's credentials.
#[tokio::test]
async fn should_send_custom_headers_but_no_credentials_on_a_redirect_hop_to_an_admitted_subdomain() {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302).append_header("location", format!("http://sub.localhost:{port}/landing")),
        )
        .mount(&mock)
        .await;
    mount_html(&mock, "/landing", "<html><body>landed</body></html>").await;

    let engine = engine_with(through_fixture(
        &mock,
        CrawlConfig {
            max_depth: Some(0),
            allow_subdomains: true,
            respect_robots_txt: false,
            auth: Some(crawlberg::AuthConfig::Bearer {
                token: "seed-only-token".to_owned(),
            }),
            custom_headers: std::collections::HashMap::from([
                ("X-Probe".to_owned(), "sent".to_owned()),
                ("Cookie".to_owned(), "sid=secret".to_owned()),
                ("X-Api-Key".to_owned(), "secret".to_owned()),
            ]),
            ..CrawlConfig::default()
        },
    ));

    let result = engine
        .crawl(&format!("http://localhost:{port}/start"))
        .await
        .expect("crawl must succeed");
    assert_eq!(
        result.pages.len(),
        1,
        "the page the redirect lands on: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );

    let requests = mock.received_requests().await.expect("the mock records requests");
    let header = |at: &str, name: &str| {
        requests
            .iter()
            .find(|request| request.url.path() == at)
            .unwrap_or_else(|| panic!("{at} was never requested"))
            .headers
            .get(name)
            .map(|value| value.to_str().expect("ascii header").to_owned())
    };

    assert_eq!(header("/start", "x-probe").as_deref(), Some("sent"));
    assert_eq!(
        header("/start", "authorization").as_deref(),
        Some("Bearer seed-only-token")
    );
    assert_eq!(
        header("/landing", "x-probe").as_deref(),
        Some("sent"),
        "the hop carries X-Probe"
    );
    for name in ["authorization", "cookie", "x-api-key"] {
        assert_eq!(
            header("/landing", name),
            None,
            "{name} must not follow a redirect onto a subdomain"
        );
    }
}
