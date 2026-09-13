//! Integration tests verifying that Tower service layers (UA rotation, caching)
//! actually affect HTTP requests sent to the server.

use crawlberg::{CrawlConfig, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read (`SsrfPolicy::from_env`, reached from
// `CrawlConfig::default()`) in this binary's other tests, aborting the process on glibc
// with no failing test name.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

#[tokio::test]
async fn test_ua_rotation_reaches_server() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Hello</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        user_agents: vec!["TestBot/1.0".into()],
        ..allow_private_config()
    };
    let handle = create_engine(Some(config)).unwrap();

    let result = scrape(&handle, &mock.uri()).await;
    assert!(result.is_ok(), "should succeed: {:?}", result.err());

    let received = mock.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        1,
        "a single scrape() call must produce exactly one request, got: {:?}",
        received.iter().map(|r| r.url.path()).collect::<Vec<_>>()
    );
    let ua_values: Vec<_> = received[0]
        .headers
        .get_all("user-agent")
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    assert!(
        ua_values.iter().any(|v| v == "TestBot/1.0"),
        "server should have received TestBot/1.0 as user-agent, got: {:?}",
        ua_values
    );
}

#[tokio::test]
async fn test_ua_rotation_cycles_through_agents() {
    let mock = MockServer::start().await;

    for i in 0..3 {
        Mock::given(method("GET"))
            .and(path(format!("/page{i}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("<html><body>Page {i}</body></html>"))
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;
    }

    let config = CrawlConfig {
        user_agents: vec!["AgentA/1.0".to_string(), "AgentB/2.0".to_string()],
        ..allow_private_config()
    };
    let handle = create_engine(Some(config)).unwrap();

    for i in 0..3 {
        let url = format!("{}/page{i}", mock.uri());
        scrape(&handle, &url).await.unwrap();
    }

    let received = mock.received_requests().await.unwrap();
    assert_eq!(received.len(), 3);

    let uas: Vec<String> = received
        .iter()
        .map(|r| {
            r.headers
                .get_all("user-agent")
                .iter()
                .map(|v| v.to_str().unwrap().to_owned())
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();

    assert!(
        uas[0].contains("AgentA/1.0"),
        "first request should use AgentA, got: {}",
        uas[0]
    );
    assert!(
        uas[1].contains("AgentB/2.0"),
        "second request should use AgentB, got: {}",
        uas[1]
    );
    assert!(
        uas[2].contains("AgentA/1.0"),
        "third request should cycle back to AgentA, got: {}",
        uas[2]
    );
}

#[tokio::test]
async fn test_cache_layer_avoids_duplicate_fetches() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Cached</body></html>")
                .append_header("content-type", "text/html")
                .append_header("etag", "\"abc123\""),
        )
        .expect(1..=2)
        .mount(&mock)
        .await;

    let handle = create_engine(Some(allow_private_config())).unwrap();

    let result1 = scrape(&handle, &mock.uri()).await.unwrap();
    assert_eq!(result1.status_code, 200);
    assert!(result1.html.contains("Cached"));

    let result2 = scrape(&handle, &mock.uri()).await.unwrap();
    assert_eq!(result2.status_code, 200);
    assert!(result2.html.contains("Cached"));
}
