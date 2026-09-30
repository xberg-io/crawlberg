//! The native browser renders through the proxy a `ProxyProvider` picks (#248).
//!
//! The site and the proxy are two mocks. The proxy answers every request itself with a page
//! the site never serves, so a render that went through the proxy shows the proxy's page, and
//! a render that went direct shows the site's page and leaves the proxy with no requests.

#![cfg(feature = "browser-native")]

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlEngine, ProxyConfig, ProxyProvider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROXY_MARK: &str = "SERVED-BY-THE-PROVIDER-PROXY";
const SITE_MARK: &str = "SERVED-BY-THE-SITE";
const PASSWORD: &str = "IMPL248-PROVIDER-PW";

/// A page that imports a module, with `mark` in its body.
async fn page_server(mark: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string(format!(
                    r#"<html><body><p>{mark}</p><script type="module" src="/m.js"></script></body></html>"#
                )),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/m.js"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/javascript")
                .set_body_string("globalThis.loaded = 1;"),
        )
        .mount(&server)
        .await;
    server
}

/// A provider that hands out `proxy` (or no proxy) and records every host it is asked for.
#[derive(Debug)]
struct RecordingProvider {
    proxy: Option<ProxyConfig>,
    hosts: Mutex<Vec<String>>,
}

impl RecordingProvider {
    fn new(proxy: Option<ProxyConfig>) -> Arc<Self> {
        Arc::new(Self {
            proxy,
            hosts: Mutex::new(Vec::new()),
        })
    }

    fn hosts(&self) -> Vec<String> {
        self.hosts.lock().expect("hosts lock").clone()
    }
}

impl ProxyProvider for RecordingProvider {
    fn next_proxy(&self, host: &str) -> Option<ProxyConfig> {
        self.hosts.lock().expect("hosts lock").push(host.to_owned());
        self.proxy.clone()
    }
}

fn credentialed(proxy: &MockServer) -> ProxyConfig {
    ProxyConfig {
        url: proxy.uri(),
        username: Some("operator".into()),
        password: Some(PASSWORD.into()),
    }
}

fn native_config() -> CrawlConfig {
    CrawlConfig {
        max_depth: Some(0),
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: std::time::Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

fn engine(config: CrawlConfig, provider: Arc<RecordingProvider>) -> CrawlEngine {
    CrawlEngine::builder()
        .config(config)
        .with_proxy_provider(provider)
        .build()
        .expect("a proxy provider with the native backend is a valid config")
}

/// The paths `server` received, with the `Proxy-Authorization` each carried.
async fn received(server: &MockServer) -> Vec<(String, Option<String>)> {
    let requests = server.received_requests().await.expect("the mock records requests");
    requests
        .iter()
        .map(|request| {
            (
                request.url.path().to_owned(),
                request
                    .headers
                    .get("proxy-authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            )
        })
        .collect()
}

/// The page load and the module import both reached the proxy with the provider's
/// credentials, and the site got nothing.
async fn assert_through_the_proxy(proxy: &MockServer, site: &MockServer) {
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("operator:{PASSWORD}"))
    );
    let seen = received(proxy).await;
    for wanted in ["/", "/m.js"] {
        assert!(
            seen.iter()
                .any(|(path, auth)| path == wanted && auth.as_deref() == Some(expected.as_str())),
            "{wanted} must reach the provider's proxy with its credentials; the proxy saw {seen:?}"
        );
    }
    let direct = received(site).await;
    assert!(direct.is_empty(), "the render went direct to the site: {direct:?}");
}

/// Every pick was for the page's host, the one an HTTP fetch of the page asks for.
fn assert_asked_for_the_page_host(provider: &RecordingProvider) {
    let hosts = provider.hosts();
    assert!(
        !hosts.is_empty() && hosts.iter().all(|host| host == "127.0.0.1"),
        "the provider must be asked for the page's host: {hosts:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_native_scrape_renders_through_the_provider_proxy() {
    let (site, proxy) = (page_server(SITE_MARK).await, page_server(PROXY_MARK).await);
    let provider = RecordingProvider::new(Some(credentialed(&proxy)));
    let result = engine(native_config(), Arc::clone(&provider))
        .scrape(&site.uri())
        .await
        .expect("the render through the provider's proxy must succeed");
    assert!(
        result.html.contains(PROXY_MARK),
        "the rendered page must come from the proxy: {}",
        result.html
    );
    assert_through_the_proxy(&proxy, &site).await;
    assert_asked_for_the_page_host(&provider);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_native_crawl_renders_through_the_provider_proxy() {
    let (site, proxy) = (page_server(SITE_MARK).await, page_server(PROXY_MARK).await);
    let provider = RecordingProvider::new(Some(credentialed(&proxy)));
    let result = engine(native_config(), Arc::clone(&provider))
        .crawl(&site.uri())
        .await
        .expect("the crawl through the provider's proxy must succeed");
    let page = result.pages.first().expect("the seed page is crawled");
    assert!(
        page.html.contains(PROXY_MARK),
        "the rendered page must come from the proxy: {}",
        page.html
    );
    assert_through_the_proxy(&proxy, &site).await;
    assert_asked_for_the_page_host(&provider);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_native_interact_runs_through_the_provider_proxy() {
    let (site, proxy) = (page_server(SITE_MARK).await, page_server(PROXY_MARK).await);
    let provider = RecordingProvider::new(Some(credentialed(&proxy)));
    let actions = [crawlberg::PageAction::ExecuteJs {
        script: "1".to_string(),
    }];
    let result = engine(native_config(), Arc::clone(&provider))
        .interact(&site.uri(), &actions)
        .await
        .expect("the interact through the provider's proxy must succeed");
    assert!(
        result.final_html.contains(PROXY_MARK),
        "the interact page must come from the proxy: {}",
        result.final_html
    );
    assert_through_the_proxy(&proxy, &site).await;
    assert_asked_for_the_page_host(&provider);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_native_render_goes_direct_when_the_provider_picks_no_proxy() {
    let (site, proxy) = (page_server(SITE_MARK).await, page_server(PROXY_MARK).await);
    let mut config = native_config();
    // ~keep The provider wins over the crawl-wide proxy, as on the HTTP path, so its `None`
    // ~keep sends the render direct rather than through this proxy.
    config.proxy = Some(credentialed(&proxy));
    let provider = RecordingProvider::new(None);
    let result = engine(config, Arc::clone(&provider))
        .scrape(&site.uri())
        .await
        .expect("a direct render must succeed");
    assert!(
        result.html.contains(SITE_MARK),
        "the render must go direct: {}",
        result.html
    );
    assert!(
        received(&proxy).await.is_empty(),
        "the render must not use the crawl-wide proxy"
    );
    assert!(!provider.hosts().is_empty(), "the render must ask the provider");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_browser_proxy_wins_over_the_provider() {
    let (site, proxy) = (page_server(SITE_MARK).await, page_server(PROXY_MARK).await);
    let unused = page_server("SERVED-BY-THE-UNUSED-PROXY").await;
    let mut config = native_config();
    config.browser.proxy = Some(credentialed(&proxy));
    let provider = RecordingProvider::new(Some(credentialed(&unused)));
    let result = engine(config, provider)
        .scrape(&site.uri())
        .await
        .expect("the render through browser.proxy must succeed");
    assert!(
        result.html.contains(PROXY_MARK),
        "the render must use browser.proxy: {}",
        result.html
    );
    assert_through_the_proxy(&proxy, &site).await;
    assert!(
        received(&unused).await.is_empty(),
        "the provider's proxy must not be used"
    );
}

#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test(flavor = "multi_thread")]
async fn a_chrome_render_with_a_provider_fails_instead_of_going_direct() {
    let (site, proxy) = (page_server(SITE_MARK).await, page_server(PROXY_MARK).await);
    let mut config = native_config();
    config.browser.backend = BrowserBackend::Chromiumoxide;
    let provider = RecordingProvider::new(Some(ProxyConfig {
        url: proxy.uri(),
        username: None,
        password: None,
    }));
    let result = engine(config, provider).scrape(&site.uri()).await;
    let error = result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("no error; the page was {:?}", result.as_ref().map(|r| &r.html)));
    assert!(
        error.contains("proxy provider"),
        "a Chrome render must refuse the provider: {error}"
    );
    assert!(received(&site).await.is_empty(), "the render went direct to the site");
}
