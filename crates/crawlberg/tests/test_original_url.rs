use crawlberg::{BrowserMode, CrawlConfig, CrawlEngine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn assert_original_link_spelling(mode: BrowserMode) {
    let server = MockServer::start().await;
    let base = server.uri();
    let hrefs = [
        "/t/naïve",
        "/t/raw space",
        "/t/b\\s",
        "/t/d/%2e%2e/up",
        "/t/ca^ret",
        "/t/old",
    ];
    let html = hrefs
        .iter()
        .map(|href| format!("<a href=\"{href}\">link</a>"))
        .collect::<String>();
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html, "text/html; charset=utf-8"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/t/old"))
        .respond_with(ResponseTemplate::new(301).append_header("location", "/t/new"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
        .with_priority(10)
        .mount(&server)
        .await;
    let config = CrawlConfig {
        browser: crawlberg::BrowserConfig {
            mode,
            chrome_path: std::env::var_os("CHROME").map(Into::into),
            ..Default::default()
        },
        max_depth: Some(1),
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let engine = CrawlEngine::builder().config(config).build().expect("engine");
    let seed = format!("{base}/#seed");
    let result = engine.crawl(&seed).await.expect("crawl");
    assert_eq!(result.pages.len(), hrefs.len() + 1, "crawl error: {:?}", result.error);
    let start = result.pages.iter().find(|page| page.depth == 0).expect("seed page");
    assert_eq!(start.original_url, seed);
    assert_eq!(
        start
            .links
            .iter()
            .map(|link| link.original_url.clone())
            .collect::<Vec<_>>(),
        hrefs.iter().map(|href| format!("{base}{href}")).collect::<Vec<_>>()
    );
    for href in hrefs {
        let original = format!("{base}{href}");
        let page = result
            .pages
            .iter()
            .find(|page| page.original_url == original)
            .expect("followed link provenance");
        if href == "/t/old" {
            assert_eq!(page.final_url, format!("{base}/t/new"));
        }
    }
}

#[tokio::test]
async fn should_preserve_link_spelling_in_http_results_after_normalization_and_redirects() {
    assert_original_link_spelling(BrowserMode::Never).await;
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn should_preserve_link_spelling_in_browser_results_after_normalization_and_redirects() {
    assert_original_link_spelling(BrowserMode::Always).await;
}
