//! With `cookies_enabled`, the Chrome pages of one crawl send the cookies its earlier pages
//! set, as HTTP mode does, and no other crawl gets them.
//!
//! Requires a real Chrome binary; skipped (not failed) when Chrome is unavailable, matching
//! the other browser tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlEngineHandle, CrawlResult, batch_crawl, crawl,
    create_engine,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn config(mode: BrowserMode, cookies_enabled: bool) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        cookies_enabled,
        respect_robots_txt: false,
        stay_on_domain: false,
        max_depth: Some(3),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

fn browser_engine() -> CrawlEngineHandle {
    create_engine(Some(config(BrowserMode::Always, true))).expect("engine must build")
}

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html")
}

async fn route(site: &MockServer, at: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .mount(site)
        .await;
}

/// The cookies `request` carries, as `name=value` pairs in name order.
fn cookies_of(request: &Request) -> Vec<String> {
    let header = request
        .headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let mut pairs: Vec<String> = header
        .split("; ")
        .filter(|pair| !pair.is_empty())
        .map(str::to_owned)
        .collect();
    pairs.sort();
    pairs
}

/// `/members`, which answers 200 to a request with the cookie `session=1` and 403 to any other.
async fn members_route(site: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/members"))
        .respond_with(|request: &Request| {
            if cookies_of(request).iter().any(|pair| pair == "session=1") {
                html("members page words")
            } else {
                ResponseTemplate::new(403).set_body_raw("<html><body>no cookie</body></html>", "text/html")
            }
        })
        .mount(site)
        .await;
}

/// A site whose start page sets the cookie `session=1` and links to `/members`.
async fn members_site() -> MockServer {
    let site = MockServer::start().await;
    route(
        &site,
        "/",
        html(r#"<a href="/members">members</a>"#).append_header("set-cookie", "session=1; Path=/"),
    )
    .await;
    members_route(&site).await;
    site
}

/// The cookies of each request for `at` the site received, in the order they arrived.
async fn cookies_sent_to(site: &MockServer, at: &str) -> Vec<Vec<String>> {
    site.received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .filter(|request| request.url.path() == at)
        .map(cookies_of)
        .collect()
}

/// The pages of a crawl as `(path, status)` in path order, or `None` when Chrome is missing.
fn pages(test_name: &str, result: Result<CrawlResult, crawlberg::CrawlError>) -> Option<Vec<(String, u16)>> {
    let result = match result {
        Ok(result) => result,
        Err(crawlberg::CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return None;
        }
        Err(error) => panic!("{test_name}: the crawl must run: {error:?}"),
    };
    if let Some(message) = result.error.as_deref()
        && is_missing_chrome_message(message)
    {
        announce_chrome_skip(test_name, message);
        return None;
    }
    let mut pages: Vec<(String, u16)> = result
        .pages
        .iter()
        .map(|page| {
            let path = url::Url::parse(&page.url).map(|url| url.path().to_owned());
            (path.unwrap_or_else(|_| page.url.clone()), page.status_code)
        })
        .collect();
    pages.sort();
    Some(pages)
}

fn owned(pages: &[(&str, u16)]) -> Vec<(String, u16)> {
    pages.iter().map(|(path, status)| ((*path).to_owned(), *status)).collect()
}

/// The start page sets a cookie and the page it links to answers 403 without it
/// (xberg-io/crawlberg#603). HTTP mode on the same site is the comparison.
#[tokio::test]
async fn a_browser_crawl_sends_the_cookie_the_start_page_set() {
    let test_name = "a_browser_crawl_sends_the_cookie_the_start_page_set";
    for mode in [BrowserMode::Never, BrowserMode::Always] {
        let site = members_site().await;
        let engine = create_engine(Some(config(mode.clone(), true))).expect("engine must build");

        let Some(pages) = pages(test_name, crawl(&engine, &format!("{}/", site.uri())).await) else {
            return;
        };

        assert_eq!(
            cookies_sent_to(&site, "/members").await,
            [["session=1"]],
            "{mode:?}: the linked page must get the cookie the start page set"
        );
        assert_eq!(pages, owned(&[("/", 200), ("/members", 200)]), "{mode:?}");
    }
}

/// `cookies_enabled` is off by default, and then a crawl keeps no cookie, in either mode.
#[tokio::test]
async fn a_crawl_without_cookies_enabled_sends_no_cookie() {
    let test_name = "a_crawl_without_cookies_enabled_sends_no_cookie";
    for mode in [BrowserMode::Never, BrowserMode::Always] {
        let site = members_site().await;
        let engine = create_engine(Some(config(mode.clone(), false))).expect("engine must build");

        let Some(pages) = pages(test_name, crawl(&engine, &format!("{}/", site.uri())).await) else {
            return;
        };

        assert_eq!(
            cookies_sent_to(&site, "/members").await,
            [Vec::<String>::new()],
            "{mode:?}"
        );
        assert_eq!(pages, owned(&[("/", 200)]), "{mode:?}");
    }
}

/// The cookie of one crawl reaches no later crawl: not one of the same engine, and not one of
/// another engine with the same configuration.
#[tokio::test]
async fn a_later_crawl_does_not_send_the_cookie_of_an_earlier_crawl() {
    let test_name = "a_later_crawl_does_not_send_the_cookie_of_an_earlier_crawl";
    let site = members_site().await;
    let engine = browser_engine();
    if pages(test_name, crawl(&engine, &format!("{}/", site.uri())).await).is_none() {
        return;
    }

    let members = format!("{}/members", site.uri());
    let same_engine = crawl(&engine, &members).await;
    let other_engine = crawl(&browser_engine(), &members).await;

    assert_eq!(
        cookies_sent_to(&site, "/members").await,
        [vec!["session=1".to_owned()], vec![], vec![]],
        "only the crawl that received the cookie sends it; later crawls: {same_engine:?}, {other_engine:?}"
    );
}

/// Two crawls of one engine that run at the same time, on two sites of one host. Cookies do
/// not tell ports apart, so shared cookies would reach the other site.
#[tokio::test]
async fn two_crawls_at_the_same_time_do_not_share_cookies() {
    let test_name = "two_crawls_at_the_same_time_do_not_share_cookies";
    let mut sites = Vec::new();
    for name in ["first", "second"] {
        let site = MockServer::start().await;
        route(
            &site,
            "/",
            html(r#"<a href="/slow">slow</a>"#).append_header("set-cookie", format!("{name}=1; Path=/")),
        )
        .await;
        // ~keep Slow, so each crawl is still running when the other has its cookie.
        route(
            &site,
            "/slow",
            html(r#"<a href="/last">last</a>"#).set_delay(Duration::from_millis(500)),
        )
        .await;
        route(&site, "/last", html("last")).await;
        sites.push(site);
    }
    let engine = browser_engine();

    let seeds = sites.iter().map(|site| format!("{}/", site.uri())).collect();
    let results = batch_crawl(&engine, seeds).await.expect("the batch must run");
    for result in results.results {
        if let Some(message) = result.error.as_deref()
            && is_missing_chrome_message(message)
        {
            announce_chrome_skip(test_name, message);
            return;
        }
        let crawled = result
            .result
            .unwrap_or_else(|| panic!("{test_name}: the crawl must run: {:?}", result.error));
        let Some(pages) = pages(test_name, Ok(crawled)) else {
            return;
        };
        assert_eq!(pages, owned(&[("/", 200), ("/last", 200), ("/slow", 200)]));
    }

    for (site, own) in sites.iter().zip(["first=1", "second=1"]) {
        for at in ["/slow", "/last"] {
            assert_eq!(
                cookies_sent_to(site, at).await,
                [[own]],
                "{at} must get the cookie of its own crawl and no other"
            );
        }
    }
}

/// Chrome decides which request gets a cookie, by the attributes the cookie was set with.
#[tokio::test]
async fn a_carried_cookie_keeps_the_scope_it_was_set_with() {
    let test_name = "a_carried_cookie_keeps_the_scope_it_was_set_with";
    let site = MockServer::start().await;
    let other_host = format!("http://localhost:{}/other-host", site.address().port());
    // ~keep A crawl does not follow a link to another host, so a redirect leads there.
    let links = r#"<a href="/area/page">area</a><a href="/outside">outside</a><a href="/to-other-host">other host</a>"#;
    route(
        &site,
        "/to-other-host",
        ResponseTemplate::new(302).append_header("location", other_host.as_str()),
    )
    .await;
    route(
        &site,
        "/",
        html(links)
            .append_header("set-cookie", "plain=1; Path=/")
            .append_header("set-cookie", "area=1; Path=/area")
            .append_header("set-cookie", "hidden=1; Path=/; HttpOnly")
            .append_header("set-cookie", "strict=1; Path=/; SameSite=Strict")
            .append_header("set-cookie", "lasting=1; Path=/; Max-Age=3600")
            .append_header("set-cookie", "expired=1; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT"),
    )
    .await;
    for at in ["/area/page", "/outside", "/other-host"] {
        route(&site, at, html("page")).await;
    }

    let Some(pages) = pages(test_name, crawl(&browser_engine(), &format!("{}/", site.uri())).await) else {
        return;
    };

    assert_eq!(pages.len(), 4, "every page must be crawled: {pages:?}");
    assert_eq!(
        cookies_sent_to(&site, "/area/page").await,
        [["area=1", "hidden=1", "lasting=1", "plain=1", "strict=1"]]
    );
    assert_eq!(
        cookies_sent_to(&site, "/outside").await,
        [["hidden=1", "lasting=1", "plain=1", "strict=1"]]
    );
    assert_eq!(
        cookies_sent_to(&site, "/other-host").await,
        [Vec::<String>::new()],
        "a cookie of one host must not reach another host"
    );
}

/// A page deletes a cookie, a script sets one, and a page that answers 403 sets one. The crawl
/// loads one page at a time here, so each page ends before the next one starts.
#[tokio::test]
async fn later_pages_see_what_each_earlier_page_did_to_the_cookies() {
    let test_name = "later_pages_see_what_each_earlier_page_did_to_the_cookies";
    let site = MockServer::start().await;
    route(
        &site,
        "/",
        html(r#"<a href="/delete">delete</a>"#)
            .append_header("set-cookie", "deleted=1; Path=/")
            .append_header("set-cookie", "kept=1; Path=/"),
    )
    .await;
    route(
        &site,
        "/delete",
        html(r#"<a href="/refused">refused</a><a href="/script">script</a>"#)
            .append_header("set-cookie", "deleted=; Path=/; Max-Age=0"),
    )
    .await;
    route(
        &site,
        "/refused",
        ResponseTemplate::new(403)
            .set_body_raw("<html><body>refused</body></html>", "text/html")
            .append_header("set-cookie", "from-error=1; Path=/"),
    )
    .await;
    route(
        &site,
        "/script",
        html(r#"<script>document.cookie = "scripted=1; path=/"</script><a href="/last">last</a>"#),
    )
    .await;
    route(&site, "/last", html("last")).await;
    let engine = create_engine(Some(CrawlConfig {
        max_concurrent: Some(1),
        ..config(BrowserMode::Always, true)
    }))
    .expect("engine must build");

    let Some(pages) = pages(test_name, crawl(&engine, &format!("{}/", site.uri())).await) else {
        return;
    };

    assert_eq!(
        pages,
        owned(&[("/", 200), ("/delete", 200), ("/last", 200), ("/script", 200)])
    );
    assert_eq!(cookies_sent_to(&site, "/delete").await, [["deleted=1", "kept=1"]]);
    assert_eq!(cookies_sent_to(&site, "/refused").await, [["kept=1"]]);
    assert_eq!(cookies_sent_to(&site, "/script").await, [["from-error=1", "kept=1"]]);
    assert_eq!(
        cookies_sent_to(&site, "/last").await,
        [["from-error=1", "kept=1", "scripted=1"]]
    );
}

/// `/slow` starts with the first values and ends after `/setter` changed them. It changed
/// nothing itself, so the crawl keeps the values of `/setter`, for every kind of cookie.
#[tokio::test]
async fn a_page_that_ends_later_does_not_undo_the_change_of_another_page() {
    let test_name = "a_page_that_ends_later_does_not_undo_the_change_of_another_page";
    let kinds = [
        "session=V; Path=/",
        "hour=V; Path=/; Max-Age=3600",
        "century=V; Path=/; Max-Age=3153600000",
        "secure=V; Path=/; Secure",
        "hidden=V; Path=/; HttpOnly",
        "strict=V; Path=/; SameSite=Strict",
    ];
    let site = MockServer::start().await;
    let mut start = html(r#"<a href="/slow">slow</a><a href="/setter">setter</a><a href="/gate">gate</a>"#);
    let mut setter = html("setter").set_delay(Duration::from_millis(300));
    for kind in kinds {
        start = start.append_header("set-cookie", kind.replace('V', "first"));
        setter = setter.append_header("set-cookie", kind.replace('V', "changed"));
    }
    route(&site, "/", start).await;
    route(&site, "/setter", setter).await;
    route(&site, "/slow", html("slow").set_delay(Duration::from_millis(2500))).await;
    // ~keep `/check` starts after both pages ended: only the slowest page links to it.
    route(
        &site,
        "/gate",
        html(r#"<a href="/check">check</a>"#).set_delay(Duration::from_millis(4500)),
    )
    .await;
    route(&site, "/check", html("check")).await;

    let Some(pages) = pages(test_name, crawl(&browser_engine(), &format!("{}/", site.uri())).await) else {
        return;
    };

    assert_eq!(pages.len(), 5, "every page must be crawled: {pages:?}");
    assert_eq!(
        cookies_sent_to(&site, "/slow").await,
        [[
            "century=first",
            "hidden=first",
            "hour=first",
            "secure=first",
            "session=first",
            "strict=first"
        ]],
        "the slow page must start with the first values"
    );
    assert_eq!(
        cookies_sent_to(&site, "/check").await,
        [[
            "century=changed",
            "hidden=changed",
            "hour=changed",
            "secure=changed",
            "session=changed",
            "strict=changed"
        ]]
    );
}

/// Four pages load at the same time and each sets the cookie `shared` and one of its own. The
/// page they all link to starts after the first of them ended.
#[tokio::test]
async fn pages_that_set_the_same_cookie_at_the_same_time_leave_one_cookie() {
    let test_name = "pages_that_set_the_same_cookie_at_the_same_time_leave_one_cookie";
    let writers = ["/writer-1", "/writer-2", "/writer-3", "/writer-4"];
    let site = MockServer::start().await;
    let links: String = writers
        .iter()
        .map(|writer| format!(r#"<a href="{writer}">w</a>"#))
        .collect();
    route(&site, "/", html(&links)).await;
    for (index, writer) in writers.iter().enumerate() {
        route(
            &site,
            writer,
            html(r#"<a href="/after">after</a>"#)
                .append_header("set-cookie", format!("shared={index}; Path=/"))
                .append_header("set-cookie", format!("own-{index}=1; Path=/")),
        )
        .await;
    }
    route(&site, "/after", html(r#"<a href="/last">last</a>"#)).await;
    route(&site, "/last", html("last")).await;

    let Some(pages) = pages(test_name, crawl(&browser_engine(), &format!("{}/", site.uri())).await) else {
        return;
    };

    assert_eq!(pages.len(), 7, "every page must be crawled: {pages:?}");
    assert!(pages.iter().all(|(_, status)| *status == 200), "{pages:?}");
    for at in ["/after", "/last"] {
        let sent = cookies_sent_to(&site, at).await;
        let [sent] = sent.as_slice() else {
            panic!("{at} must be requested once: {sent:?}");
        };
        let shared = sent.iter().filter(|pair| pair.starts_with("shared=")).count();
        assert_eq!(shared, 1, "{at} must get one `shared` cookie: {sent:?}");
        assert!(
            sent.iter().any(|pair| pair.starts_with("own-")),
            "{at} must get the cookie of a page that ended before it started: {sent:?}"
        );
    }
}
