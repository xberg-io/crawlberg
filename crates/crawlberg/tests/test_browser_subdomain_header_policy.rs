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

async fn public_header_redirect(backend: BrowserBackend) {
    let server = MockServer::start().await;
    mount(
        &server,
        "/seed",
        ResponseTemplate::new(302).insert_header("location", url(&server, "sub.localhost", "/target")),
    )
    .await;
    mount(&server, "/target", page("")).await;
    let mut config = config(&server, backend, BrowserMode::Always);
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
async fn interaction_fetch_redirect(backend: BrowserBackend, allow_public: bool) {
    let server = MockServer::start().await;
    let native = backend == BrowserBackend::Native;
    let contents = if native {
        "<script>fetch('/fetch-hop').then(response => response.text()).then(() => globalThis.headerDone = 1);</script>"
    } else {
        ""
    };
    mount(&server, "/seed", page(contents)).await;
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
    let mut settings = config(&server, backend, BrowserMode::Always);
    if allow_public {
        allow_public_headers(&mut settings);
    }
    let engine = crawlberg::create_engine(Some(settings)).expect("interaction config builds");
    let actions = vec![
        crawlberg::PageAction::ExecuteJs {
            script: if native {
                "globalThis.headerDone".to_owned()
            } else {
                "fetch('/fetch-hop', {mode:'no-cors'}); 1".to_owned()
            },
        },
        crawlberg::PageAction::Wait {
            milliseconds: Some(500),
            selector: None,
        },
    ];
    let result = tokio::time::timeout(
        Duration::from_secs(40),
        crawlberg::interact(&engine, &url(&server, "localhost", "/seed"), actions),
    )
    .await
    .expect("interaction finishes before deadline")
    .expect("interaction succeeds");
    assert_eq!(result.action_results.len(), 2);
    assert_eq!(
        result
            .action_results
            .iter()
            .map(|action| action.success)
            .collect::<Vec<_>>(),
        vec![true, true],
        "all interaction actions must execute successfully"
    );
    assert!(
        result.final_html.contains(MARKER),
        "complete interaction page must be rendered"
    );
    if native {
        assert_eq!(
            result.action_results[0].data,
            Some(serde_json::json!(1.0)),
            "native page fetch must complete"
        );
    }
    let requests = requests(&server, &["/seed", "/fetch-hop", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_seed(at(&requests, "/fetch-hop"));
    assert_eq!(
        header(at(&requests, "/target"), "host"),
        Some(format!("sub.localhost:{}", server.address().port()).as_str())
    );
    if allow_public {
        assert_allowed_public(at(&requests, "/target"));
    } else {
        assert_withheld(at(&requests, "/target"), "sub.localhost");
    }
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
async fn native_script_redirect(module: bool, mode: BrowserMode, allow_public: bool) {
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
    let mut settings = config(&server, BrowserBackend::Native, mode);
    if allow_public {
        allow_public_headers(&mut settings);
    }
    scrape(&server, settings).await;
    let requests = requests(&server, &["/seed", "/script-hop", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_seed(at(&requests, "/script-hop"));
    assert_eq!(
        header(at(&requests, "/target"), "host"),
        Some(format!("sub.localhost:{}", server.address().port()).as_str())
    );
    if allow_public {
        assert_allowed_public(at(&requests, "/target"));
    } else {
        assert_withheld(at(&requests, "/target"), "sub.localhost");
    }
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_native_fetch_redirect_to_a_subdomain() {
    native_script_redirect(false, BrowserMode::Always, false).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_native_module_redirect_to_a_subdomain() {
    native_script_redirect(true, BrowserMode::Always, false).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_stealth_native_fetch_redirect_to_a_subdomain() {
    native_script_redirect(false, BrowserMode::Stealth, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_explicit_public_headers_after_a_chrome_subdomain_redirect() {
    public_header_redirect(BrowserBackend::Chromiumoxide).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_explicit_public_headers_after_a_native_subdomain_redirect() {
    public_header_redirect(BrowserBackend::Native).await;
}

fn allow_public_headers(config: &mut CrawlConfig) {
    config.subdomain_header_allowlist = OPT_IN_TEST_HEADERS
        .into_iter()
        .chain(["x-synthetic-public"])
        .map(str::to_owned)
        .collect();
    config.custom_headers.extend([
        ("Accept".to_owned(), "application/scope-test".to_owned()),
        ("Accept-Language".to_owned(), "en-GB,scope;q=0.5".to_owned()),
    ]);
}

fn assert_allowed_public(request: &Request) {
    for (name, value) in [
        ("Accept", "application/scope-test"),
        ("Accept-Language", "en-GB,scope;q=0.5"),
        ("X-Synthetic-Public", "browser-public-sentinel"),
        ("User-Agent", CUSTOM_UA),
    ] {
        assert_eq!(
            header_values(request, name),
            vec![value],
            "allowed public header {name}"
        );
    }
    for name in ["X-Access-Value", "Authorization", "X-Operator-Capability"] {
        assert_eq!(header(request, name), None, "subdomain credential {name}");
    }
}

async fn linked_subdomain_crawl(backend: BrowserBackend, allow_public: bool) {
    let server = MockServer::start().await;
    let target = url(&server, "sub.localhost", "/target");
    mount(&server, "/seed", page(&format!("<a href='{target}'>subdomain</a>"))).await;
    mount(&server, "/target", page("")).await;
    let mut settings = config(&server, backend, BrowserMode::Always);
    settings.max_depth = Some(1);
    if allow_public {
        allow_public_headers(&mut settings);
    }
    let engine = CrawlEngine::builder()
        .config(settings)
        .build()
        .expect("crawl config builds");
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        engine.crawl(&url(&server, "localhost", "/seed")),
    )
    .await
    .expect("linked crawl finishes")
    .expect("linked crawl succeeds");
    assert_eq!(result.pages.len(), 2, "seed and linked subdomain must both be crawled");
    for expected_url in [url(&server, "localhost", "/seed"), target] {
        let page = result
            .pages
            .iter()
            .find(|page| page.url == expected_url)
            .expect("expected page exists");
        assert_eq!(page.status_code, 200);
        assert!(page.html.contains(MARKER), "complete linked page must be rendered");
    }
    let requests = requests(&server, &["/seed", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_eq!(
        header(at(&requests, "/target"), "host"),
        Some(format!("sub.localhost:{}", server.address().port()).as_str())
    );
    if allow_public {
        assert_allowed_public(at(&requests, "/target"));
    } else {
        assert_withheld(at(&requests, "/target"), "sub.localhost");
    }
}

async fn document_redirect_chain(backend: BrowserBackend, allow_public: bool) {
    let server = MockServer::start().await;
    for (from, host, to) in [
        ("/seed", "sub.localhost", "/subdomain"),
        ("/subdomain", "127.0.0.1", "/external"),
        ("/external", "localhost", "/returned"),
    ] {
        mount(
            &server,
            from,
            ResponseTemplate::new(302).insert_header("location", url(&server, host, to)),
        )
        .await;
    }
    mount(&server, "/returned", page("")).await;
    let mut settings = config(&server, backend, BrowserMode::Always);
    if allow_public {
        allow_public_headers(&mut settings);
    }
    scrape(&server, settings).await;
    let requests = requests(&server, &["/seed", "/subdomain", "/external", "/returned"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_seed(at(&requests, "/returned"));
    for (route, host) in [
        ("/seed", "localhost"),
        ("/subdomain", "sub.localhost"),
        ("/external", "127.0.0.1"),
        ("/returned", "localhost"),
    ] {
        assert_eq!(
            header(at(&requests, route), "host"),
            Some(format!("{host}:{}", server.address().port()).as_str())
        );
    }
    assert_withheld(at(&requests, "/external"), "127.0.0.1");
    if allow_public {
        assert_allowed_public(at(&requests, "/subdomain"));
        for (name, value) in [
            ("Accept", "application/scope-test"),
            ("Accept-Language", "en-GB,scope;q=0.5"),
        ] {
            assert_ne!(
                header(at(&requests, "/external"), name),
                Some(value),
                "external public {name}"
            );
        }
    } else {
        assert_withheld(at(&requests, "/subdomain"), "sub.localhost");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_on_chrome_crawled_subdomain_links_by_default() {
    linked_subdomain_crawl(BrowserBackend::Chromiumoxide, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_allowed_public_headers_on_chrome_crawled_subdomain_links() {
    linked_subdomain_crawl(BrowserBackend::Chromiumoxide, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn should_apply_seed_scope_at_every_chrome_redirect_chain_hop() {
    for allow_public in [false, true] {
        document_redirect_chain(BrowserBackend::Chromiumoxide, allow_public).await;
    }
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_on_native_crawled_subdomain_links_by_default() {
    linked_subdomain_crawl(BrowserBackend::Native, false).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_allowed_public_headers_on_native_crawled_subdomain_links() {
    linked_subdomain_crawl(BrowserBackend::Native, true).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_apply_seed_scope_at_every_native_redirect_chain_hop() {
    for allow_public in [false, true] {
        document_redirect_chain(BrowserBackend::Native, allow_public).await;
    }
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_a_native_unrelated_host_redirect() {
    document_redirect(BrowserBackend::Native, "127.0.0.1", true).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_allowed_public_headers_after_a_native_fetch_redirect() {
    native_script_redirect(false, BrowserMode::Always, true).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_allowed_public_headers_after_a_native_module_redirect() {
    native_script_redirect(true, BrowserMode::Always, true).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_allowed_public_headers_after_a_stealth_native_fetch_redirect() {
    native_script_redirect(false, BrowserMode::Stealth, true).await;
}

#[cfg(feature = "interact")]
#[tokio::test(flavor = "multi_thread")]
async fn should_withhold_seed_headers_after_an_interaction_fetch_redirect_to_a_subdomain() {
    interaction_fetch_redirect(BrowserBackend::Chromiumoxide, false).await;
}

#[cfg(all(feature = "interact", feature = "browser-native"))]
#[tokio::test(flavor = "multi_thread")]
async fn should_forward_only_allowed_public_headers_for_a_native_interaction_page_fetch() {
    interaction_fetch_redirect(BrowserBackend::Native, true).await;
}

#[cfg(feature = "browser-native")]
#[tokio::test(flavor = "multi_thread")]
async fn should_ignore_native_public_header_allowlist_when_subdomains_are_disabled() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/seed",
        ResponseTemplate::new(302).insert_header("location", url(&server, "sub.localhost", "/target")),
    )
    .await;
    mount(&server, "/target", page("")).await;
    let mut settings = config(&server, BrowserBackend::Native, BrowserMode::Always);
    settings.allow_subdomains = false;
    allow_public_headers(&mut settings);
    scrape(&server, settings).await;
    let requests = requests(&server, &["/seed", "/target"]).await;
    assert_seed(at(&requests, "/seed"));
    assert_withheld(at(&requests, "/target"), "sub.localhost");
    for (name, value) in [
        ("Accept", "application/scope-test"),
        ("Accept-Language", "en-GB,scope;q=0.5"),
    ] {
        assert_ne!(
            header(at(&requests, "/target"), name),
            Some(value),
            "disabled public {name}"
        );
    }
}
