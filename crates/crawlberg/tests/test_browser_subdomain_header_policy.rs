#![cfg(feature = "browser")]

use std::collections::HashMap;
use std::time::Duration;

use crawlberg::{AuthConfig, BrowserBackend, BrowserMode, CrawlConfig, CrawlEngine, HostMatcher, SsrfPolicy};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SECRET: &str = "browser-arbitrary-secret-sentinel";
const CUSTOM_UA: &str = "browser-custom-agent-secret-sentinel";
const AUTH: &str = "browser-configured-secret-sentinel";
const MARKER: &str = "browser-header-scope-complete";
const OPT_IN_TEST_HEADERS: [&str; 5] = [
    "aCcEpT",
    "ACCEPT-LANGUAGE",
    "uSeR-aGeNt",
    "Authorization",
    "X-Operator-Capability",
];

fn config(server: &MockServer, backend: BrowserBackend, mode: BrowserMode) -> CrawlConfig {
    assert!(
        server.address().ip().is_loopback(),
        "fixture receiver must remain local"
    );
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.allow_subdomains = true;
    config.respect_robots_txt = false;
    config.user_agent = Some("browser-public-crawl-identity".to_owned());
    config.ssrf = SsrfPolicy {
        deny_private: false,
        allowlist: vec![HostMatcher::suffix("localhost")],
        ..SsrfPolicy::default()
    };
    config.browser.backend = backend;
    config.browser.mode = mode;
    config.browser.timeout = Duration::from_secs(25);
    config.browser.extra_wait = Some(Duration::from_millis(300));
    if config.browser.backend == BrowserBackend::Chromiumoxide {
        let chrome = std::env::var_os("CRAWLBERG_TEST_CHROME_PATH").expect("real Chrome path is required");
        assert!(std::path::Path::new(&chrome).is_file(), "Chrome must exist");
        config.browser.chrome_path = Some(chrome.into());
        config.browser.chrome_args = vec!["--host-resolver-rules=MAP *.localhost 127.0.0.1".to_owned()];
    }
    #[cfg(feature = "browser-native")]
    if config.browser.backend == BrowserBackend::Native {
        config.proxy = Some(crawlberg::ProxyConfig {
            url: server.uri(),
            username: None,
            password: None,
        });
    }
    config.custom_headers = HashMap::from([
        ("X-Access-Value".to_owned(), SECRET.to_owned()),
        ("X-Synthetic-Public".to_owned(), "browser-public-sentinel".to_owned()),
        ("Authorization".to_owned(), "Bearer browser-standard-secret".to_owned()),
        ("User-Agent".to_owned(), CUSTOM_UA.to_owned()),
    ]);
    config.auth = Some(AuthConfig::Header {
        name: "X-Operator-Capability".to_owned(),
        value: AUTH.to_owned(),
    });
    config
}

async fn mount(server: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

fn page(contents: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(
        format!("<html><body><p>{MARKER}</p>{contents}</body></html>"),
        "text/html",
    )
}

fn url(server: &MockServer, host: &str, route: &str) -> String {
    format!("http://{host}:{}{route}", server.address().port())
}

async fn scrape(server: &MockServer, config: CrawlConfig) {
    let engine = CrawlEngine::builder()
        .config(config)
        .build()
        .expect("browser config builds");
    let result = tokio::time::timeout(
        Duration::from_secs(40),
        engine.scrape(&url(server, "localhost", "/seed")),
    )
    .await
    .expect("browser finishes before deadline")
    .expect("browser scrape succeeds");
    assert_eq!(result.status_code, 200);
    assert!(
        result.html.contains(MARKER),
        "complete fixture response must be rendered"
    );
}

async fn requests(server: &MockServer, routes: &[&str]) -> Vec<Request> {
    let requests = server.received_requests().await.expect("receiver records requests");
    let observed = requests
        .iter()
        .filter(|request| routes.contains(&request.url.path()))
        .count();
    assert_eq!(observed, routes.len(), "expected and observed fixture endpoint counts");
    for route in routes {
        assert_eq!(
            requests.iter().filter(|request| request.url.path() == *route).count(),
            1,
            "fixture route {route} must be received exactly once"
        );
    }
    requests
}

fn at<'a>(requests: &'a [Request], route: &str) -> &'a Request {
    requests
        .iter()
        .find(|request| request.url.path() == route)
        .expect("counted fixture request exists")
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .get(name)
        .map(|value| value.to_str().expect("synthetic ASCII header"))
}

fn header_values<'a>(request: &'a Request, name: &str) -> Vec<&'a str> {
    request
        .headers
        .get_all(name)
        .iter()
        .map(|value| value.to_str().expect("synthetic ASCII header"))
        .collect()
}

fn assert_seed(request: &Request) {
    for (name, value) in [
        ("X-Access-Value", SECRET),
        ("X-Synthetic-Public", "browser-public-sentinel"),
        ("Authorization", "Bearer browser-standard-secret"),
        ("X-Operator-Capability", AUTH),
        ("User-Agent", CUSTOM_UA),
    ] {
        assert_eq!(
            header_values(request, name),
            vec![value],
            "seed header {name} for {}",
            request.url
        );
    }
}

fn assert_withheld(request: &Request, target_host: &str) {
    assert_eq!(
        header(request, "host").and_then(|host| host.split(':').next()),
        Some(target_host),
        "target must actually be the requested other host"
    );
    for name in [
        "X-Access-Value",
        "X-Synthetic-Public",
        "Authorization",
        "X-Operator-Capability",
    ] {
        assert_eq!(header(request, name), None, "target received seed header {name}");
    }
    assert!(
        header_values(request, "User-Agent")
            .iter()
            .all(|value| !value.contains(CUSTOM_UA)),
        "target received a secret in one of its User-Agent headers"
    );
}

async fn document_redirect(backend: BrowserBackend, target_host: &str, public_identity: bool) {
    let server = MockServer::start().await;
    mount(
        &server,
        "/seed",
        ResponseTemplate::new(302).insert_header("location", url(&server, target_host, "/target")),
    )
    .await;
    mount(&server, "/target", page("")).await;
    let mut config = config(&server, backend, BrowserMode::Always);
    if !public_identity {
        config.user_agent = None;
    }
    scrape(&server, config).await;
    let requests = requests(&server, &["/seed", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_eq!(
        header(at(&requests, "/target"), "host"),
        Some(format!("{target_host}:{}", server.address().port()).as_str())
    );
    if target_host == "localhost" {
        assert_seed(at(&requests, "/target"));
    } else {
        assert_withheld(at(&requests, "/target"), target_host);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_chrome_document_redirect_to_a_subdomain() {
    document_redirect(BrowserBackend::Chromiumoxide, "sub.localhost", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_a_custom_agent_after_a_chrome_subdomain_redirect_without_a_public_agent() {
    document_redirect(BrowserBackend::Chromiumoxide, "sub.localhost", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn should_retain_seed_headers_after_a_chrome_same_host_redirect() {
    document_redirect(BrowserBackend::Chromiumoxide, "localhost", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_chrome_unrelated_host_redirect() {
    document_redirect(BrowserBackend::Chromiumoxide, "127.0.0.1", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_explicit_public_headers_after_a_chrome_subdomain_redirect() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/seed",
        ResponseTemplate::new(302).insert_header("location", url(&server, "sub.localhost", "/target")),
    )
    .await;
    mount(&server, "/target", page("")).await;
    let mut config = config(&server, BrowserBackend::Chromiumoxide, BrowserMode::Always);
    config.subdomain_header_allowlist = OPT_IN_TEST_HEADERS.map(str::to_owned).to_vec();
    config.custom_headers.extend([
        ("Accept".to_owned(), "application/json".to_owned()),
        ("Accept-Language".to_owned(), "en-GB".to_owned()),
    ]);
    scrape(&server, config).await;
    let requests = requests(&server, &["/seed", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    let target = at(&requests, "/target");
    assert_eq!(
        header(target, "host"),
        Some(format!("sub.localhost:{}", server.address().port()).as_str())
    );
    for (name, value) in [
        ("Accept", "application/json"),
        ("Accept-Language", "en-GB"),
        ("User-Agent", CUSTOM_UA),
    ] {
        assert_eq!(
            header_values(target, name),
            vec![value],
            "explicit public header {name}"
        );
    }
    for name in [
        "X-Access-Value",
        "X-Synthetic-Public",
        "Authorization",
        "X-Operator-Capability",
    ] {
        assert_eq!(header(target, name), None, "credential or unlisted header {name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_chrome_subresource_redirect_to_a_subdomain() {
    let server = MockServer::start().await;
    mount(&server, "/seed", page("<img src='/image-hop'>")).await;
    mount(
        &server,
        "/image-hop",
        ResponseTemplate::new(302).insert_header("location", url(&server, "sub.localhost", "/target")),
    )
    .await;
    mount(
        &server,
        "/target",
        ResponseTemplate::new(200).set_body_raw(b"image".to_vec(), "image/png"),
    )
    .await;
    scrape(
        &server,
        config(&server, BrowserBackend::Chromiumoxide, BrowserMode::Always),
    )
    .await;
    let requests = requests(&server, &["/seed", "/image-hop", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_seed(at(&requests, "/image-hop"));
    assert_withheld(at(&requests, "/target"), "sub.localhost");
}

#[cfg(feature = "interact")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_an_interaction_fetch_redirect_to_a_subdomain() {
    let server = MockServer::start().await;
    mount(&server, "/seed", page("")).await;
    mount(
        &server,
        "/fetch-hop",
        ResponseTemplate::new(302).insert_header("location", url(&server, "sub.localhost", "/target")),
    )
    .await;
    mount(
        &server,
        "/target",
        ResponseTemplate::new(200).set_body_string("fetch-complete"),
    )
    .await;
    let engine = crawlberg::create_engine(Some(config(
        &server,
        BrowserBackend::Chromiumoxide,
        BrowserMode::Always,
    )))
    .expect("interaction config builds");
    let actions = vec![
        crawlberg::PageAction::ExecuteJs {
            script: "fetch('/fetch-hop', {mode:'no-cors'}); 1".to_owned(),
        },
        crawlberg::PageAction::Wait {
            milliseconds: Some(500),
            selector: None,
        },
    ];
    tokio::time::timeout(
        Duration::from_secs(40),
        crawlberg::interact(&engine, &url(&server, "localhost", "/seed"), actions),
    )
    .await
    .expect("interaction finishes before deadline")
    .expect("interaction succeeds");
    let requests = requests(&server, &["/seed", "/fetch-hop", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_seed(at(&requests, "/fetch-hop"));
    assert_withheld(at(&requests, "/target"), "sub.localhost");
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_native_document_redirect_to_a_subdomain() {
    document_redirect(BrowserBackend::Native, "sub.localhost", true).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_a_custom_agent_after_a_native_subdomain_redirect_without_a_public_agent() {
    document_redirect(BrowserBackend::Native, "sub.localhost", false).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_retain_seed_headers_after_a_native_same_host_redirect() {
    document_redirect(BrowserBackend::Native, "localhost", true).await;
}

#[cfg(feature = "browser-native")]
async fn native_script_redirect(module: bool, mode: BrowserMode) {
    let server = MockServer::start().await;
    let script = if module {
        "<script type='module' src='/script-hop'></script>"
    } else {
        "<script>fetch('/script-hop').then(response => response.text()).then(() => window.headerDone = 1);</script>"
    };
    mount(&server, "/seed", page(script)).await;
    mount(
        &server,
        "/script-hop",
        ResponseTemplate::new(302).insert_header("location", url(&server, "sub.localhost", "/target")),
    )
    .await;
    mount(
        &server,
        "/target",
        ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            .set_body_raw("globalThis.headerDone = 1;", "application/javascript"),
    )
    .await;
    scrape(&server, config(&server, BrowserBackend::Native, mode)).await;
    let requests = requests(&server, &["/seed", "/script-hop", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_seed(at(&requests, "/script-hop"));
    assert_withheld(at(&requests, "/target"), "sub.localhost");
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_native_fetch_redirect_to_a_subdomain() {
    native_script_redirect(false, BrowserMode::Always).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_native_module_redirect_to_a_subdomain() {
    native_script_redirect(true, BrowserMode::Always).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_stealth_native_fetch_redirect_to_a_subdomain() {
    native_script_redirect(false, BrowserMode::Stealth).await;
}
