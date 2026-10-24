use std::collections::HashMap;
use std::time::Duration;

use crawlberg::{AuthConfig, BrowserMode, CrawlConfig, CrawlEngine, HostMatcher, ProxyConfig, SsrfPolicy};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const BODY: &str = "<html><body><h1>Header scope sentinel</h1><p>Complete response body.</p></body></html>";
const CUSTOM_HEADERS: [(&str, &str); 3] = [
    ("X-Access-Value", "arbitrary-secret-sentinel"),
    ("Authorization", "Bearer standard-secret-sentinel"),
    ("X-Synthetic-Public", "synthetic-public"),
];
const PUBLIC_HEADERS: [(&str, &str); 3] = [
    ("Accept", "text/html,application/scope-test"),
    ("Accept-Language", "en-GB,scope;q=0.5"),
    ("X-Synthetic-Public", "synthetic-public"),
];
const PUBLIC_ALLOWLIST: [&str; 6] = [
    "aCcEpT",
    "ACCEPT-LANGUAGE",
    "x-synthetic-public",
    "Authorization",
    "X-Operator-Capability",
    "X-Access",
];

fn engine(mock: &MockServer, allow_subdomains: bool, custom_user_agent: bool, allowlist: &[&str]) -> CrawlEngine {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.allow_subdomains = allow_subdomains;
    config.subdomain_header_allowlist = allowlist.iter().map(|name| (*name).to_owned()).collect();
    config.max_depth = Some(1);
    config.respect_robots_txt = false;
    config.browser.mode = BrowserMode::Never;
    config.ssrf = SsrfPolicy {
        deny_private: false,
        allowlist: vec![HostMatcher::suffix("localhost")],
        ..SsrfPolicy::default()
    };
    config.proxy = Some(ProxyConfig {
        url: mock.uri(),
        username: None,
        password: None,
    });
    config.auth = Some(AuthConfig::Header {
        name: "X-Operator-Capability".to_owned(),
        value: "configured-secret-sentinel".to_owned(),
    });
    config.custom_headers = CUSTOM_HEADERS
        .into_iter()
        .chain(PUBLIC_HEADERS)
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<HashMap<_, _>>();
    if custom_user_agent {
        config
            .custom_headers
            .insert("User-Agent".to_owned(), "custom-agent-secret-sentinel".to_owned());
    }
    CrawlEngine::builder().config(config).build().expect("engine builds")
}

async fn mount_response(mock: &MockServer, at: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .expect(1)
        .mount(mock)
        .await;
}

async fn received_pair(mock: &MockServer) -> Vec<Request> {
    let requests = mock.received_requests().await.expect("receiver records requests");
    assert_eq!(requests.len(), 2, "expected seed and target exactly once: {requests:?}");
    for at in ["/seed", "/target"] {
        assert_eq!(requests.iter().filter(|request| request.url.path() == at).count(), 1);
    }
    requests
}

async fn redirect_requests(
    target_host: &str,
    allow_subdomains: bool,
    custom_user_agent: bool,
    allowlist: &[&str],
) -> Vec<Request> {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    let target = format!("http://{target_host}:{port}/target");
    mount_response(
        &mock,
        "/seed",
        ResponseTemplate::new(302).insert_header("location", target.as_str()),
    )
    .await;
    mount_response(
        &mock,
        "/target",
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/html")
            .set_body_string(BODY),
    )
    .await;
    let engine = engine(&mock, allow_subdomains, custom_user_agent, allowlist);
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        engine.scrape(&format!("http://localhost:{port}/seed")),
    )
    .await
    .expect("scrape finishes before timeout")
    .expect("redirect scrape succeeds");
    assert_eq!(result.status_code, 200);
    assert_eq!(result.final_url, target);
    assert_eq!(result.html, BODY);
    received_pair(&mock).await
}

async fn crawl_requests(allowlist: &[&str]) -> Vec<Request> {
    let mock = MockServer::start().await;
    let port = mock.address().port();
    let target = format!("http://sub.localhost:{port}/target");
    mount_response(
        &mock,
        "/seed",
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/html")
            .set_body_string(format!("<html><body><a href=\"{target}\">subdomain</a></body></html>")),
    )
    .await;
    mount_response(
        &mock,
        "/target",
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/html")
            .set_body_string(BODY),
    )
    .await;
    let engine = engine(&mock, true, false, allowlist);
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        engine.crawl(&format!("http://localhost:{port}/seed")),
    )
    .await
    .expect("crawl finishes before timeout")
    .expect("crawl succeeds");
    assert_eq!(result.pages.len(), 2);
    let page = result
        .pages
        .iter()
        .find(|page| page.url == target)
        .expect("subdomain page was crawled");
    assert_eq!(page.status_code, 200);
    assert_eq!(page.html, BODY);
    received_pair(&mock).await
}

fn header(requests: &[Request], at: &str, name: &str) -> Option<String> {
    let values: Vec<String> = requests
        .iter()
        .find(|request| request.url.path() == at)
        .unwrap_or_else(|| panic!("{at} was not received"))
        .headers
        .get_all(name)
        .iter()
        .map(|value| value.to_str().expect("synthetic ASCII header").to_owned())
        .collect();
    assert!(values.len() <= 1, "{at} received repeated header {name}: {values:?}");
    values.into_iter().next()
}

fn assert_seed_received_credentials(requests: &[Request]) {
    for (name, value) in CUSTOM_HEADERS {
        assert_eq!(
            header(requests, "/seed", name).as_deref(),
            Some(value),
            "seed header {name}"
        );
    }
    assert_eq!(
        header(requests, "/seed", "X-Operator-Capability").as_deref(),
        Some("configured-secret-sentinel")
    );
}

fn assert_target_withheld_custom_headers(requests: &[Request]) {
    for name in [
        "Authorization",
        "X-Operator-Capability",
        "X-Access-Value",
        "X-Synthetic-Public",
    ] {
        assert_eq!(
            header(requests, "/target", name),
            None,
            "custom header {name} crossed the seed host boundary"
        );
    }
    for (name, value) in PUBLIC_HEADERS {
        assert_ne!(
            header(requests, "/target", name).as_deref(),
            Some(value),
            "unlisted public header {name}"
        );
    }
}

fn assert_target_received_only_allowed_public_headers(requests: &[Request]) {
    for (name, value) in PUBLIC_HEADERS {
        assert_eq!(
            header(requests, "/target", name).as_deref(),
            Some(value),
            "allowed header {name}"
        );
        assert_eq!(
            header(requests, "/seed", name).as_deref(),
            Some(value),
            "seed header {name}"
        );
    }
    for name in ["Authorization", "X-Operator-Capability", "X-Access-Value"] {
        assert_eq!(header(requests, "/target", name), None, "unauthorized header {name}");
    }
}

#[tokio::test]
async fn should_withhold_arbitrary_custom_headers_from_an_admitted_subdomain_by_default() {
    let requests = redirect_requests("sub.localhost", true, false, &[]).await;
    assert_seed_received_credentials(&requests);
    assert_target_withheld_custom_headers(&requests);
}

#[tokio::test]
async fn should_withhold_custom_headers_when_subdomains_are_not_admitted() {
    let requests = redirect_requests("sub.localhost", false, false, &[]).await;
    assert_seed_received_credentials(&requests);
    assert_target_withheld_custom_headers(&requests);
}

#[tokio::test]
async fn should_withhold_a_custom_user_agent_from_an_admitted_subdomain_by_default() {
    let requests = redirect_requests("sub.localhost", true, true, &[]).await;
    assert_eq!(
        header(&requests, "/seed", "User-Agent").as_deref(),
        Some("custom-agent-secret-sentinel")
    );
    assert_ne!(
        header(&requests, "/target", "User-Agent").as_deref(),
        Some("custom-agent-secret-sentinel")
    );
}

#[tokio::test]
async fn should_retain_custom_headers_on_a_same_host_redirect() {
    let requests = redirect_requests("localhost", true, true, &[]).await;
    assert_seed_received_credentials(&requests);
    for (name, value) in CUSTOM_HEADERS.into_iter().chain([
        ("X-Operator-Capability", "configured-secret-sentinel"),
        ("User-Agent", "custom-agent-secret-sentinel"),
    ]) {
        assert_eq!(
            header(&requests, "/target", name).as_deref(),
            Some(value),
            "same-host header {name}"
        );
    }
}

#[tokio::test]
async fn should_withhold_custom_headers_from_an_unrelated_redirect_host() {
    let requests = redirect_requests("127.0.0.1", true, false, &[]).await;
    assert_seed_received_credentials(&requests);
    assert_target_withheld_custom_headers(&requests);
}

#[tokio::test]
async fn should_forward_only_explicitly_allowlisted_public_headers_to_a_subdomain() {
    let requests = redirect_requests("sub.localhost", true, false, &PUBLIC_ALLOWLIST).await;
    assert_seed_received_credentials(&requests);
    assert_target_received_only_allowed_public_headers(&requests);
}

#[tokio::test]
async fn should_ignore_subdomain_header_allowlist_when_subdomains_are_disabled() {
    let requests = redirect_requests("sub.localhost", false, false, &PUBLIC_ALLOWLIST).await;
    assert_seed_received_credentials(&requests);
    assert_target_withheld_custom_headers(&requests);
}

#[tokio::test]
async fn should_ignore_subdomain_header_allowlist_for_unrelated_hosts() {
    let requests = redirect_requests("127.0.0.1", true, false, &PUBLIC_ALLOWLIST).await;
    assert_seed_received_credentials(&requests);
    assert_target_withheld_custom_headers(&requests);
}

#[tokio::test]
async fn should_withhold_custom_user_agent_from_an_unrelated_host() {
    let requests = redirect_requests("127.0.0.1", true, true, &["User-Agent"]).await;
    assert_eq!(
        header(&requests, "/seed", "User-Agent").as_deref(),
        Some("custom-agent-secret-sentinel")
    );
    assert_ne!(
        header(&requests, "/target", "User-Agent").as_deref(),
        Some("custom-agent-secret-sentinel")
    );
}

#[tokio::test]
async fn should_forward_custom_user_agent_only_after_explicit_subdomain_opt_in() {
    let requests = redirect_requests("sub.localhost", true, true, &["uSeR-aGeNt"]).await;
    assert_eq!(
        header(&requests, "/seed", "User-Agent").as_deref(),
        Some("custom-agent-secret-sentinel")
    );
    assert_eq!(
        header(&requests, "/target", "User-Agent").as_deref(),
        Some("custom-agent-secret-sentinel")
    );
    assert_target_withheld_custom_headers(&requests);
}

#[tokio::test]
async fn should_withhold_custom_headers_on_crawled_subdomain_links_by_default() {
    let requests = crawl_requests(&[]).await;
    assert_seed_received_credentials(&requests);
    assert_target_withheld_custom_headers(&requests);
}

#[tokio::test]
async fn should_forward_only_allowlisted_public_headers_on_crawled_subdomain_links() {
    let requests = crawl_requests(&PUBLIC_ALLOWLIST).await;
    assert_seed_received_credentials(&requests);
    assert_target_received_only_allowed_public_headers(&requests);
}
