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

/// The address a full Chrome asks for on its own, for the icon of the tab. The page does not
/// send this request, and the headless shell does not send it at all.
const FAVICON_PATH: &str = "/favicon.ico";

/// The paths `server` was asked for, in the order of arrival, without the icon of the tab.
async fn paths_the_page_asked_for(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .map(|request| request.url.path().to_owned())
        .filter(|path| path != FAVICON_PATH)
        .collect()
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
        .respond_with(move |request: &wiremock::Request| {
            if request.url.path() != FAVICON_PATH {
                record.lock().expect("record").push(Instant::now());
            }
            ResponseTemplate::new(200).set_body_raw("<p>paced</p>", "text/html")
        })
        .mount(&site)
        .await;
    let mut settings = config();
    settings.rate_limit_ms = Some(2000);
    settings.rate_limit_jitter_ratio = 0.0;
    let engine = create_engine(Some(settings)).expect("engine");
    let first_url = format!("{}/first", site.uri());
    let second_url = format!("{}/second", site.uri());
    let (first, second) = tokio::join!(scrape(&engine, &first_url), scrape(&engine, &second_url));
    first.expect("first");
    second.expect("second");
    let arrivals = arrivals.lock().expect("arrivals").clone();
    assert_eq!(arrivals.len(), 2, "each page is asked for once");
    // ~keep The delay runs from the moment a fetch gets its turn, not from the arrival of its
    // ~keep request. The first page of a new Chrome takes longer to send, so the gap at the
    // ~keep server is a little under the delay: 57 to 107 ms under it in three measured runs.
    let gap = arrivals[1].duration_since(arrivals[0]);
    assert!(
        gap >= Duration::from_millis(1700),
        "the two pages must arrive about one delay of 2000 ms apart, got {gap:?}"
    );
    let mut pages = paths_the_page_asked_for(&site).await;
    pages.sort();
    assert_eq!(pages, ["/first", "/second"]);
}

#[tokio::test]
async fn should_block_browser_media_url_patterns() {
    let site = MockServer::start().await;
    let page = "<h1>Media</h1><p>text</p><img src='/media/a.png' alt='a'>\
        <video src='/media/v.mp4' poster='/media/p.png'></video><audio src='/media/a.mp3'></audio>";
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(page, "text/html"))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("x", "application/octet-stream"))
        .with_priority(10)
        .mount(&site)
        .await;
    let mut settings = config();
    settings.browser.block_url_patterns = vec!["*/media/*".into()];
    let engine = create_engine(Some(settings)).expect("engine");
    scrape(&engine, &format!("{}/", site.uri())).await.expect("page");
    assert_eq!(
        paths_the_page_asked_for(&site).await,
        ["/"],
        "no media file of the page must reach the server"
    );
}

/// The control of the test above: with no pattern the same page loads its four media files, so
/// the count of one above comes from the patterns and not from a page that loads nothing.
#[tokio::test]
async fn a_browser_fetch_with_no_block_pattern_loads_the_media_of_the_page() {
    let site = MockServer::start().await;
    let page = "<h1>Media</h1><p>text</p><img src='/media/a.png' alt='a'>\
        <video src='/media/v.mp4' poster='/media/p.png'></video><audio src='/media/a.mp3'></audio>";
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(page, "text/html"))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("x", "application/octet-stream"))
        .with_priority(10)
        .mount(&site)
        .await;
    let engine = create_engine(Some(config())).expect("engine");
    scrape(&engine, &format!("{}/", site.uri())).await.expect("page");
    let mut asked = paths_the_page_asked_for(&site).await;
    asked.sort();
    assert_eq!(
        asked,
        ["/", "/media/a.mp3", "/media/a.png", "/media/p.png", "/media/v.mp4"]
    );
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
    assert_eq!(paths_the_page_asked_for(&site).await, ["/start", "/"]);
    assert_eq!(paths_the_page_asked_for(&script).await, ["/script.js"]);
    // ~keep The headers are read on every request, also on the one for the icon of the tab.
    for server in [&site, &script] {
        let requests = server.received_requests().await.expect("requests");
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
