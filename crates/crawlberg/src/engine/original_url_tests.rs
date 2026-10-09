use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn should_preserve_source_spelling_in_sequential_crawl() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<a href="/t/naïve">link</a><a href="/old">redirect</a>"#,
            "text/html; charset=utf-8",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/new"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
        .with_priority(10)
        .mount(&server)
        .await;
    let config = CrawlConfig {
        max_depth: Some(1),
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let engine = CrawlEngine::builder().config(config).build().expect("engine");
    let seed = format!("{}/#seed", server.uri());
    let (engine, admitted) = engine.admit(&seed).expect("seed");
    let result = engine.crawl_sequential(&admitted).await.expect("crawl");
    assert_eq!(result.pages.len(), 3);
    assert_eq!(result.pages[0].original_url, seed);
    for path in ["/t/naïve", "/old"] {
        assert!(
            result
                .pages
                .iter()
                .any(|page| page.original_url == format!("{}{path}", server.uri()))
        );
    }
    let redirected = result
        .pages
        .iter()
        .find(|page| page.original_url.ends_with("/old"))
        .expect("redirected page");
    assert_eq!(redirected.final_url, format!("{}/new", server.uri()));
}
