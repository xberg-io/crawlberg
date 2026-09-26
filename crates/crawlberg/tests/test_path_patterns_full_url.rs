//! Integration tests for crawlberg#78: `include_paths`/`exclude_paths` matching the full URL,
//! and look-around patterns compiling instead of refusing the whole configuration.

use crawlberg::{CrawlConfig, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SEED_BODY: &str = r#"<html><body><a href="/private/x">x</a><a href="/public/y">y</a></body></html>"#;

async fn mount_site(mock: &MockServer) {
    for (at, body) in [
        ("/", SEED_BODY),
        ("/private/x", "<html><body>private</body></html>"),
        ("/public/y", "<html><body>public</body></html>"),
    ] {
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
}

/// Every path the server was asked for, sorted.
async fn requested_paths(mock: &MockServer) -> Vec<String> {
    let mut paths: Vec<String> = mock
        .received_requests()
        .await
        .expect("the mock server records its requests")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    paths.sort();
    paths
}

/// Crawl the three-page site with `config` and return the paths the server saw.
async fn crawl_site(config: CrawlConfig) -> Vec<String> {
    let mock = MockServer::start().await;
    mount_site(&mock).await;
    let engine = create_engine(Some(config)).expect("engine builds");
    crawlberg::crawl(&engine, &format!("{}/", mock.uri()))
        .await
        .expect("crawl runs");
    requested_paths(&mock).await
}

fn excluding(pattern: &str) -> crawlberg::CrawlConfigBuilder {
    CrawlConfig::builder()
        .allow_private_networks(true)
        .max_depth(1)
        .max_pages(10)
        .exclude_paths(vec![pattern.to_owned()])
}

/// A pattern anchored on scheme and host excludes `/private/x` once patterns see the full URL.
#[tokio::test]
async fn host_anchored_exclude_pattern_matches_when_path_patterns_match_url_is_set() {
    let config = excluding(r"^https?://127\.0\.0\.1:\d+/private/")
        .path_patterns_match_url(true)
        .build();

    assert_eq!(crawl_site(config).await, ["/", "/public/y"]);
}

/// Characterizes the default: patterns see the path alone, so a host-anchored one never matches.
#[tokio::test]
async fn host_anchored_exclude_pattern_never_matches_by_default() {
    let config = excluding(r"^https?://127\.0\.0\.1:\d+/private/").build();

    assert_eq!(crawl_site(config).await, ["/", "/private/x", "/public/y"]);
}

/// `path_patterns_match_url` takes precedence over `path_patterns_match_query`.
#[tokio::test]
async fn path_patterns_match_url_wins_over_path_patterns_match_query() {
    let config = excluding(r"^https?://127\.0\.0\.1:\d+/private/")
        .path_patterns_match_query(true)
        .path_patterns_match_url(true)
        .build();

    assert_eq!(crawl_site(config).await, ["/", "/public/y"]);
}

/// The issue's path-only pattern keeps excluding `/private/x`.
#[tokio::test]
async fn path_only_exclude_pattern_still_matches() {
    let config = excluding("/private/").path_patterns_match_query(true).build();

    assert_eq!(crawl_site(config).await, ["/", "/public/y"]);
}

/// A negative look-ahead compiles, so the engine builds and the pattern excludes what it names.
#[tokio::test]
async fn negative_look_ahead_exclude_pattern_builds_the_engine_and_matches() {
    let config = excluding("^/(?!public/).").build();

    assert_eq!(crawl_site(config).await, ["/", "/public/y"]);
}

/// A look-ahead `include_paths` pattern admits only what it names.
#[tokio::test]
async fn look_ahead_include_pattern_admits_only_matching_urls() {
    let config = CrawlConfig::builder()
        .allow_private_networks(true)
        .max_depth(1)
        .max_pages(10)
        .include_paths(vec!["^/(?=public/)".to_owned()])
        .build();

    assert_eq!(crawl_site(config).await, ["/", "/public/y"]);
}

/// A host-anchored `include_paths` pattern admits what it names once patterns see the full URL.
#[tokio::test]
async fn host_anchored_include_pattern_admits_when_path_patterns_match_url_is_set() {
    let config = CrawlConfig::builder()
        .allow_private_networks(true)
        .max_depth(1)
        .max_pages(10)
        .include_paths(vec![r"^https?://127\.0\.0\.1:\d+/public/".to_owned()])
        .path_patterns_match_url(true)
        .build();

    assert_eq!(crawl_site(config).await, ["/", "/public/y"]);
}

/// A redirect target is judged against the full URL too, so a host-anchored pattern stops the
/// hop before it is requested.
#[tokio::test]
async fn redirect_target_matching_a_host_anchored_exclude_pattern_is_never_requested() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/private/x"))
        .mount(&mock)
        .await;
    mount_site(&mock).await;
    let config = excluding(r"^https?://127\.0\.0\.1:\d+/private/")
        .path_patterns_match_url(true)
        .build();
    let engine = create_engine(Some(config)).expect("engine builds");

    let result = crawlberg::crawl(&engine, &format!("{}/", mock.uri()))
        .await
        .expect("crawl runs");

    assert_eq!(requested_paths(&mock).await, ["/"]);
    assert!(result.pages.is_empty(), "a filtered redirect target yields no page");
}

/// A pattern that still fails to compile refuses the configuration and names the pattern.
#[test]
fn invalid_pattern_still_refuses_the_engine_and_names_the_pattern() {
    let config = CrawlConfig::builder()
        .exclude_paths(vec!["(?<=a+)b(".to_owned()])
        .build();

    let Err(error) = create_engine(Some(config)) else {
        panic!("an invalid pattern must refuse the engine");
    };
    assert!(
        error.to_string().contains("(?<=a+)b("),
        "the error must name the offending pattern, got: {error}"
    );
}
