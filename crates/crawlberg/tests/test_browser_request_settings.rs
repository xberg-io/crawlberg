#![cfg(feature = "browser")]

use crawlberg::traits::RateLimiter;
use crawlberg::{
    BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, CrawlConfig, CrawlEngine, CrawlEngineHandle, CrawlError,
    PerDomainThrottle, create_engine, scrape,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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

/// The throttle of the engine, with the time of each turn it grants.
struct RecordedTurns {
    throttle: PerDomainThrottle,
    turns: Arc<Mutex<Vec<(String, Instant)>>>,
}

#[async_trait::async_trait]
impl RateLimiter for RecordedTurns {
    async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
        self.throttle.acquire(domain).await?;
        // ~keep Stamped in the poll that grants the turn: no await lies between the two.
        self.turns.lock().expect("turns").push((domain.to_owned(), Instant::now()));
        Ok(())
    }

    async fn record_response(&self, domain: &str, status: u16) -> Result<(), CrawlError> {
        self.throttle.record_response(domain, status).await
    }

    async fn set_crawl_delay(&self, domain: &str, delay: Duration) -> Result<(), CrawlError> {
        self.throttle.set_crawl_delay(domain, delay).await
    }
}

/// An engine with `rate_limiter` and a pool of pages on one Chrome, the shape `create_engine`
/// makes. The caller shuts the pool down.
fn engine_with_rate_limiter(
    settings: CrawlConfig,
    rate_limiter: impl RateLimiter + 'static,
) -> (CrawlEngineHandle, Arc<BrowserPool>) {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let engine = CrawlEngine::builder()
        .config(settings)
        .rate_limiter(rate_limiter)
        .with_browser_pool(Arc::clone(&pool))
        .build()
        .expect("engine");
    (CrawlEngineHandle::from_engine(engine), pool)
}

/// Two pages of one host at once: the second gets its turn one delay after the first, no page is
/// asked for before its turn, and the first page is asked for before the second turn comes.
#[tokio::test]
async fn should_space_browser_requests_to_the_same_domain() {
    // ~keep The first page must be asked for within one delay of its turn, on a busy host too:
    // ~keep the first request of a new Chrome took up to about half a second there.
    const DELAY: Duration = Duration::from_millis(4000);
    let site = MockServer::start().await;
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&arrivals);
    Mock::given(method("GET"))
        .respond_with(move |request: &wiremock::Request| {
            let path = request.url.path().to_owned();
            if path != FAVICON_PATH {
                record.lock().expect("record").push((path, Instant::now()));
            }
            ResponseTemplate::new(200).set_body_raw("<p>paced</p>", "text/html")
        })
        .mount(&site)
        .await;
    let turns = Arc::new(Mutex::new(Vec::new()));
    let rate_limiter = RecordedTurns {
        throttle: PerDomainThrottle::new(DELAY),
        turns: Arc::clone(&turns),
    };
    let mut settings = config();
    // ~keep As `create_engine` sets it: a parked page runs without an SSRF watch (#179).
    settings.browser.session_affinity = false;
    let (engine, pool) = engine_with_rate_limiter(settings, rate_limiter);
    let first_url = format!("{}/first", site.uri());
    let second_url = format!("{}/second", site.uri());
    let (first, second) = tokio::join!(scrape(&engine, &first_url), scrape(&engine, &second_url));
    pool.shutdown().await;
    first.expect("first");
    second.expect("second");
    let turns = turns.lock().expect("turns").clone();
    let arrivals = arrivals.lock().expect("arrivals").clone();

    let hosts: Vec<&str> = turns.iter().map(|(domain, _)| domain.as_str()).collect();
    assert_eq!(hosts, ["127.0.0.1", "127.0.0.1"], "each page takes one turn of its host");
    // ~keep The product spaces the turns. The time from a turn to the arrival of the request at
    // ~keep the server is not the same for the two pages (the first page of a new Chrome is
    // ~keep slower to send, by 57 to 435 ms in measured runs), so the gap between the arrivals
    // ~keep is not asserted: it was under the delay in 3 of 41 runs on a busy host.
    let turn_gap = turns[1].1.duration_since(turns[0].1);
    assert!(
        turn_gap >= DELAY,
        "the second turn must come one delay of {DELAY:?} after the first, got {turn_gap:?}"
    );
    let mut pages: Vec<&str> = arrivals.iter().map(|(path, _)| path.as_str()).collect();
    pages.sort_unstable();
    assert_eq!(pages, ["/first", "/second"], "each page is asked for once");
    assert!(
        arrivals[0].1 >= turns[0].1 && arrivals[1].1 >= turns[1].1,
        "no page is asked for before its turn: turns {turns:?}, arrivals {arrivals:?}"
    );
    assert!(
        arrivals[0].1 < turns[1].1,
        "the first page must be asked for before the second turn comes: turns {turns:?}, arrivals {arrivals:?}"
    );
}

/// A delay of the domain longer than the deadline of a page: the second page loads all the
/// same, because its deadline starts when its turn comes.
#[tokio::test]
async fn should_give_a_page_its_whole_deadline_after_the_delay_of_the_domain() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>paced</p>", "text/html"))
        .mount(&site)
        .await;
    let mut settings = config();
    settings.rate_limit_ms = Some(9000);
    settings.rate_limit_jitter_ratio = 0.0;
    settings.browser.overall_timeout = Duration::from_secs(6);
    let engine = create_engine(Some(settings)).expect("engine");
    let first_url = format!("{}/first", site.uri());
    let second_url = format!("{}/second", site.uri());
    let (first, second) = tokio::join!(scrape(&engine, &first_url), scrape(&engine, &second_url));
    first.expect("the first page loads at once");
    second.expect("the second page loads after the delay of the domain");
    let mut pages = paths_the_page_asked_for(&site).await;
    pages.sort_unstable();
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
