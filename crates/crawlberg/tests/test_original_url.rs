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
    let errors = RecordedFetchErrors::default();
    let engine = CrawlEngine::builder()
        .config(config)
        .event_emitter(errors.clone())
        .build()
        .expect("engine");
    let seed = format!("{base}/#seed");
    let result = engine.crawl(&seed).await.expect("crawl");
    assert_eq!(result.pages.len(), hrefs.len() + 1, "{}", {
        let fetch_errors = errors.0.lock().expect("recorded fetch errors").clone();
        let requests = server.received_requests().await.expect("recorded requests");
        format!(
            concat!(
                "crawl error: {:?}; returned pages: {:?}; expected links: {:?}; ",
                "fetch errors: {:?}; requests: {:?}; Chrome: {:?}"
            ),
            result.error,
            result
                .pages
                .iter()
                .map(|page| (&page.url, &page.original_url, &page.final_url))
                .collect::<Vec<_>>(),
            hrefs.iter().map(|href| format!("{base}{href}")).collect::<Vec<_>>(),
            fetch_errors,
            requests.iter().map(|request| &request.url).collect::<Vec<_>>(),
            std::env::var_os("CHROME")
        )
    });
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

/// The crawl removes the tracking parameter of the seed, so the fetched address is another text.
#[tokio::test]
async fn the_seed_page_gives_the_address_as_the_caller_passed_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
        .mount(&server)
        .await;
    let config = CrawlConfig {
        max_depth: Some(0),
        respect_robots_txt: false,
        strip_tracking_params: true,
        tracking_params: vec!["utm_*".to_owned()],
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let engine = CrawlEngine::builder().config(config).build().expect("engine");
    let seed = format!("{}/?utm_source=mail#seed", server.uri());
    let result = engine.crawl(&seed).await.expect("crawl");
    assert_eq!(result.pages.len(), 1, "crawl error: {:?}", result.error);
    assert_eq!(result.pages[0].original_url, seed);
}

#[tokio::test]
async fn should_preserve_link_spelling_in_http_results_after_normalization_and_redirects() {
    assert_original_link_spelling(BrowserMode::Never).await;
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn should_preserve_link_spelling_in_browser_results_after_normalization_and_redirects() {
    // ~keep Retain the controller or transport failure before a cancelled page command obscures it.
    tracing_subscriber::fmt()
        .with_env_filter(concat!(
            "crawlberg::browser_pool=debug,",
            "crawlberg::ssrf_intercept=debug,",
            "chromiumoxide::handler=warn"
        ))
        .with_ansi(false)
        .with_test_writer()
        .try_init()
        .expect("install browser failure diagnostics");
    assert_original_link_spelling(BrowserMode::Always).await;
}

#[tokio::test]
async fn should_remove_link_credentials_without_rewriting_query_or_fragment() {
    let cases = [
        (r"/\user:pass@other.test/x?y=://z@w", "http://other.test/x?y=://z@w"),
        (r"http://user:pass@other.test/a", r"http://other.test/a"),
        (r"http:\\user:pass@other.test\a", "http://other.test/a"),
        (r"http:/\user:pass@other.test/a", "http://other.test/a"),
        (
            r"http:\\user:pass@other.test\x?y=://z@w",
            "http://other.test/x?y=://z@w",
        ),
        (
            r"http:/\user:pass@other.test/x#f=://z@w",
            "http://other.test/x#f=://z@w",
        ),
    ];
    let server = MockServer::start().await;
    let html = cases
        .iter()
        .map(|(href, _)| format!("<a href=\"{href}\">link</a>"))
        .collect::<String>();
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html, "text/html"))
        .mount(&server)
        .await;
    let engine = CrawlEngine::builder()
        .config(CrawlConfig {
            respect_robots_txt: false,
            ..CrawlConfig::builder().allow_private_networks(true).build()
        })
        .build()
        .expect("engine");
    let result = engine.scrape(&server.uri()).await.expect("scrape");
    assert_eq!(result.links.len(), cases.len());
    for (link, (href, expected)) in result.links.iter().zip(cases) {
        assert_eq!(link.original_url, expected, "href: {href}");
        let parsed = url::Url::parse(&link.url).expect("resolved URL");
        assert_eq!(parsed.username(), "");
        assert_eq!(parsed.password(), None);
    }
}

#[derive(Clone, Default)]
struct RecordedFetchErrors(std::sync::Arc<std::sync::Mutex<Vec<crawlberg::traits::ErrorEvent>>>);

#[async_trait::async_trait]
impl crawlberg::traits::EventEmitter for RecordedFetchErrors {
    async fn on_page(&self, _: &crawlberg::traits::PageEvent) {}

    async fn on_error(&self, event: &crawlberg::traits::ErrorEvent) {
        self.0.lock().expect("recorded fetch errors").push(event.clone());
    }

    async fn on_complete(&self, _: &crawlberg::traits::CompleteEvent) {}

    async fn on_discovered(&self, _: &str, _: usize) {}
}
