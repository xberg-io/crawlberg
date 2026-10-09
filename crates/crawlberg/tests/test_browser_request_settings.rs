#![cfg(feature = "browser")]

use crawlberg::{BrowserConfig, BrowserMode, CrawlConfig, create_engine, scrape};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            mode: BrowserMode::Always,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        retry_initial_delay_ms: 10,
        retry_max_delay_ms: 20,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

#[tokio::test]
async fn should_retry_browser_rate_limits_with_the_configured_budget() {
    let site = MockServer::start().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&hits);
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(move |_: &wiremock::Request| {
            let status = if count.fetch_add(1, Ordering::SeqCst) < 2 {
                429
            } else {
                200
            };
            ResponseTemplate::new(status).set_body_raw("<p>accepted after refusals</p>", "text/html")
        })
        .mount(&site)
        .await;
    let mut settings = config();
    settings.retry_count = 2;
    settings.retry_codes = vec![429];
    let engine = create_engine(Some(settings)).expect("engine");
    let result = scrape(&engine, &format!("{}/", site.uri()))
        .await
        .expect("retry succeeds");
    assert_eq!(result.status_code, 200);
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn should_space_browser_requests_to_the_same_domain() {
    let site = MockServer::start().await;
    let arrivals = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = Arc::clone(&arrivals);
    Mock::given(method("GET"))
        .respond_with(move |_: &wiremock::Request| {
            record.lock().expect("record").push(Instant::now());
            ResponseTemplate::new(200).set_body_raw("<p>paced</p>", "text/html")
        })
        .mount(&site)
        .await;
    let mut settings = config();
    settings.rate_limit_ms = Some(1200);
    settings.rate_limit_jitter_ratio = 0.0;
    let engine = create_engine(Some(settings)).expect("engine");
    let first_url = format!("{}/first", site.uri());
    let second_url = format!("{}/second", site.uri());
    let (first, second) = tokio::join!(scrape(&engine, &first_url), scrape(&engine, &second_url));
    first.expect("first");
    second.expect("second");
    let arrivals = arrivals.lock().expect("arrivals").clone();
    assert_eq!(arrivals.len(), 2);
    assert!(arrivals[1].duration_since(arrivals[0]) >= Duration::from_millis(1100));
    assert_eq!(site.received_requests().await.expect("requests").len(), 2);
}

#[tokio::test]
async fn should_block_browser_media_url_patterns() {
    let site = MockServer::start().await;
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>text</p><img src='/media/image.png'>", "text/html"))
        .mount(&site)
        .await;
    Mock::given(path("/media/image.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("image", "image/png"))
        .mount(&site)
        .await;
    let mut settings = config();
    settings.browser.block_url_patterns = vec!["*/media/*".into()];
    let engine = create_engine(Some(settings)).expect("engine");
    scrape(&engine, &format!("{}/", site.uri())).await.expect("page");
    let requests = site.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1, "media must never reach the server: {requests:?}");
}

#[tokio::test]
async fn should_send_matching_client_hints_to_start_redirect_and_cross_host_script() {
    let script = MockServer::start().await;
    Mock::given(path("/script.js"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("globalThis.loaded = true;", "application/javascript"))
        .mount(&script)
        .await;
    let site = MockServer::start().await;
    Mock::given(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
        .mount(&site)
        .await;
    let script_url = format!("{}/script.js", script.uri().replace("127.0.0.1", "localhost"));
    Mock::given(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(format!("<p>hints</p><script src='{script_url}'></script>"), "text/html"),
        )
        .mount(&site)
        .await;
    let mut settings = config();
    settings.user_agent = Some(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36".into(),
    );
    let engine = create_engine(Some(settings)).expect("engine");
    scrape(&engine, &format!("{}/start", site.uri())).await.expect("page");
    for (server, count) in [(&site, 2), (&script, 1)] {
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), count);
        for request in requests {
            let headers = &request.headers;
            assert_eq!(headers.get("sec-ch-ua-mobile").expect("mobile hint"), "?0");
            assert_eq!(
                headers.get("user-agent").expect("user agent"),
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36"
            );
            assert_eq!(headers.get("sec-ch-ua-platform").expect("platform hint"), "\"Linux\"");
            let brands = headers
                .get("sec-ch-ua")
                .expect("brand hints")
                .to_str()
                .expect("text header");
            assert!(brands.contains("\"Chromium\";v=\"116\""), "{brands}");
            assert!(brands.contains("\"Google Chrome\";v=\"116\""), "{brands}");
            assert!(!brands.contains("HeadlessChrome"), "{brands}");
        }
    }
}
