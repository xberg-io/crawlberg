use crawlberg::{CrawlConfig, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config() -> CrawlConfig {
    CrawlConfig {
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

#[tokio::test]
async fn five_scrapes_share_the_engines_robots_rules() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .expect(1)
        .mount(&site)
        .await;
    Mock::given(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<p>page</p>"))
        .expect(5)
        .mount(&site)
        .await;
    let engine = create_engine(Some(CrawlConfig {
        respect_robots_txt: true,
        ..config()
    }))
    .unwrap();
    for _ in 0..5 {
        let result = scrape(&engine, &format!("{}/page", site.uri())).await.unwrap();
        assert!(result.is_allowed);
    }
    site.verify().await;
}

#[tokio::test]
async fn delayed_refresh_header_is_a_link_against_the_response_address() {
    let site = MockServer::start().await;
    Mock::given(path("/page"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<base href='/different/'><p>page</p>", "text/html")
                .append_header("refresh", "5; url=/later"),
        )
        .mount(&site)
        .await;
    let engine = create_engine(Some(config())).unwrap();
    let result = scrape(&engine, &format!("{}/page", site.uri())).await.unwrap();
    assert_eq!(
        result.links.iter().map(|link| link.url.as_str()).collect::<Vec<_>>(),
        [format!("{}/later", site.uri())],
        "{result:?}"
    );
}

#[tokio::test]
async fn a_headerless_heading_fragment_reports_conversion_failure() {
    let site = MockServer::start().await;
    Mock::given(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"<h2> </h2><p>body text</p>"))
        .mount(&site)
        .await;
    let engine = create_engine(Some(config())).unwrap();
    let result = scrape(&engine, &format!("{}/page", site.uri())).await;
    assert!(
        matches!(result, Err(ref error) if error.to_string().starts_with("conversion_failed:")),
        "{result:?}"
    );
}

#[tokio::test]
async fn map_spaces_robots_and_three_advertised_sitemaps() {
    let site = MockServer::start().await;
    let robots = format!(
        "User-agent: *\nAllow: /\nSitemap: {0}/one.xml\nSitemap: {0}/two.xml\nSitemap: {0}/three.xml\n",
        site.uri()
    );
    Mock::given(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(robots))
        .expect(1)
        .mount(&site)
        .await;
    for name in ["one", "two", "three"] {
        Mock::given(path(format!("/{name}.xml")))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "<?xml version='1.0'?><urlset><url><loc>{}/{name}</loc></url></urlset>",
                site.uri()
            )))
            .expect(1)
            .mount(&site)
            .await;
    }
    let engine = create_engine(Some(CrawlConfig {
        respect_robots_txt: true,
        rate_limit_ms: Some(150),
        rate_limit_jitter_ratio: 0.0,
        ..config()
    }))
    .unwrap();
    let start = std::time::Instant::now();
    let result = crawlberg::map_urls(&engine, &site.uri()).await.unwrap();
    assert_eq!(result.urls.len(), 3);
    assert!(
        start.elapsed() >= std::time::Duration::from_millis(450),
        "{:?}",
        start.elapsed()
    );
    site.verify().await;
}

#[tokio::test]
async fn maps_use_the_default_host_delay_when_rate_limit_ms_is_unset() {
    let site = MockServer::start().await;
    Mock::given(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("User-agent: *\nAllow: /\nSitemap: {}/one.xml\n", site.uri())),
        )
        .expect(1)
        .mount(&site)
        .await;
    Mock::given(path("/one.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<?xml version='1.0'?><urlset><url><loc>{}/page</loc></url></urlset>",
            site.uri()
        )))
        .expect(1)
        .mount(&site)
        .await;
    let engine = create_engine(Some(CrawlConfig {
        respect_robots_txt: true,
        rate_limit_ms: None,
        rate_limit_jitter_ratio: 0.0,
        ..config()
    }))
    .unwrap();
    let start = std::time::Instant::now();
    let result = crawlberg::map_urls(&engine, &site.uri()).await.unwrap();
    assert_eq!(result.urls.len(), 1);
    assert!(
        start.elapsed() >= std::time::Duration::from_millis(200),
        "the second same-host request must wait for the default delay: {:?}",
        start.elapsed()
    );
    site.verify().await;
}

#[tokio::test]
async fn a_redirect_loop_has_the_redirect_limit_classification() {
    let site = MockServer::start().await;
    Mock::given(path("/loop"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/loop"))
        .mount(&site)
        .await;
    let engine = create_engine(Some(CrawlConfig {
        max_redirects: 3,
        ..config()
    }))
    .unwrap();
    let error = scrape(&engine, &format!("{}/loop", site.uri())).await.unwrap_err();
    assert!(error.to_string().starts_with("too_many_redirects:"), "{error:?}");
}

#[tokio::test]
async fn credentialed_scrapes_do_not_share_robots_rules() {
    let site = MockServer::start().await;
    Mock::given(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nAllow: /\n"))
        .expect(5)
        .mount(&site)
        .await;
    Mock::given(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
        .expect(5)
        .mount(&site)
        .await;
    let mut config = CrawlConfig {
        respect_robots_txt: true,
        ..config()
    };
    config.auth = Some(crawlberg::AuthConfig::Bearer {
        token: "fixture".to_string(),
    });
    let engine = create_engine(Some(config)).unwrap();
    for _ in 0..5 {
        assert!(
            scrape(&engine, &format!("{}/page", site.uri()))
                .await
                .unwrap()
                .is_allowed
        );
    }
    site.verify().await;
}

#[tokio::test]
async fn map_discovers_a_delayed_refresh_target_without_fetching_it() {
    let site = MockServer::start().await;
    Mock::given(path("/sitemap.xml"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&site)
        .await;
    Mock::given(path("/page"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<p>page</p>", "text/html")
                .append_header("refresh", "5; url=/later"),
        )
        .expect(1)
        .mount(&site)
        .await;
    Mock::given(path("/later"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&site)
        .await;
    let engine = create_engine(Some(config())).unwrap();
    let result = crawlberg::map_urls(&engine, &format!("{}/page", site.uri()))
        .await
        .unwrap();
    assert_eq!(
        result.urls.iter().map(|url| url.url.as_str()).collect::<Vec<_>>(),
        [format!("{}/later", site.uri())]
    );
    site.verify().await;
}

#[derive(Clone, Default)]
struct RecordedLimiter(std::sync::Arc<std::sync::Mutex<Vec<(String, u16)>>>);

#[async_trait::async_trait]
impl crawlberg::traits::RateLimiter for RecordedLimiter {
    async fn acquire(&self, domain: &str) -> Result<(), crawlberg::CrawlError> {
        self.0.lock().unwrap().push((domain.to_string(), 0));
        Ok(())
    }
    async fn record_response(&self, domain: &str, status: u16) -> Result<(), crawlberg::CrawlError> {
        self.0.lock().unwrap().push((domain.to_string(), status));
        Ok(())
    }
    async fn set_crawl_delay(&self, _: &str, _: std::time::Duration) -> Result<(), crawlberg::CrawlError> {
        Ok(())
    }
}

#[tokio::test]
async fn map_reports_each_redirect_child_and_429_to_its_host_limiter() {
    let site = MockServer::start().await;
    let children = MockServer::start().await;
    let child_origin = children.uri().replace("127.0.0.1", "localhost");
    Mock::given(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("User-agent: *\nAllow: /\nSitemap: {child_origin}/root.xml\n")),
        )
        .expect(2)
        .mount(&site)
        .await;
    Mock::given(path("/root.xml"))
        .respond_with(ResponseTemplate::new(302).append_header("location", format!("{}/index.xml", site.uri())))
        .expect(1)
        .mount(&children)
        .await;
    Mock::given(path("/index.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<?xml version='1.0'?><sitemapindex><sitemap><loc>{child_origin}/leaf.xml</loc></sitemap></sitemapindex>"
        )))
        .expect(1)
        .mount(&site)
        .await;
    Mock::given(path("/leaf.xml"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .mount(&children)
        .await;
    Mock::given(path("/sitemap.xml"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&site)
        .await;
    Mock::given(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<a href='/found'>page</a>", "text/html"))
        .expect(1)
        .mount(&site)
        .await;
    let limiter = RecordedLimiter::default();
    let engine = crawlberg::CrawlEngine::builder()
        .config(CrawlConfig {
            respect_robots_txt: true,
            retry_count: 0,
            ..config()
        })
        .rate_limiter(limiter.clone())
        .build()
        .unwrap();
    let result = engine.map(&format!("{}/page", site.uri())).await.unwrap();
    assert_eq!(result.urls.len(), 1);
    let expected_statuses = [
        ("127.0.0.1", 200),
        ("localhost", 302),
        ("127.0.0.1", 200),
        ("localhost", 429),
        ("127.0.0.1", 404),
        ("127.0.0.1", 200),
        ("127.0.0.1", 200),
    ];
    let expected: Vec<_> = expected_statuses
        .into_iter()
        .flat_map(|(host, status)| [(host.to_string(), 0), (host.to_string(), status)])
        .collect();
    assert_eq!(*limiter.0.lock().unwrap(), expected);
    site.verify().await;
    children.verify().await;
}

#[tokio::test]
async fn crawl_discovers_a_delayed_refresh_target_as_a_separate_page() {
    let site = MockServer::start().await;
    Mock::given(path("/page"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<p>first page</p>", "text/html")
                .append_header("refresh", "5; url=/later"),
        )
        .expect(1)
        .mount(&site)
        .await;
    Mock::given(path("/later"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>later page</p>", "text/html"))
        .expect(1)
        .mount(&site)
        .await;
    let engine = create_engine(Some(CrawlConfig {
        max_depth: Some(1),
        ..config()
    }))
    .unwrap();
    let result = crawlberg::crawl(&engine, &format!("{}/page", site.uri()))
        .await
        .unwrap();
    let mut urls = result.pages.iter().map(|page| page.url.clone()).collect::<Vec<_>>();
    urls.sort();
    assert_eq!(urls, [format!("{}/later", site.uri()), format!("{}/page", site.uri())]);
    site.verify().await;
}
