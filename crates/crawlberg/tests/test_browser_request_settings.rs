#![cfg(feature = "browser")]

use crawlberg::traits::RateLimiter;
use crawlberg::{
    BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, CrawlConfig, CrawlEngine, CrawlEngineHandle,
    PerDomainThrottle, create_engine, scrape,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::RecordedTurns;

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
    assert_eq!(
        hosts,
        ["127.0.0.1", "127.0.0.1"],
        "each page takes one turn of its host"
    );
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

/// With the default delay settings, `overall_timeout` bounds the whole fetch, the wait for a
/// page of the pool included. Two pages of 3 s each share a pool of one page with a deadline of
/// 5 s: one loads, and the other fails at the deadline, counted from the start of its fetch.
#[tokio::test]
async fn should_bound_the_whole_fetch_by_the_deadline_with_the_default_delay_settings() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/warm"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>warm</p>", "text/html"))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<p>slow</p>", "text/html")
                .set_delay(Duration::from_secs(3)),
        )
        .with_priority(10)
        .mount(&site)
        .await;
    let pool = BrowserPool::new(BrowserPoolConfig {
        max_pages: 1,
        ..BrowserPoolConfig::default()
    });
    let mut settings = config();
    settings.browser.session_affinity = false;
    settings.browser.timeout = Duration::from_secs(4);
    settings.browser.overall_timeout = Duration::from_secs(5);
    settings.browser_pool = Some(Arc::clone(&pool));
    let engine = create_engine(Some(settings)).expect("engine");
    // ~keep The launch of Chrome is not part of what the two fetches below measure.
    scrape(&engine, &format!("{}/warm", site.uri())).await.expect("warm");
    let timed = |url: String| {
        let engine = &engine;
        async move {
            let started = Instant::now();
            let outcome = scrape(engine, &url).await;
            (
                started.elapsed(),
                outcome.map(|_| ()).map_err(|error| error.to_string()),
            )
        }
    };
    let (first, second) = tokio::join!(
        timed(format!("{}/slow-1", site.uri())),
        timed(format!("{}/slow-2", site.uri()))
    );
    pool.shutdown().await;
    let (loaded, failed): (Vec<_>, Vec<_>) = [first, second].into_iter().partition(|(_, outcome)| outcome.is_ok());
    assert_eq!(
        (loaded.len(), failed.len()),
        (1, 1),
        "one page must load and the other must fail at the deadline: loaded {loaded:?}, failed {failed:?}"
    );
    let (elapsed, error) = &failed[0];
    let error = error.as_ref().expect_err("the failed fetch");
    assert!(
        error.contains("exceeded the overall deadline of 5s"),
        "the fetch must fail on its deadline: {error}"
    );
    assert!(
        *elapsed >= Duration::from_millis(4900) && *elapsed < Duration::from_millis(6000),
        "the fetch must fail 5 s after its start, not a deadline later: {elapsed:?}"
    );
}

/// A delay of the domain longer than the deadline of a page: the second page loads all the
/// same, because the wait for its turn is not counted against its deadline.
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

struct HeldBrowserClock(tokio::task::JoinHandle<()>);

impl HeldBrowserClock {
    fn pause() -> Self {
        let awake = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        tokio::time::pause();
        Self(awake)
    }
}

impl Drop for HeldBrowserClock {
    fn drop(&mut self) {
        tokio::time::resume();
        self.0.abort();
    }
}

async fn wait_for_browser_progress(mut ready: impl FnMut() -> bool, phase: &str) {
    let started = Instant::now();
    while !ready() {
        assert!(started.elapsed() < Duration::from_secs(20), "browser stalled {phase}");
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn should_stop_real_browser_retries_at_one_overall_deadline() {
    const ATTEMPT_DELAY: Duration = Duration::from_secs(2);
    const FETCH_BUDGET: Duration = Duration::from_secs(30);
    let site = MockServer::start().await;
    Mock::given(path("/warm"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>ready</p>", "text/html"))
        .mount(&site)
        .await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    Mock::given(path("/retry"))
        .respond_with(move |_: &wiremock::Request| {
            let attempt = observed.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(429)
                .set_body_raw("<p>retry</p>", "text/html")
                .set_delay(if attempt == 0 { ATTEMPT_DELAY } else { FETCH_BUDGET * 2 })
        })
        .mount(&site)
        .await;
    let mut settings = config();
    settings.retry_count = 2;
    settings.retry_codes = vec![429];
    settings.retry_initial_delay_ms = 0;
    settings.retry_max_delay_ms = 0;
    settings.browser.overall_timeout = FETCH_BUDGET;
    settings.browser.wait = crawlberg::BrowserWait::Selector;
    settings.browser.wait_selector = Some("body".into());
    settings.browser.extra_wait = Some(Duration::ZERO);
    settings.browser.session_affinity = false;
    let mut warm_settings = settings.clone();
    warm_settings.browser.overall_timeout = Duration::from_secs(20);
    let (warm_engine, pool) = engine_with_rate_limiter(warm_settings, PerDomainThrottle::new(Duration::ZERO));
    scrape(&warm_engine, &format!("{}/warm", site.uri()))
        .await
        .expect("warm the pool");
    let engine = CrawlEngineHandle::from_engine(
        CrawlEngine::builder()
            .config(settings)
            .rate_limiter(PerDomainThrottle::new(Duration::ZERO))
            .with_browser_pool(pool.clone())
            .build()
            .expect("engine"),
    );
    let started = tokio::time::Instant::now();
    let url = format!("{}/retry", site.uri());
    let fetch = tokio::spawn(async move { scrape(&engine, &url).await });
    wait_for_browser_progress(|| attempts.load(Ordering::SeqCst) == 2, "before real retry").await;
    assert!(
        started.elapsed() < FETCH_BUDGET,
        "setup exhausted the shared fetch budget"
    );
    assert!(!fetch.is_finished(), "the second real request must still be pending");
    // ~keep Freeze only after real retry preparation; its CDP/grace timers must run normally.
    let clock = HeldBrowserClock::pause();
    let scheduling_margin = Duration::from_millis(100);
    tokio::time::advance(FETCH_BUDGET - started.elapsed() + scheduling_margin).await;
    wait_for_browser_progress(|| fetch.is_finished(), "at the shared deadline after two real requests").await;
    let outcome = fetch.await.expect("browser fetch task");
    let elapsed = started.elapsed();
    drop(clock);
    pool.shutdown().await;
    let retries = paths_the_page_asked_for(&site)
        .await
        .into_iter()
        .filter(|path| path == "/retry")
        .count();
    assert!(
        matches!(outcome, Err(crawlberg::CrawlError::BrowserTimeout { .. })),
        "{outcome:?}, {retries} requests in {elapsed:?}"
    );
    assert_eq!(
        elapsed,
        FETCH_BUDGET + scheduling_margin,
        "all real retries must share the original deadline"
    );
    assert_eq!(retries, 2);
}

struct HeldDomainTurn {
    waiting: Arc<tokio::sync::Notify>,
    released: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl RateLimiter for HeldDomainTurn {
    async fn acquire(&self, domain: &str) -> Result<(), crawlberg::CrawlError> {
        if domain == "localhost" {
            self.waiting.notify_one();
            self.released.notified().await;
        }
        Ok(())
    }
    async fn record_response(&self, _: &str, _: u16) -> Result<(), crawlberg::CrawlError> {
        Ok(())
    }
    async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), crawlberg::CrawlError> {
        Ok(())
    }
}

#[tokio::test]
async fn should_leave_the_only_browser_pool_page_free_while_waiting_for_a_domain() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>ready</p>", "text/html"))
        .mount(&site)
        .await;
    let waiting = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    let pool = BrowserPool::new(BrowserPoolConfig {
        max_pages: 1,
        ..BrowserPoolConfig::default()
    });
    let mut settings = config();
    settings.browser.session_affinity = false;
    let engine = CrawlEngineHandle::from_engine(
        CrawlEngine::builder()
            .config(settings)
            .rate_limiter(HeldDomainTurn {
                waiting: waiting.clone(),
                released: released.clone(),
            })
            .with_browser_pool(pool.clone())
            .build()
            .expect("engine"),
    );
    scrape(&engine, &format!("{}/warm", site.uri()))
        .await
        .expect("warm pool");
    let slow_engine = engine.clone();
    let slow_url = format!("{}/slow", site.uri().replace("127.0.0.1", "localhost"));
    let slow = tokio::spawn(async move { scrape(&slow_engine, &slow_url).await });
    tokio::time::timeout(Duration::from_secs(5), waiting.notified())
        .await
        .expect("domain waiter reached");
    let fast = tokio::time::timeout(Duration::from_secs(3), scrape(&engine, &format!("{}/fast", site.uri()))).await;
    released.notify_one();
    let slow = slow.await.expect("slow task");
    pool.shutdown().await;
    fast.expect("free host must obtain the pool page before the delayed host is released")
        .expect("fast page");
    slow.expect("delayed page");
    let mut paths = paths_the_page_asked_for(&site).await;
    paths.sort_unstable();
    assert_eq!(paths, ["/fast", "/slow", "/warm"]);
}

#[tokio::test]
async fn should_clear_blocked_url_patterns_when_reusing_a_retained_browser_page() {
    let site = MockServer::start().await;
    Mock::given(path("/blocked"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<script>window.name='retained-block-pattern-page';</script><p>blocked</p><img src='/first.png'>",
            "text/html",
        ))
        .mount(&site)
        .await;
    Mock::given(path("/clear"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<p>clear</p><img src='/second.png'><script>document.body.dataset.retained=window.name;</script>",
            "text/html",
        ))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "no-store")
                .set_body_raw("image", "image/png"),
        )
        .with_priority(10)
        .mount(&site)
        .await;
    let pool = BrowserPool::new(BrowserPoolConfig {
        max_pages: 1,
        ..BrowserPoolConfig::default()
    });
    let sessions = Arc::new(crawlberg::BrowserSessionPool::new());
    let mut settings = config();
    settings.browser.session_affinity = true;
    settings.browser_session_pool = Some(sessions.clone());
    settings.browser.block_url_patterns = vec!["*.png".into()];
    let blocked = CrawlEngineHandle::from_engine(
        CrawlEngine::builder()
            .config(settings.clone())
            .with_browser_pool(pool.clone())
            .build()
            .expect("blocked engine"),
    );
    scrape(&blocked, &format!("{}/blocked", site.uri()))
        .await
        .expect("blocked page");
    assert_eq!(sessions.size().await, 1, "first page must be retained");
    assert_eq!(paths_the_page_asked_for(&site).await, ["/blocked"]);
    settings.browser.block_url_patterns.clear();
    let cleared = CrawlEngineHandle::from_engine(
        CrawlEngine::builder()
            .config(settings)
            .with_browser_pool(pool.clone())
            .build()
            .expect("clear engine"),
    );
    let result = scrape(&cleared, &format!("{}/clear", site.uri()))
        .await
        .expect("clear page");
    sessions.shutdown().await;
    pool.shutdown().await;
    assert!(
        result.html.contains("data-retained=\"retained-block-pattern-page\""),
        "a fresh page would not prove reset: {}",
        result.html
    );
    let mut requested = paths_the_page_asked_for(&site).await;
    requested.sort_unstable();
    assert_eq!(requested, ["/blocked", "/clear", "/second.png"]);
}
