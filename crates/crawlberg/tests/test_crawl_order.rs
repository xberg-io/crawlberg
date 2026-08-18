//! Integration tests for crawl *visit order*: that the default strategy is genuinely
//! breadth-first once the engine removes the selected entry order-preservingly and
//! enqueues discovered links in document order.

use crawlberg::{CrawlConfig, CrawlEngine, CrawlResult, LifoFrontier};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mount_html(mock: &MockServer, at: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

/// Root links to `/a`, `/b`, `/c` in that document order; each child links to exactly one
/// grandchild (`/a1`, `/b1`, `/c1`). A depth-2 tree of 7 pages.
async fn setup_branching_mock() -> MockServer {
    let mock = MockServer::start().await;

    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/a">A</a><a href="/b">B</a><a href="/c">C</a></body></html>"#.to_owned(),
    )
    .await;

    for child in ["a", "b", "c"] {
        mount_html(
            &mock,
            &format!("/{child}"),
            format!(r#"<html><body><a href="/{child}1">{child}1</a></body></html>"#),
        )
        .await;
        mount_html(
            &mock,
            &format!("/{child}1"),
            format!("<html><body>leaf {child}1</body></html>"),
        )
        .await;
    }

    mock
}

/// Map each visited page to its server-relative path, preserving visit order.
fn visited_paths(result: &CrawlResult, base: &str) -> Vec<String> {
    result
        .pages
        .iter()
        .map(|page| match page.url.strip_prefix(base).unwrap_or(&page.url) {
            "" => "/".to_owned(),
            rest => rest.to_owned(),
        })
        .collect()
}

/// Regression test for the breadth-first traversal defect.
///
/// The engine removed the strategy-selected entry with `Vec::swap_remove`, which moves the
/// last element into the vacated slot. `BfsStrategy` always selects index 0, so after the
/// first removal index 0 held the newest sibling and the crawl went `/ -> /a -> /c`,
/// skipping `/b` entirely. Discovered links additionally reached the queue in SSRF
/// validation *completion* order, so sibling order was nondeterministic on top of that.
///
/// ~keep `max_concurrent = 1` is required, not incidental: `CrawlResult.pages` is filled in
/// fetch-completion order, so a positional assertion is only deterministic with a single
/// fetch in flight.
#[tokio::test]
async fn should_visit_siblings_in_breadth_first_order_when_strategy_is_bfs() {
    let mock = setup_branching_mock().await;
    let base = mock.uri();

    let config = CrawlConfig {
        max_depth: Some(2),
        max_pages: Some(3),
        max_concurrent: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let engine = CrawlEngine::builder()
        .config(config)
        .build()
        .expect("engine must build");

    let result = engine.crawl(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited_paths(&result, &base),
        vec!["/".to_owned(), "/a".to_owned(), "/b".to_owned()],
        "BfsStrategy must visit the root then its siblings in document order; `/c` in \
         position 2 means the selected entry was removed with swap_remove, which moves the \
         newest sibling into index 0"
    );
}

/// The whole depth-1 level must be visited before any depth-2 page, which is what
/// breadth-first means beyond mere sibling ordering.
#[tokio::test]
async fn should_exhaust_each_depth_level_before_descending_when_strategy_is_bfs() {
    let mock = setup_branching_mock().await;
    let base = mock.uri();

    let config = CrawlConfig {
        max_depth: Some(2),
        max_pages: Some(4),
        max_concurrent: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let engine = CrawlEngine::builder()
        .config(config)
        .build()
        .expect("engine must build");

    let result = engine.crawl(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited_paths(&result, &base),
        vec!["/".to_owned(), "/a".to_owned(), "/b".to_owned(), "/c".to_owned()],
        "all three depth-1 siblings must be visited before any grandchild"
    );
}

/// Depth-first traversal is a property of the frontier, not of the strategy.
///
/// The engine passes its bounded selection window to `CrawlStrategy::select_next`, so a
/// strategy can only reorder what has already been popped; global order comes from the
/// frontier's own queue discipline. A LIFO frontier must therefore descend into a child
/// before visiting its remaining siblings.
#[tokio::test]
async fn should_visit_deepest_branch_first_when_frontier_is_lifo() {
    let mock = setup_branching_mock().await;
    let base = mock.uri();

    let config = CrawlConfig {
        max_depth: Some(2),
        max_pages: Some(3),
        max_concurrent: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let engine = CrawlEngine::builder()
        .config(config)
        .frontier(LifoFrontier::new())
        .build()
        .expect("engine must build");

    let result = engine.crawl(&base).await.expect("crawl must succeed");

    assert_eq!(
        visited_paths(&result, &base),
        vec!["/".to_owned(), "/c".to_owned(), "/c1".to_owned()],
        "a LIFO frontier must descend into the newest entry's child before returning to its \
         siblings"
    );
}
