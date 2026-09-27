//! A redirect on a *discovered link* is followed during crawl traversal (issue #62).
//!
//! Before this fix, frontier fetches went straight through `CrawlEngine::fetch_response`,
//! which never follows a 3xx: a link answering 301/302/etc. was reported as a page with an
//! empty body and the redirect status, and its target was never requested. Only the seed's
//! own redirect chain was resolved, via a separate code path (`resolve_initial_redirects`).
//!
//! These tests exercise the traversal path specifically -- a redirect reached by following a
//! link discovered on some other page, not the crawl's starting URL.

use crawlberg::{CrawlConfig, CrawlResult, HostMatcher, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";

async fn mount_robots(mock: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_owned()))
        .mount(mock)
        .await;
}

async fn mount_page(mock: &MockServer, at: &str, html: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(html.to_owned())
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

async fn mount_redirect(mock: &MockServer, at: &str, to: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(301).append_header("location", to.to_owned()))
        .mount(mock)
        .await;
}

/// Every path the server was asked for, in arrival order.
async fn request_log(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("the mock server records its requests")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

async fn crawl_seed(config: CrawlConfig, seed: &str) -> CrawlResult {
    let engine = create_engine(Some(config)).expect("engine builds");
    crawlberg::crawl(&engine, seed).await.expect("crawl runs")
}

fn config() -> crawlberg::CrawlConfigBuilder {
    CrawlConfig::builder()
        .respect_robots_txt(true)
        .allow_private_networks(true)
        .max_depth(1)
        .max_concurrent(1)
        .max_pages(10)
}

/// The reporter's exact case: `/` links `/old`, `/old` redirects to `/new`.
#[tokio::test]
async fn discovered_link_redirect_target_is_requested_and_reported() {
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_page(&mock, "/", "<html><body><a href=\"/old\">Link</a></body></html>").await;
    mount_redirect(&mock, "/old", "/new").await;
    mount_page(&mock, "/new", "<html><body><h1>New Page</h1></body></html>").await;

    let result = crawl_seed(config().build(), &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        log.iter().any(|entry| entry == "/new"),
        "the redirect target must be requested, got {log:?}"
    );
    assert_eq!(
        log.iter().filter(|entry| *entry == "/new").count(),
        1,
        "the redirect target must be requested exactly once, got {log:?}"
    );
    assert_eq!(
        result.pages.len(),
        2,
        "the seed and the /old entry are the crawl's two pages"
    );

    let redirected_page = result
        .pages
        .iter()
        .find(|p| p.url.ends_with("/old"))
        .expect("the /old entry must be reported as a page");
    assert_eq!(
        redirected_page.status_code, 200,
        "the reported status must be the target's, not the 301 the discovered link answered with"
    );
    assert!(
        redirected_page.html.contains("New Page"),
        "the reported page must carry the target's content, got {:?}",
        redirected_page.html
    );
    assert!(
        redirected_page.final_url.ends_with("/new"),
        "the reported page must carry the final URL, got {}",
        redirected_page.final_url
    );
    assert_eq!(
        redirected_page.redirect_count, 1,
        "one redirect hop was followed to reach the target"
    );
}

/// robots.txt still governs a redirect target reached through crawl traversal, not only
/// through the seed's own chain.
#[tokio::test]
async fn per_hop_robots_is_still_enforced_on_a_traversal_redirect_target() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: *\nDisallow: /private.html\n").await;
    mount_page(&mock, "/", "<html><body><a href=\"/old\">Link</a></body></html>").await;
    mount_redirect(&mock, "/old", "/private.html").await;
    mount_page(&mock, "/private.html", "<html><body>private</body></html>").await;

    let result = crawl_seed(config().build(), &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|entry| entry == "/private.html"),
        "robots.txt disallows the redirect target, so it must never be requested, got {log:?}"
    );
    assert_eq!(
        result.pages.len(),
        1,
        "only the seed is reported; the disallowed redirect target yields no page"
    );
}

/// `exclude_paths` still governs a redirect target reached through crawl traversal.
#[tokio::test]
async fn per_hop_exclude_paths_is_still_enforced_on_a_traversal_redirect_target() {
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_page(&mock, "/", "<html><body><a href=\"/old\">Link</a></body></html>").await;
    mount_redirect(&mock, "/old", "/private/x.html").await;
    mount_page(&mock, "/private/x.html", "<html><body>private</body></html>").await;

    let config = config().exclude_paths(vec!["/private".to_owned()]).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|entry| entry == "/private/x.html"),
        "exclude_paths names the redirect target, so it must never be requested, got {log:?}"
    );
    assert_eq!(
        result.pages.len(),
        1,
        "only the seed is reported; the excluded redirect target yields no page"
    );
}

/// SSRF is still enforced on a redirect target reached through crawl traversal, even though
/// the seed's own loopback host is allowlisted so the mock server can be reached at all.
#[tokio::test]
async fn ssrf_is_still_enforced_on_a_traversal_redirect_target() {
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_page(&mock, "/", "<html><body><a href=\"/old\">Link</a></body></html>").await;
    mount_redirect(&mock, "/old", "http://169.254.169.254/latest/meta-data/").await;

    // ~keep Only the mock server's own loopback address is allowlisted (via CIDR -- an
    // ~keep exact matcher does not bypass IP-literal denial, see `HostMatcher::exact` docs).
    // ~keep `deny_private` stays on, so the metadata-IP redirect target is still refused.
    let config = CrawlConfig::builder()
        .respect_robots_txt(true)
        .max_depth(1)
        .max_concurrent(1)
        .max_pages(10)
        .ssrf_allowlist_host(HostMatcher::cidr("127.0.0.1/32").expect("valid CIDR"))
        .build();

    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    assert_eq!(
        result.pages.len(),
        1,
        "only the seed is reported; the SSRF-denied redirect target yields no page"
    );
    assert!(
        result.error.is_none(),
        "an SSRF violation on a traversal redirect target must not fail the whole crawl, got {:?}",
        result.error
    );
}

/// Reaching `max_redirects` mid-traversal is not an error: the chain stops and the crawl
/// reports the most recent 3xx response, matching the seed's existing behaviour
/// (`should_report_the_hops_already_followed_when_a_later_hop_is_refused` in
/// `test_redirect_policy.rs`).
#[tokio::test]
async fn redirect_chain_reaching_max_redirects_during_traversal_stops_and_reports_the_last_hop() {
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_page(&mock, "/", "<html><body><a href=\"/old\">Link</a></body></html>").await;
    mount_redirect(&mock, "/old", "/a").await;
    mount_redirect(&mock, "/a", "/b").await;
    mount_redirect(&mock, "/b", "/c").await;
    mount_page(&mock, "/c", "<html><body>unreachable within budget</body></html>").await;

    let config = config().max_redirects(2).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|entry| entry == "/c"),
        "the hop past max_redirects must never be requested, got {log:?}"
    );
    assert_eq!(
        result.pages.len(),
        2,
        "the seed and the /old entry (stopped mid-chain) are the crawl's two pages"
    );
    let stopped_page = result
        .pages
        .iter()
        .find(|p| p.url.ends_with("/old"))
        .expect("the /old entry must still be reported, carrying the last hop it reached");
    assert_eq!(
        stopped_page.redirect_count, 2,
        "exactly max_redirects hops were followed before the chain stopped"
    );
    assert_eq!(
        stopped_page.status_code, 301,
        "the reported response is the unfollowed 3xx the chain stopped on, not a 200"
    );
    assert!(
        stopped_page.final_url.ends_with("/b"),
        "the chain must stop at the hop max_redirects allows, not its target, got {}",
        stopped_page.final_url
    );
}

/// Mirrors `fixtures/crawl/crawl_redirect_in_traversal.json`: proves the redirect target was
/// genuinely fetched and parsed, not merely counted, by requiring its own outbound link to be
/// discovered. Before this fix the redirect target's content (and hence its links) was never
/// seen, so `pages.length` alone -- without this second-level link -- could not tell a fixed
/// crawl from a buggy one; both produced the same page count.
#[tokio::test]
async fn redirect_target_content_is_actually_parsed_for_further_links() {
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_page(&mock, "/", "<html><body><a href=\"/old\">Link</a></body></html>").await;
    mount_redirect(&mock, "/old", "/new").await;
    mount_page(
        &mock,
        "/new",
        "<html><body><h1>New Page</h1><a href=\"/deeper\">Deeper</a></body></html>",
    )
    .await;
    mount_page(&mock, "/deeper", "<html><body><h1>Deeper Page</h1></body></html>").await;

    let config = CrawlConfig::builder()
        .respect_robots_txt(true)
        .allow_private_networks(true)
        .max_depth(2)
        .max_concurrent(1)
        .max_pages(10)
        .build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    assert_eq!(
        result.pages.len(),
        3,
        "/, /old (serving /new's content) and /deeper (discovered only from that content) \
         must all be reported"
    );
}

/// A page reachable both directly and via a redirect is requested once, not twice.
#[tokio::test]
async fn page_reachable_both_directly_and_via_redirect_is_fetched_once() {
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_page(
        &mock,
        "/",
        "<html><body><a href=\"/old\">Old</a><a href=\"/shared\">Direct</a></body></html>",
    )
    .await;
    mount_redirect(&mock, "/old", "/shared").await;
    mount_page(&mock, "/shared", "<html><body>shared content</body></html>").await;

    let result = crawl_seed(config().build(), &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert_eq!(
        log.iter().filter(|entry| *entry == "/shared").count(),
        1,
        "a page reachable both directly and via a redirect must be requested exactly once, got {log:?}"
    );
    assert_eq!(
        result.pages.len(),
        2,
        "the seed plus exactly one reported page for /shared -- the duplicate path is skipped, not double-reported"
    );
}

/// A redirected page's markdown resolves its relative links against the URL that served it,
/// the same base the page's `links` list uses (issue #63).
#[tokio::test]
async fn redirected_page_markdown_resolves_relative_links_against_the_final_url() {
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_page(&mock, "/", "<html><body><a href=\"/old\">Link</a></body></html>").await;
    mount_redirect(&mock, "/old", "/landing/index.html").await;
    mount_page(
        &mock,
        "/landing/index.html",
        "<html><body><p><a href=\"next.html\">next</a></p></body></html>",
    )
    .await;

    let result = crawl_seed(config().build(), &format!("{}/", mock.uri())).await;

    let redirected_page = result
        .pages
        .iter()
        .find(|p| p.url.ends_with("/old"))
        .expect("the /old entry must be reported as a page");
    let expected = format!("{}/landing/next.html", mock.uri());
    let markdown = &redirected_page.markdown.as_ref().expect("markdown is produced").content;
    assert!(
        markdown.contains(&format!("[next]({expected})")),
        "the markdown link must resolve against the final URL, got {markdown:?}"
    );
    assert!(
        redirected_page.links.iter().any(|link| link.url == expected),
        "the links list must agree with the markdown, got {:?}",
        redirected_page.links
    );
}
