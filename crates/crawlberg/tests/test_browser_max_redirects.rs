//! `max_redirects` bounds the redirects Chrome follows in browser mode the same way it bounds
//! the HTTP mode's own chain: the same redirect count, the same stopping response, and no
//! request past the limit.
//!
//! Requires a real Chrome binary (chromiumoxide auto-detects it) and is gated behind the
//! `browser` feature; skipped (not failed) when Chrome is unavailable, matching the other
//! browser tests.

#![cfg(feature = "browser")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserSessionPool, CrawlConfig,
    CrawlError, CrawlPageResult, CrawlResult, crawl, create_engine, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn config(mode: BrowserMode, max_redirects: usize) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        max_depth: Some(0),
        max_redirects,
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

async fn mount_html(mock: &MockServer, route: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html"))
        .mount(mock)
        .await;
}

/// `/` answers 301 to `/r1`, `/r1` to `/r2`, and so on for `hops` redirects; the last
/// hop lands on a 200 page.
async fn redirect_chain(hops: usize) -> MockServer {
    let mock = MockServer::start().await;
    for hop in 0..hops {
        let from = if hop == 0 { "/".to_owned() } else { format!("/r{hop}") };
        Mock::given(method("GET"))
            .and(path(from))
            .respond_with(ResponseTemplate::new(301).append_header("location", format!("/r{}", hop + 1)))
            .mount(&mock)
            .await;
    }
    mount_html(&mock, &format!("/r{hops}"), r#"<p id="landed">landed</p>"#).await;
    mock
}

/// Crawl `seed`, or `None` when no usable Chrome exists on this host.
async fn crawl_with(test_name: &str, config: CrawlConfig, seed: &str) -> Option<CrawlResult> {
    let engine = create_engine(Some(config)).expect("engine must build");
    match crawl(&engine, seed).await {
        Ok(result) => Some(result),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("crawl must succeed: {error:?}"),
    }
}

async fn requested_paths(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

/// The seed's page, or a panic that names what the crawl reported instead.
fn seed_page(result: &CrawlResult) -> &CrawlPageResult {
    result.pages.first().unwrap_or_else(|| {
        panic!(
            "no seed page: redirect_count={}, final_url={}, error={:?}",
            result.redirect_count, result.final_url, result.error
        )
    })
}

/// What a crawl reports about its seed's redirect chain.
fn chain_outcome(result: &CrawlResult, base: &str) -> (usize, String, u16) {
    let page = seed_page(result);
    (
        result.redirect_count,
        result.final_url.trim_start_matches(base).to_owned(),
        page.status_code,
    )
}

#[tokio::test]
async fn browser_mode_stops_a_chain_longer_than_max_redirects_where_http_mode_does() {
    let test_name = "browser_mode_stops_a_chain_longer_than_max_redirects_where_http_mode_does";
    let http_site = redirect_chain(5).await;
    let http = crawl_with(
        test_name,
        config(BrowserMode::Never, 2),
        &format!("{}/", http_site.uri()),
    )
    .await
    .expect("HTTP mode needs no Chrome");
    let http_outcome = chain_outcome(&http, &http_site.uri());
    assert_eq!(
        http_outcome,
        (2, "/r2".to_owned(), 301),
        "HTTP mode stops on the 3xx at the limit"
    );

    let browser_site = redirect_chain(5).await;
    let Some(browser) = crawl_with(
        test_name,
        config(BrowserMode::Always, 2),
        &format!("{}/", browser_site.uri()),
    )
    .await
    else {
        return;
    };
    assert_eq!(
        chain_outcome(&browser, &browser_site.uri()),
        http_outcome,
        "browser mode must report the chain the way HTTP mode does"
    );

    let requested = requested_paths(&browser_site).await;
    assert!(
        !requested.iter().any(|p| ["/r3", "/r4", "/r5"].contains(&p.as_str())),
        "Chrome must not request past the limit, requested: {requested:?}"
    );
}

#[tokio::test]
async fn browser_mode_follows_a_chain_of_exactly_max_redirects() {
    let site = redirect_chain(2).await;
    let Some(result) = crawl_with(
        "browser_mode_follows_a_chain_of_exactly_max_redirects",
        config(BrowserMode::Always, 2),
        &format!("{}/", site.uri()),
    )
    .await
    else {
        return;
    };

    assert_eq!(chain_outcome(&result, &site.uri()), (2, "/r2".to_owned(), 200));
    let page = seed_page(&result);
    assert!(
        page.html.contains("landed"),
        "the landing page must be rendered: {}",
        page.html
    );
}

/// Crawl `site` in HTTP mode and in browser mode at `max_redirects`, and return both outcomes
/// with the paths Chrome requested, or `None` when no usable Chrome exists on this host.
async fn http_and_browser(
    test_name: &str,
    max_redirects: usize,
    http_site: &MockServer,
    browser_site: &MockServer,
) -> Option<((usize, String, u16), (usize, String, u16), Vec<String>)> {
    let http = crawl_with(
        test_name,
        config(BrowserMode::Never, max_redirects),
        &format!("{}/", http_site.uri()),
    )
    .await
    .expect("HTTP mode needs no Chrome");
    let browser = crawl_with(
        test_name,
        config(BrowserMode::Always, max_redirects),
        &format!("{}/", browser_site.uri()),
    )
    .await?;
    Some((
        chain_outcome(&http, &http_site.uri()),
        chain_outcome(&browser, &browser_site.uri()),
        requested_paths(browser_site).await,
    ))
}

/// Each step of `steps` but the last carries a zero-delay meta refresh to the next step; the
/// last is a plain page.
async fn meta_refresh_chain(steps: &[&str]) -> MockServer {
    let mock = MockServer::start().await;
    for step in steps.windows(2) {
        Mock::given(method("GET"))
            .and(path(step[0]))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head><meta http-equiv="refresh" content="0; url={}"></head><body>{}</body></html>"#,
                    step[1], step[0]
                ),
                "text/html",
            ))
            .mount(&mock)
            .await;
    }
    let last = steps.last().expect("a chain has a last step");
    mount_html(&mock, last, &format!(r#"<p id="last">{last}</p>"#)).await;
    mock
}

/// A meta refresh Chrome follows is one redirect, as it is in HTTP mode, and at the limit Chrome
/// stays on the page that carries it (#117).
#[tokio::test]
async fn a_meta_refresh_chrome_follows_counts_as_one_redirect_as_in_http_mode() {
    let test_name = "a_meta_refresh_chrome_follows_counts_as_one_redirect_as_in_http_mode";
    for (max_redirects, expected) in [(0, (0, "/".to_owned(), 200)), (1, (1, "/n".to_owned(), 200))] {
        let Some((http, browser, requested)) = http_and_browser(
            test_name,
            max_redirects,
            &meta_refresh_chain(&["/", "/n"]).await,
            &meta_refresh_chain(&["/", "/n"]).await,
        )
        .await
        else {
            return;
        };
        assert_eq!(http, expected, "max_redirects={max_redirects}: HTTP mode");
        assert_eq!(
            browser, http,
            "max_redirects={max_redirects}: browser mode must count the refresh as HTTP mode does"
        );
        if max_redirects == 0 {
            assert!(
                !requested.iter().any(|p| p == "/n"),
                "Chrome must not follow the refresh past the limit, requested: {requested:?}"
            );
        }
    }
}

/// A chain of meta refreshes Chrome follows stops at the limit where HTTP mode stops (#193).
#[tokio::test]
async fn max_redirects_bounds_a_meta_refresh_chain_chrome_follows() {
    let test_name = "max_redirects_bounds_a_meta_refresh_chain_chrome_follows";
    let steps = ["/", "/a", "/b", "/c", "/d"];
    for (max_redirects, expected, never) in [
        (0, (0, "/".to_owned(), 200), &["/a", "/b", "/c", "/d"][..]),
        (2, (2, "/b".to_owned(), 200), &["/c", "/d"][..]),
    ] {
        let Some((http, browser, requested)) = http_and_browser(
            test_name,
            max_redirects,
            &meta_refresh_chain(&steps).await,
            &meta_refresh_chain(&steps).await,
        )
        .await
        else {
            return;
        };
        assert_eq!(http, expected, "max_redirects={max_redirects}: HTTP mode");
        assert_eq!(browser, http, "max_redirects={max_redirects}: browser mode");
        assert!(
            !requested.iter().any(|p| never.contains(&p.as_str())),
            "max_redirects={max_redirects}: Chrome must not follow the chain past the limit, requested: {requested:?}"
        );
    }
}

/// `/` loads, then its script navigates to `/g0`, which redirects to `/g1`, and so on to `/g4`.
async fn script_navigation_into_redirects() -> MockServer {
    let site = MockServer::start().await;
    mount_html(
        &site,
        "/",
        "<p>start</p><script>addEventListener('load', () => location.replace('/g0'))</script>",
    )
    .await;
    for hop in 0..4 {
        Mock::given(method("GET"))
            .and(path(format!("/g{hop}")))
            .respond_with(ResponseTemplate::new(302).append_header("location", format!("/g{}", hop + 1)))
            .mount(&site)
            .await;
    }
    mount_html(&site, "/g4", "<p>g4</p>").await;
    site
}

/// A navigation the page's script starts counts as one redirect, and each redirect it follows
/// counts too. Past the limit Chrome stays on the page it has (#193).
#[tokio::test]
async fn max_redirects_bounds_a_script_navigation_and_the_redirects_after_it() {
    let test_name = "max_redirects_bounds_a_script_navigation_and_the_redirects_after_it";
    for (max_redirects, never) in [(0, &["/g0", "/g1"][..]), (2, &["/g2", "/g3", "/g4"][..])] {
        let site = script_navigation_into_redirects().await;
        let Some(result) = crawl_with(
            test_name,
            config(BrowserMode::Always, max_redirects),
            &format!("{}/", site.uri()),
        )
        .await
        else {
            return;
        };
        let requested = requested_paths(&site).await;
        assert!(
            !requested.iter().any(|p| never.contains(&p.as_str())),
            "max_redirects={max_redirects}: Chrome must not follow the navigation past the limit, requested: {requested:?}"
        );
        assert_eq!(
            chain_outcome(&result, &site.uri()),
            (0, "/".to_owned(), 200),
            "max_redirects={max_redirects}: the page stays on the seed"
        );
    }
}

/// Within the limit, a navigation the page's script starts is followed and counts as one.
#[tokio::test]
async fn a_script_navigation_within_the_limit_counts_as_one_redirect() {
    let site = MockServer::start().await;
    mount_html(
        &site,
        "/",
        "<p>start</p><script>addEventListener('load', () => location.replace('/after'))</script>",
    )
    .await;
    mount_html(&site, "/after", r#"<p id="after">after</p>"#).await;

    let Some(result) = crawl_with(
        "a_script_navigation_within_the_limit_counts_as_one_redirect",
        config(BrowserMode::Always, 1),
        &format!("{}/", site.uri()),
    )
    .await
    else {
        return;
    };

    assert_eq!(chain_outcome(&result, &site.uri()), (1, "/after".to_owned(), 200));
    let page = seed_page(&result);
    assert!(
        page.html.contains("id=\"after\""),
        "the page must be the one the script navigated to: {}",
        page.html
    );
}

/// A navigation the page starts during the extra wait counts too: the count is read with the
/// page, not when the load ends.
///
/// ~keep The 1200 ms delay outlasts the 500 ms settle after the load, so the navigation starts
/// ~keep inside the extra wait.
#[tokio::test]
async fn a_script_navigation_during_the_extra_wait_counts_as_one_redirect() {
    let site = MockServer::start().await;
    mount_html(
        &site,
        "/",
        "<p>start</p><script>addEventListener('load', () => setTimeout(() => location.replace('/after'), 1200))</script>",
    )
    .await;
    mount_html(&site, "/after", r#"<p id="after">after</p>"#).await;
    let mut config = config(BrowserMode::Always, 1);
    config.browser.extra_wait = Some(Duration::from_millis(2500));

    let Some(result) = crawl_with(
        "a_script_navigation_during_the_extra_wait_counts_as_one_redirect",
        config,
        &format!("{}/", site.uri()),
    )
    .await
    else {
        return;
    };

    assert_eq!(chain_outcome(&result, &site.uri()), (1, "/after".to_owned(), 200));
}

/// `/` redirects twice to `/m`, whose meta refresh (too slow for Chrome to act on before the
/// page is read) points at `/n`, which starts a second chain of two redirects.
///
/// ~keep The 30-second delay is deliberate: Chrome never acts on the refresh, so the second
/// ~keep chain is walked by the crawl's own chain, one browser fetch per hop. A refresh Chrome
/// ~keep does act on is covered by the meta-refresh tests above; shortening the delay here would
/// ~keep change what this test measures.
async fn chain_with_a_meta_refresh() -> MockServer {
    let mock = MockServer::start().await;
    for (from, to) in [("/", "/r1"), ("/r1", "/m"), ("/n", "/n1"), ("/n1", "/n2")] {
        Mock::given(method("GET"))
            .and(path(from))
            .respond_with(ResponseTemplate::new(301).append_header("location", to))
            .mount(&mock)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><head><meta http-equiv="refresh" content="30; url=/n"></head><body>m</body></html>"#,
            "text/html",
        ))
        .mount(&mock)
        .await;
    mount_html(&mock, "/n2", "<p>n2</p>").await;
    mock
}

/// A browser fetch later in a chain may follow only the redirects the chain has left.
#[tokio::test]
async fn a_later_browser_hop_gets_only_the_redirects_the_chain_has_left() {
    let test_name = "a_later_browser_hop_gets_only_the_redirects_the_chain_has_left";
    let http_site = chain_with_a_meta_refresh().await;
    let http = crawl_with(
        test_name,
        config(BrowserMode::Never, 3),
        &format!("{}/", http_site.uri()),
    )
    .await
    .expect("HTTP mode needs no Chrome");
    let http_outcome = chain_outcome(&http, &http_site.uri());
    assert_eq!(
        http_outcome,
        (3, "/n".to_owned(), 301),
        "HTTP mode stops on the 3xx at the limit"
    );

    let browser_site = chain_with_a_meta_refresh().await;
    let Some(browser) = crawl_with(
        test_name,
        config(BrowserMode::Always, 3),
        &format!("{}/", browser_site.uri()),
    )
    .await
    else {
        return;
    };
    assert_eq!(chain_outcome(&browser, &browser_site.uri()), http_outcome);
    let requested = requested_paths(&browser_site).await;
    assert!(
        !requested.iter().any(|p| p == "/n1"),
        "Chrome must not follow the second chain past the limit, requested: {requested:?}"
    );
}

/// `scrape()` reaches Chrome through the redirect chain, or directly when a screenshot is
/// requested; both stop at the limit.
#[tokio::test]
async fn scrape_stops_at_max_redirects_with_and_without_a_screenshot() {
    for capture_screenshot in [false, true] {
        let site = redirect_chain(5).await;
        let config = CrawlConfig {
            capture_screenshot,
            ..config(BrowserMode::Always, 2)
        };
        let engine = create_engine(Some(config)).expect("engine must build");
        let result = match scrape(&engine, &format!("{}/", site.uri())).await {
            Ok(result) => result,
            Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
                announce_chrome_skip("scrape_stops_at_max_redirects_with_and_without_a_screenshot", &message);
                return;
            }
            Err(error) => panic!("scrape must succeed: {error:?}"),
        };
        assert_eq!(
            (result.status_code, result.final_url.trim_start_matches(&site.uri())),
            (301, "/r2"),
            "capture_screenshot={capture_screenshot}: the scrape must stop on the 3xx at the limit"
        );
        let requested = requested_paths(&site).await;
        assert!(
            !requested.iter().any(|p| p == "/r3"),
            "capture_screenshot={capture_screenshot}: Chrome must not request past the limit, requested: {requested:?}"
        );
    }
}

/// Only the page's own navigation is limited: a redirect inside an iframe does not count.
///
/// ~keep The seed lands on itself here, so the redirect chain would report `(0, "/", 200)` even
/// ~keep if Chrome were never limited. What turns this red is an iframe navigation counted
/// ~keep against the seed: at `max_redirects = 0` the check then drops the iframe's request or its
/// ~keep redirect, and the iframe never reaches `/f` or `/f1`.
#[tokio::test]
async fn a_redirect_inside_an_iframe_does_not_count() {
    let site = MockServer::start().await;
    mount_html(&site, "/", r#"<p id="top">top</p><iframe src="/f"></iframe>"#).await;
    Mock::given(method("GET"))
        .and(path("/f"))
        .respond_with(ResponseTemplate::new(301).append_header("location", "/f1"))
        .mount(&site)
        .await;
    mount_html(&site, "/f1", "<p>frame</p>").await;

    let Some(result) = crawl_with(
        "a_redirect_inside_an_iframe_does_not_count",
        config(BrowserMode::Always, 0),
        &format!("{}/", site.uri()),
    )
    .await
    else {
        return;
    };

    let requested = requested_paths(&site).await;
    assert!(
        requested.iter().any(|p| p == "/f") && requested.iter().any(|p| p == "/f1"),
        "Chrome must follow the iframe's redirect to its target, requested: {requested:?}"
    );
    assert_eq!(chain_outcome(&result, &site.uri()), (0, "/".to_owned(), 200));
}

/// A script that navigates while the page is still parsing counts as one redirect. Past the
/// limit the crawl and the scrape end on the seed's document, as far as Chrome parsed it.
///
/// ~keep Chrome stops parsing a page whose script navigates, so once the check drops that
/// ~keep navigation the seed never fires its load event, and a fetch that waits for it runs into
/// ~keep the browser timeout.
#[tokio::test]
async fn a_script_navigation_while_the_page_parses_counts_as_one_redirect() {
    let test_name = "a_script_navigation_while_the_page_parses_counts_as_one_redirect";
    let site = MockServer::start().await;
    mount_html(
        &site,
        "/",
        r#"<p id="seed">seed</p><script>location.replace('/n')</script>"#,
    )
    .await;
    mount_html(&site, "/n", r#"<p id="landed">landed</p>"#).await;
    let seed = format!("{}/", site.uri());

    let Some(stopped) = crawl_with(test_name, config(BrowserMode::Always, 0), &seed).await else {
        return;
    };
    assert_eq!(
        chain_outcome(&stopped, &site.uri()),
        (0, "/".to_owned(), 200),
        "past the limit the crawl must end on the seed"
    );
    let page = seed_page(&stopped);
    assert!(
        page.html.contains("id=\"seed\""),
        "the crawl must keep the seed's document: {}",
        page.html
    );

    let engine = create_engine(Some(config(BrowserMode::Always, 0))).expect("engine must build");
    let scraped = scrape(&engine, &seed)
        .await
        .expect("past the limit the scrape must end on the seed");
    assert_eq!(
        (scraped.status_code, scraped.final_url.trim_start_matches(&site.uri())),
        (200, "/"),
        "past the limit the scrape must end on the seed"
    );
    assert!(
        scraped.html.contains("id=\"seed\""),
        "the scrape must keep the seed's document: {}",
        scraped.html
    );
    let requested = requested_paths(&site).await;
    assert!(
        !requested.iter().any(|p| p == "/n"),
        "Chrome must not follow the navigation past the limit, requested: {requested:?}"
    );

    let Some(followed) = crawl_with(test_name, config(BrowserMode::Always, 1), &seed).await else {
        return;
    };
    assert_eq!(
        chain_outcome(&followed, &site.uri()),
        (1, "/n".to_owned(), 200),
        "within the limit the navigation counts as one redirect"
    );
}

/// A pooled page kept for session reuse serves the next fetch on the same site at once, after
/// a script navigated it past the limit while it parsed.
///
/// ~keep The first fetch ends on the seed without the load event chromiumoxide waits for, and
/// ~keep chromiumoxide keeps that navigation open for 30 s. A page parked in that state holds the
/// ~keep next fetch's navigation until the browser timeout.
#[tokio::test]
async fn the_next_pooled_fetch_on_the_site_is_prompt_after_a_script_navigation_past_the_limit() {
    let test_name = "the_next_pooled_fetch_on_the_site_is_prompt_after_a_script_navigation_past_the_limit";
    let site = MockServer::start().await;
    mount_html(
        &site,
        "/",
        r#"<p id="seed">seed</p><script>location.replace('/n')</script>"#,
    )
    .await;
    mount_html(&site, "/next", r#"<p id="landed">landed</p>"#).await;

    let mut config = config(BrowserMode::Always, 0);
    config.browser.session_affinity = true;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    config.browser_pool = Some(Arc::clone(&pool));
    config.browser_session_pool = Some(Arc::new(BrowserSessionPool::new()));
    let engine = create_engine(Some(config)).expect("engine must build");

    match scrape(&engine, &format!("{}/", site.uri())).await {
        Ok(first) => assert!(first.html.contains("id=\"seed\""), "{}", first.html),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            pool.shutdown().await;
            return;
        }
        Err(error) => panic!("{test_name}: the first scrape must end on the seed: {error:?}"),
    }
    let started = Instant::now();
    let next = scrape(&engine, &format!("{}/next", site.uri())).await;
    let elapsed = started.elapsed();
    pool.shutdown().await;
    let next = next.expect("the next scrape on the site must succeed");
    assert!(next.html.contains("landed"), "{}", next.html);
    assert!(
        elapsed < Duration::from_secs(10),
        "the next scrape must not wait on the first page's navigation, took {elapsed:?}"
    );
}
