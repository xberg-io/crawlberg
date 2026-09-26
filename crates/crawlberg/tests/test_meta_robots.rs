//! Integration tests for the page's own robots instructions during a crawl: the robots meta
//! tag and every `X-Robots-Tag` header. A `rel="nofollow"` link is a hint and is still followed.
//!
//! Every target a crawl must not follow is mounted with `.expect(0)`, so the assertion is that
//! the server never saw the request, not only that the page is missing from the result.

use std::time::Duration;

use crawlberg::{CrawlConfig, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount an HTML page with extra response headers, expecting exactly `expected` requests.
async fn mount_page(mock: &MockServer, at: &str, body: &str, headers: &[(&str, &str)], expected: u64) {
    let mut response = ResponseTemplate::new(200)
        .set_body_string(body.to_owned())
        .append_header("content-type", "text/html");
    for (name, value) in headers {
        response = response.append_header(*name, *value);
    }
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .expect(expected)
        .mount(mock)
        .await;
}

/// A 404 robots.txt allows everything, so only the page's own instructions decide.
async fn mount_absent_robots_txt(mock: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(mock)
        .await;
}

fn config(respect_robots_txt: bool) -> CrawlConfig {
    CrawlConfig::builder()
        .respect_robots_txt(respect_robots_txt)
        .allow_private_networks(true)
        .max_depth(2)
        .max_pages(10)
        .request_timeout(Duration::from_secs(5))
        .build()
}

const NOFOLLOW_PAGE: &str = r#"<html><head><meta name="robots" content="noindex, nofollow"></head>
<body><a href="/child">child</a></body></html>"#;

const REL_NOFOLLOW_PAGE: &str = r#"<html><body>
<a href="/nf" rel="nofollow">sponsored</a><a href="/ok">ok</a></body></html>"#;

#[tokio::test]
async fn should_not_follow_links_from_a_meta_nofollow_page_when_respecting_robots() {
    let mock = MockServer::start().await;
    mount_absent_robots_txt(&mock).await;
    mount_page(&mock, "/", NOFOLLOW_PAGE, &[], 1).await;
    mount_page(&mock, "/child", "<html><body>child</body></html>", &[], 0).await;

    let engine = create_engine(Some(config(true))).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(result.pages.len(), 1, "only the nofollow seed may be crawled");
    drop(mock);
}

#[tokio::test]
async fn should_not_follow_links_from_an_x_robots_tag_nofollow_page_when_respecting_robots() {
    let mock = MockServer::start().await;
    mount_absent_robots_txt(&mock).await;
    mount_page(
        &mock,
        "/",
        r#"<html><body><a href="/child">child</a></body></html>"#,
        &[("x-robots-tag", "nofollow")],
        1,
    )
    .await;
    mount_page(&mock, "/child", "<html><body>child</body></html>", &[], 0).await;

    let engine = create_engine(Some(config(true))).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(result.pages.len(), 1, "only the nofollow seed may be crawled");
    drop(mock);
}

/// A `rel="nofollow"` link is a hint, not a robots directive, so it is still followed.
///
/// ~keep A guard, not evidence the fix works: `REL_NOFOLLOW_PAGE` carries no robots meta tag and
/// ~keep no `X-Robots-Tag`, so the suppression conjunct never fires and this passes with the
/// ~keep production change reverted. It exists to catch the plausible mis-implementation -- also
/// ~keep the literal wording of #135 -- of filtering on `LinkInfo::nofollow` as well, which would
/// ~keep make `crawl()` contradict `map()`. Keep it; do not read it as coverage of the fix.
#[tokio::test]
async fn should_follow_a_rel_nofollow_link_when_respecting_robots() {
    let mock = MockServer::start().await;
    mount_absent_robots_txt(&mock).await;
    mount_page(&mock, "/", REL_NOFOLLOW_PAGE, &[], 1).await;
    mount_page(&mock, "/nf", "<html><body>nf</body></html>", &[], 1).await;
    mount_page(&mock, "/ok", "<html><body>ok</body></html>", &[], 1).await;

    let engine = create_engine(Some(config(true))).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(
        result.pages.len(),
        3,
        "rel=\"nofollow\" is a link hint, not a robots directive: both links must be crawled"
    );
    drop(mock);
}

#[tokio::test]
async fn should_not_follow_links_when_a_second_x_robots_tag_header_says_nofollow() {
    let mock = MockServer::start().await;
    mount_absent_robots_txt(&mock).await;
    mount_page(
        &mock,
        "/",
        r#"<html><body><a href="/child">child</a></body></html>"#,
        &[("x-robots-tag", "noarchive"), ("x-robots-tag", "nofollow")],
        1,
    )
    .await;
    mount_page(&mock, "/child", "<html><body>child</body></html>", &[], 0).await;

    let engine = create_engine(Some(config(true))).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(result.pages.len(), 1, "only the nofollow seed may be crawled");
    assert!(result.pages[0].nofollow_detected, "the seed must be marked nofollow");
    drop(mock);
}

/// With `respect_robots_txt` off -- the default -- a nofollow page's links are still followed.
///
/// ~keep A guard, not evidence the fix works: `config(false)` makes the suppression conjunct
/// ~keep `!(false && _)`, so this passes with the production change reverted. It exists to catch
/// ~keep a mis-implementation that applied nofollow unconditionally, which would silently narrow
/// ~keep every default-configured crawl. Keep it; do not read it as coverage of the fix.
#[tokio::test]
async fn should_follow_nofollow_links_when_not_respecting_robots() {
    let mock = MockServer::start().await;
    mount_page(
        &mock,
        "/",
        r#"<html><head><meta name="robots" content="noindex, nofollow"></head>
<body><a href="/child">child</a><a href="/nf" rel="nofollow">nf</a></body></html>"#,
        &[],
        1,
    )
    .await;
    mount_page(&mock, "/child", "<html><body>child</body></html>", &[], 1).await;
    mount_page(&mock, "/nf", "<html><body>nf</body></html>", &[], 1).await;

    let engine = create_engine(Some(config(false))).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(result.pages.len(), 3, "robots off: every link must be followed");
    drop(mock);
}

#[tokio::test]
async fn should_crawl_and_mark_a_noindex_page_and_follow_its_links_when_respecting_robots() {
    let mock = MockServer::start().await;
    mount_absent_robots_txt(&mock).await;
    mount_page(
        &mock,
        "/",
        r#"<html><head><meta name="robots" content="noindex"></head>
<body><a href="/child">child</a></body></html>"#,
        &[],
        1,
    )
    .await;
    mount_page(&mock, "/child", "<html><body>child</body></html>", &[], 1).await;

    let engine = create_engine(Some(config(true))).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(result.pages.len(), 2, "a noindex page's links must still be followed");
    let seed = &result.pages[0];
    assert!(seed.noindex_detected, "the noindex seed must be marked");
    assert!(!seed.nofollow_detected, "the noindex seed must not be marked nofollow");
    assert!(!result.pages[1].noindex_detected, "the plain child must not be marked");
    drop(mock);
}

#[tokio::test]
async fn should_mark_a_nofollow_page_even_when_not_respecting_robots() {
    let mock = MockServer::start().await;
    mount_page(&mock, "/", NOFOLLOW_PAGE, &[], 1).await;
    mount_page(&mock, "/child", "<html><body>child</body></html>", &[], 1).await;

    let engine = create_engine(Some(config(false))).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    let seed = &result.pages[0];
    assert!(
        seed.noindex_detected && seed.nofollow_detected,
        "the seed must carry both marks"
    );
    drop(mock);
}
