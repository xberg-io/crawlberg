//! `ProxyProvider` end-to-end: the engine routes reqwest fetches through the
//! injected provider per request.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crawlberg::{CrawlConfig, ProxyConfig, ProxyProvider, StaticProxyProvider, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn static_provider_round_robin() {
    let provider = StaticProxyProvider::new(vec![
        ProxyConfig {
            url: "http://p1:8080".into(),
            username: None,
            password: None,
        },
        ProxyConfig {
            url: "http://p2:8080".into(),
            username: None,
            password: None,
        },
    ]);
    assert_eq!(provider.next_proxy("a").unwrap().url, "http://p1:8080");
    assert_eq!(provider.next_proxy("b").unwrap().url, "http://p2:8080");
    assert_eq!(provider.next_proxy("c").unwrap().url, "http://p1:8080");
}

#[derive(Debug)]
struct CountingProvider {
    inner: StaticProxyProvider,
    calls: AtomicUsize,
}

impl ProxyProvider for CountingProvider {
    fn next_proxy(&self, host: &str) -> Option<ProxyConfig> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.next_proxy(host)
    }
}

#[tokio::test]
async fn engine_invokes_provider_per_fetch() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>ok</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let provider = Arc::new(CountingProvider {
        inner: StaticProxyProvider::empty(),
        calls: AtomicUsize::new(0),
    });

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(0),
        proxy_provider: Some(provider.clone()),
        ..base
    };
    let handle = create_engine(Some(config)).expect("engine should build");

    let uri = mock.uri();
    let result = crawl(&handle, &uri).await.expect("crawl should succeed");
    assert!(!result.pages.is_empty(), "page should have been fetched");
    assert!(
        provider.calls.load(Ordering::Relaxed) >= 1,
        "provider must be called at least once per fetch, got {}",
        provider.calls.load(Ordering::Relaxed)
    );
}

/// A mock that serves as a proxy. It answers the absolute-form request a proxy gets.
async fn mock_proxy() -> MockServer {
    let proxy = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>through a proxy</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&proxy)
        .await;
    proxy
}

fn basic(user: &str, password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

/// The `Proxy-Authorization` value of every request `proxy` received.
async fn authorizations(proxy: &MockServer) -> Vec<String> {
    let requests = proxy.received_requests().await.expect("the mock records requests");
    requests
        .iter()
        .map(|request| {
            request
                .headers
                .get("proxy-authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<none>")
                .to_owned()
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    Scrape,
    Map,
    Crawl,
}

/// Runs `operation` 4 times through a provider that rotates over two proxies, A and B, each with
/// its own credentials.
///
/// ~keep Each request must reach one proxy with that proxy's own credentials, and the provider
/// ~keep must be asked exactly once for each request. A provider asked more than once for one
/// ~keep request can hand the credentials of one proxy to the other, or send a request over a
/// ~keep connection to a proxy it did not pick.
async fn assert_one_pick_per_request(operation: Operation) {
    let site = MockServer::start().await;
    let (a, b) = (mock_proxy().await, mock_proxy().await);
    let proxy = |mock: &MockServer, user: &str, password: &str| ProxyConfig {
        url: mock.uri(),
        username: Some(user.to_owned()),
        password: Some(password.to_owned()),
    };
    let provider = Arc::new(CountingProvider {
        inner: StaticProxyProvider::new(vec![proxy(&a, "user-a", "pw-a"), proxy(&b, "user-b", "pw-b")]),
        calls: AtomicUsize::new(0),
    });
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_depth = Some(0);
    config.browser.mode = crawlberg::BrowserMode::Never;
    let engine = crawlberg::CrawlEngine::builder()
        .config(config)
        .with_proxy_provider(provider.clone())
        .build()
        .expect("a proxy provider is a valid config");

    for _ in 0..4 {
        let outcome = match operation {
            Operation::Scrape => engine.scrape(&site.uri()).await.map(drop),
            Operation::Map => engine.map(&site.uri()).await.map(drop),
            Operation::Crawl => engine.crawl(&site.uri()).await.map(drop),
        };
        outcome.unwrap_or_else(|error| panic!("{operation:?} through the proxies failed: {error}"));
    }

    let (at_a, at_b) = (authorizations(&a).await, authorizations(&b).await);
    let direct = site.received_requests().await.expect("the mock records requests").len();
    let calls = provider.calls.load(Ordering::Relaxed);
    assert!(
        at_a.iter().all(|value| *value == basic("user-a", "pw-a")),
        "{operation:?}: proxy A got a request without its own credentials: {at_a:?}"
    );
    assert!(
        at_b.iter().all(|value| *value == basic("user-b", "pw-b")),
        "{operation:?}: proxy B got a request without its own credentials: {at_b:?}"
    );
    assert_eq!(direct, 0, "{operation:?}: a request went direct to the site");
    assert_eq!(
        calls,
        at_a.len() + at_b.len(),
        "{operation:?}: the provider must be asked once per request (A got {}, B got {})",
        at_a.len(),
        at_b.len()
    );
    assert!(
        at_a.len().abs_diff(at_b.len()) <= 1,
        "{operation:?}: the rotation must alternate between A and B: A got {}, B got {}",
        at_a.len(),
        at_b.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scrape_asks_the_provider_once_per_request_and_uses_the_proxy_it_picked() {
    assert_one_pick_per_request(Operation::Scrape).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_map_asks_the_provider_once_per_request_and_uses_the_proxy_it_picked() {
    assert_one_pick_per_request(Operation::Map).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crawl_asks_the_provider_once_per_request_and_uses_the_proxy_it_picked() {
    assert_one_pick_per_request(Operation::Crawl).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cookie_set_through_one_rotated_proxy_is_sent_through_the_next() {
    let site = MockServer::start().await;
    let (a, b) = (MockServer::start().await, MockServer::start().await);
    for proxy in [&a, &b] {
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>through a proxy</body></html>")
                    .append_header("content-type", "text/html")
                    .append_header("set-cookie", "session=fix385g; Path=/"),
            )
            .mount(proxy)
            .await;
    }
    let provider = StaticProxyProvider::new(vec![
        ProxyConfig {
            url: a.uri(),
            ..ProxyConfig::default()
        },
        ProxyConfig {
            url: b.uri(),
            ..ProxyConfig::default()
        },
    ]);
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.cookies_enabled = true;
    config.browser.mode = crawlberg::BrowserMode::Never;
    let engine = crawlberg::CrawlEngine::builder()
        .config(config)
        .with_proxy_provider(Arc::new(provider))
        .build()
        .expect("a proxy provider is a valid config");

    for _ in 0..2 {
        engine
            .scrape(&site.uri())
            .await
            .expect("the scrape through the proxies must succeed");
    }

    let requests = b.received_requests().await.expect("the mock records requests");
    assert!(
        requests.iter().any(|request| request
            .headers
            .get("cookie")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("session=fix385g"))),
        "the cookie proxy A's response set must be sent through proxy B: {requests:?}"
    );
}
