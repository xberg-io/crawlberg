#![cfg(feature = "browser-native")]

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, DispatchProfile, EscalationStrategy, crawl, create_engine,
};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HTTP_AGENT: &str = "native-feature-http-agent";
const NATIVE_AGENT: &str = "native-feature-browser-agent";
const RENDERED: &str = "native-script-ran";

async fn site_with_an_http_challenge() -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                "<html><body><p id='result'>unrendered</p>\
                <script>document.getElementById('result').textContent='{RENDERED}'</script></body></html>"
            ),
            "text/html",
        ))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("user-agent", HTTP_AGENT))
        .respond_with(ResponseTemplate::new(403).set_body_string("HTTP challenge"))
        .with_priority(1)
        .mount(&site)
        .await;
    site
}

fn config(mode: BrowserMode) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode,
            ..BrowserConfig::default()
        },
        user_agent: Some(NATIVE_AGENT.to_owned()),
        user_agents: vec![HTTP_AGENT.to_owned()],
        respect_robots_txt: false,
        max_depth: Some(0),
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BrowserOnly,
            ..DispatchProfile::default()
        }),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

#[tokio::test]
async fn forced_native_crawls_execute_scripts_without_an_http_attempt() {
    for mode in [BrowserMode::Always, BrowserMode::Stealth] {
        let site = site_with_an_http_challenge().await;
        let engine = create_engine(Some(config(mode))).expect("native engine");
        let result = crawl(&engine, &site.uri()).await.expect("native crawl");
        assert_eq!(
            result.error, None,
            "forced native crawl must not report an unavailable browser tier"
        );
        let page = result.pages.first().expect("seed page");
        assert!(
            page.html.contains(&format!("<p id=\"result\">{RENDERED}</p>")),
            "native browser must execute the page script: {page:?}"
        );
        let requests = site.received_requests().await.expect("requests");
        assert!(
            requests.iter().all(
                |request| request.headers.get("user-agent").and_then(|agent| agent.to_str().ok()) != Some(HTTP_AGENT)
            ),
            "a forced native crawl must not send a rotated HTTP agent"
        );
    }
}

#[tokio::test]
async fn an_auto_native_crawl_escalates_the_http_challenge_to_rendering() {
    let site = site_with_an_http_challenge().await;
    let engine = create_engine(Some(config(BrowserMode::Auto))).expect("native engine");
    let result = crawl(&engine, &site.uri()).await.expect("native crawl");
    assert_eq!(
        result.error, None,
        "native escalation must not report an unavailable browser tier"
    );
    let page = result.pages.first().expect("seed page");
    assert!(
        page.html.contains(&format!("<p id=\"result\">{RENDERED}</p>")),
        "native escalation must execute the page script: {page:?}"
    );
    let requests = site.received_requests().await.expect("requests");
    let agents = requests
        .iter()
        .filter_map(|request| request.headers.get("user-agent").and_then(|agent| agent.to_str().ok()))
        .collect::<Vec<_>>();
    assert_eq!(agents, [HTTP_AGENT, NATIVE_AGENT]);
}
