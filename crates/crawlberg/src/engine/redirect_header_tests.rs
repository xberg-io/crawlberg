use super::*;
use crate::types::{AuthConfig, BrowserMode, CrawlConfig, ProxyConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SECRET: &str = "redirected-robots-synthetic-secret";

fn fixture_config(site: &MockServer) -> CrawlConfig {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.respect_robots_txt = true;
    config.browser.mode = BrowserMode::Never;
    config.rate_limit_ms = Some(0);
    config.proxy = Some(ProxyConfig {
        url: site.uri(),
        username: None,
        password: None,
    });
    config
}

async fn redirected_site() -> MockServer {
    let site = MockServer::start().await;
    let seed_robots = format!("http://localhost:{}/seed-robots.txt", site.address().port());
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", seed_robots))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/seed-robots.txt"))
        .respond_with(|request: &Request| {
            let private = request.headers.get("x-access-value").and_then(|v| v.to_str().ok()) == Some(SECRET);
            let body = if private {
                "User-agent: *\nDisallow: /private-only\n"
            } else {
                "User-agent: *\nDisallow:\n"
            };
            ResponseTemplate::new(200).set_body_string(body)
        })
        .mount(&site)
        .await;
    site
}

async fn assert_redirected_robots_cache(custom: bool) {
    let site = redirected_site().await;
    let port = site.address().port();
    let seed = Url::parse(&format!("http://localhost:{port}/seed")).expect("seed URL");
    let initial = Url::parse(&format!("http://sub.localhost:{port}/page")).expect("initial URL");
    let mut config = fixture_config(&site);
    if custom {
        config
            .custom_headers
            .insert("X-Access-Value".to_owned(), SECRET.to_owned());
    }
    let mut engine = CrawlEngine::builder().config(config).build().expect("engine");
    engine.config.credential_scope = crate::net::CredentialScope::for_seed(&seed, None);
    let client = crate::http::build_client(&engine.config).expect("HTTP client");
    for _ in 0..2 {
        let outcome = resolve_robots_outcome(&engine, &client, &initial, initial.as_str(), "PublicAgent").await;
        assert_eq!(outcome.allows("/private-only"), !custom);
    }
    let requests = site.received_requests().await.expect("received requests");
    for request in &requests {
        let expected = (custom && request.url.path() == "/seed-robots.txt").then_some(SECRET);
        assert_eq!(
            request.headers.get("x-access-value").and_then(|v| v.to_str().ok()),
            expected
        );
    }
    let expected = if custom { 2 } else { 1 };
    for route in ["/robots.txt", "/seed-robots.txt"] {
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == route).count(),
            expected,
            "route {route}"
        );
    }
    assert_eq!(requests.len(), expected * 2);
}

#[tokio::test]
async fn redirected_custom_headers_bypass_shared_robots_cache() {
    assert_redirected_robots_cache(true).await;
}

#[tokio::test]
async fn anonymous_redirected_robots_rules_reuse_the_shared_cache() {
    assert_redirected_robots_cache(false).await;
}

#[tokio::test]
async fn typed_agent_credentials_override_rotation_for_robots_and_the_wire() {
    let site = MockServer::start().await;
    Mock::given(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("User-agent: Rotating\nDisallow: /allowed\n\nUser-agent: CredentialAgent\nAllow: /\n"),
        )
        .mount(&site)
        .await;
    Mock::given(path("/allowed"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>allowed-page</body></html>", "text/html"))
        .mount(&site)
        .await;
    let mut config = fixture_config(&site);
    config.max_depth = Some(0);
    config.user_agents = vec!["Rotating".to_owned()];
    config.auth = Some(AuthConfig::Header {
        name: "User-Agent".to_owned(),
        value: "CredentialAgent".to_owned(),
    });
    let engine = CrawlEngine::builder().config(config).build().expect("engine");
    let result = engine
        .crawl(&format!("{}/allowed", site.uri()))
        .await
        .expect("crawl completes");
    let requests = site.received_requests().await.expect("received requests");
    let robots: Vec<_> = requests.iter().filter(|r| r.url.path() == "/robots.txt").collect();
    assert_eq!(robots.len(), 1);
    assert_eq!(
        robots[0].headers.get("user-agent").and_then(|v| v.to_str().ok()),
        Some("CredentialAgent")
    );
    assert_eq!(
        result.pages.len(),
        1,
        "robots must judge the credential agent actually sent"
    );
    let pages: Vec<_> = requests.iter().filter(|r| r.url.path() == "/allowed").collect();
    assert_eq!(pages.len(), 1);
    assert_eq!(
        pages[0].headers.get("user-agent").and_then(|v| v.to_str().ok()),
        Some("CredentialAgent")
    );
    assert_eq!(requests.len(), 2);
}
