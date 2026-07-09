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
