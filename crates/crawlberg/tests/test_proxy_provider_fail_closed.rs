//! A `ProxyProvider` that hands back a proxy URL the client cannot use must fail the request
//! rather than silently sending it direct.

use std::sync::Arc;

use crawlberg::{CrawlConfig, CrawlEngine, ProxyConfig, ProxyProvider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROXY_PASSWORD: &str = "s3cr3t-proxy-pw";

/// A proxy URL that `Url::parse` rejects, carrying userinfo so a leak of it into a log
/// field would be detectable.
const UNPARSEABLE_PROXY_URL: &str = "://operator:s3cr3t-proxy-pw@proxy.invalid:8080";

/// Hands out a proxy URL that cannot be parsed, for every host.
#[derive(Debug)]
struct BrokenProxyProvider;

impl ProxyProvider for BrokenProxyProvider {
    fn next_proxy(&self, _host: &str) -> Option<ProxyConfig> {
        Some(ProxyConfig {
            url: UNPARSEABLE_PROXY_URL.to_owned(),
            username: Some("operator".to_owned()),
            password: Some(PROXY_PASSWORD.to_owned()),
        })
    }
}

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
async fn an_unparseable_provider_proxy_url_fails_instead_of_sending_direct() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>reached directly</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let engine = CrawlEngine::builder()
        .config(allow_private_config())
        .with_proxy_provider(Arc::new(BrokenProxyProvider))
        .build()
        .expect("engine must build");

    let seed_url = format!("{}/", mock.uri());
    let error = engine
        .crawl(&seed_url)
        .await
        .expect_err("an unusable provider proxy must fail the crawl")
        .to_string();
    assert!(error.contains("invalid proxy URL"), "unexpected error: {error}");
    assert!(
        !error.contains(PROXY_PASSWORD),
        "the error shows the proxy password: {error}"
    );
    assert!(
        mock.received_requests()
            .await
            .expect("the mock records requests")
            .is_empty(),
        "the refused proxy must not fall back to a direct request"
    );
}

/// Hands out a proxy that the credential check refuses: an unencoded `#` ends its password.
#[derive(Debug)]
struct RefusedProxyProvider;

impl ProxyProvider for RefusedProxyProvider {
    fn next_proxy(&self, _host: &str) -> Option<ProxyConfig> {
        Some(ProxyConfig {
            url: format!("http://operator:{PROXY_PASSWORD}#tail@proxy.invalid:8080"),
            username: None,
            password: None,
        })
    }
}

#[tokio::test]
async fn a_refused_provider_proxy_fails_each_request_instead_of_sending_direct() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>reached directly</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    let engine = CrawlEngine::builder()
        .config(allow_private_config())
        .with_proxy_provider(Arc::new(RefusedProxyProvider))
        .build()
        .expect("engine must build");

    let scrape_error = engine
        .scrape(&mock.uri())
        .await
        .expect_err("a refused provider proxy must fail the scrape instead of sending direct")
        .to_string();
    let map_error = engine
        .map(&mock.uri())
        .await
        .expect_err("a refused provider proxy must fail the map instead of sending direct")
        .to_string();
    for error in [scrape_error, map_error] {
        assert!(error.contains("percent-encode"), "unexpected error: {error}");
        assert!(
            !error.contains(PROXY_PASSWORD),
            "the error shows the proxy password: {error}"
        );
    }
    assert!(
        mock.received_requests()
            .await
            .expect("the mock records requests")
            .is_empty(),
        "the refused proxy must not fall back to a direct request"
    );
}
