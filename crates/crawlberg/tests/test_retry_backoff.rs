//! Regression coverage for the retry/backoff path (crawlberg#67, crawlberg#68).
//!
//! ~keep Before these tests, `retry_count`/`retry_codes` had zero behavioural coverage
//! ~keep anywhere in this repo. Two independent retry loops — the tower-service inner loop
//! ~keep (`tower::service::HttpFetchService::call`) and the dispatch loop's
//! ~keep `SimpleRetryPolicy` (`defaults::dispatch`, built with a hardcoded 3 retries regardless
//! ~keep of `CrawlConfig::retry_count`) — multiplied together: `retry_count=4` produced 20
//! ~keep requests instead of 5. Confirmed by temporarily reverting the fix and re-running
//! ~keep `retry_count_*_sends_exactly_*_requests` below, which then observed 4, 8 and 20
//! ~keep requests for `retry_count` 0, 1 and 4 — matching the bug report exactly.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crawlberg::{BrowserConfig, BrowserMode, CrawlConfig, crawl, create_engine, map_urls, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable, and whose browser tier never engages — retries are an
/// HTTP-tier concern here, and a browser fallback would confuse the request count.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read in this binary's other tests.
fn base_config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// Always answers with a fixed status after a fixed delay, recording the wall-clock instant
/// of every request it receives so tests can assert both the request count and the gaps
/// between attempts.
struct RecordingResponder {
    timestamps: Arc<Mutex<Vec<Instant>>>,
    status: u16,
    delay: Duration,
}

impl Respond for RecordingResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.timestamps.lock().expect("lock poisoned").push(Instant::now());
        ResponseTemplate::new(self.status).set_delay(self.delay)
    }
}

/// Mounts a mock that always answers `status` after `delay`, drives one `scrape` call against
/// it, and returns the timestamps of every request the mock actually received.
async fn scrape_against(status: u16, delay: Duration, mock_path: &str, config: CrawlConfig) -> Vec<Instant> {
    let mock = MockServer::start().await;
    let timestamps = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("GET"))
        .and(path(mock_path))
        .respond_with(RecordingResponder {
            timestamps: Arc::clone(&timestamps),
            status,
            delay,
        })
        .mount(&mock)
        .await;

    let handle = create_engine(Some(config)).expect("engine build must not fail");
    let result = scrape(&handle, &format!("{}{mock_path}", mock.uri())).await;
    assert!(
        result.is_err(),
        "a persistently failing fetch must not be swallowed as Ok, got {result:?}"
    );

    timestamps.lock().expect("lock poisoned").clone()
}

/// Mounts a mock that always answers 503 at once. See [`scrape_against`].
async fn scrape_against_always_503(mock_path: &str, config: CrawlConfig) -> Vec<Instant> {
    scrape_against(503, Duration::ZERO, mock_path, config).await
}

/// A config that fails fast: a 1ms initial delay keeps these tests from being slow while
/// still exercising real `tokio::time::sleep` backoff between attempts.
fn fast_retry_config(retry_count: usize) -> CrawlConfig {
    CrawlConfig {
        retry_count,
        retry_initial_delay_ms: 1,
        retry_max_delay_ms: 50,
        ..base_config()
    }
}

// --- crawlberg#68: retry_count must be honoured exactly once, not multiplied ---

#[tokio::test]
async fn retry_count_zero_sends_exactly_one_request() {
    let timestamps = scrape_against_always_503("/retry-zero", fast_retry_config(0)).await;
    assert_eq!(
        timestamps.len(),
        1,
        "retry_count=0 must send exactly 1 request, got {}",
        timestamps.len()
    );
}

#[tokio::test]
async fn retry_count_one_sends_exactly_two_requests() {
    let timestamps = scrape_against_always_503("/retry-one", fast_retry_config(1)).await;
    assert_eq!(
        timestamps.len(),
        2,
        "retry_count=1 must send exactly 2 requests, got {}",
        timestamps.len()
    );
}

#[tokio::test]
async fn retry_count_four_sends_exactly_five_requests() {
    let timestamps = scrape_against_always_503("/retry-four", fast_retry_config(4)).await;
    assert_eq!(
        timestamps.len(),
        5,
        "retry_count=4 must send exactly 5 requests, got {} (the pre-fix bug produced 20: \
         two nested retry loops multiplied instead of composing)",
        timestamps.len()
    );
}

// --- crawlberg#76: a non-empty retry_codes is an allowlist for error retries too ---

/// `fast_retry_config(3)` with `retry_codes` set and a request timeout short enough for a
/// slow response to time out.
fn allowlist_config(retry_codes: Vec<u16>) -> CrawlConfig {
    CrawlConfig {
        retry_codes,
        request_timeout: Duration::from_millis(100),
        ..fast_retry_config(3)
    }
}

#[tokio::test]
async fn retry_codes_stop_a_server_error_whose_status_is_not_listed() {
    let timestamps = scrape_against(500, Duration::ZERO, "/unlisted-500", allowlist_config(vec![429, 503])).await;
    assert_eq!(
        timestamps.len(),
        1,
        "a 500 is not in retry_codes [429, 503], so it must be requested once, got {}",
        timestamps.len()
    );
}

/// ~keep A regression guard, not evidence for #76: this sent 4 requests before the allowlist
/// existed too. It guards against the allowlist over-blocking a status `retry_codes` does list.
#[tokio::test]
async fn retry_codes_still_retry_a_listed_status() {
    let timestamps = scrape_against(503, Duration::ZERO, "/listed-503", allowlist_config(vec![429, 503])).await;
    assert_eq!(
        timestamps.len(),
        4,
        "a 503 is in retry_codes, so retry_count=3 must send 4 requests, got {}",
        timestamps.len()
    );
}

#[tokio::test]
async fn retry_codes_stop_a_timeout() {
    let slow = Duration::from_millis(500);
    let timestamps = scrape_against(200, slow, "/timeout-listed", allowlist_config(vec![429, 503])).await;
    assert_eq!(
        timestamps.len(),
        1,
        "a timeout carries no status, so a non-empty retry_codes must not retry it, got {}",
        timestamps.len()
    );
}

/// ~keep A regression guard, not evidence for #76: this sent 4 requests before the allowlist
/// existed too. It guards against an empty `retry_codes` being read as a list that admits nothing.
#[tokio::test]
async fn empty_retry_codes_still_retry_a_timeout() {
    let slow = Duration::from_millis(500);
    let timestamps = scrape_against(200, slow, "/timeout-empty", allowlist_config(Vec::new())).await;
    assert_eq!(
        timestamps.len(),
        4,
        "an empty retry_codes retries every retryable error, so retry_count=3 must send 4 \
         requests, got {}",
        timestamps.len()
    );
}

/// ~keep Only the timeout half is evidence for #92. The 408-response half sent 4 requests before
/// the allowlist existed too, and is a guard against over-blocking a listed status.
#[tokio::test]
async fn retry_codes_listing_408_retry_a_408_response_but_not_a_timeout() {
    let slow = Duration::from_millis(500);
    let timed_out = scrape_against(200, slow, "/timeout-408", allowlist_config(vec![408])).await;
    assert_eq!(
        timed_out.len(),
        1,
        "a timeout without a response is not a 408, so retry_codes [408] must not retry it, got {}",
        timed_out.len()
    );
    let answered = scrape_against(408, Duration::ZERO, "/answered-408", allowlist_config(vec![408])).await;
    assert_eq!(
        answered.len(),
        4,
        "a 408 response is listed, so retry_count=3 must send 4 requests, got {}",
        answered.len()
    );
}

/// A 504 must be the same failure on every path: an empty `retry_codes` retries it for
/// `scrape`, `crawl` and `map_urls` alike.
#[tokio::test]
async fn empty_retry_codes_retry_a_504_on_scrape_crawl_and_map() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gateway"))
        .respond_with(ResponseTemplate::new(504))
        .mount(&mock)
        .await;
    let config = CrawlConfig {
        respect_robots_txt: false,
        max_depth: Some(0),
        ..fast_retry_config(2)
    };
    let handle = create_engine(Some(config)).expect("engine build must not fail");
    let url = format!("{}/gateway", mock.uri());
    let requests = || async {
        let received = mock.received_requests().await.expect("request recording is on");
        received
            .iter()
            .filter(|request| request.url.path() == "/gateway")
            .count()
    };

    let scraped = scrape(&handle, &url).await;
    assert!(scraped.is_err(), "a 504 must fail the scrape, got {scraped:?}");
    assert_eq!(requests().await, 3, "scrape: retry_count=2 must send 3 requests");

    let _ = crawl(&handle, &url).await;
    assert_eq!(requests().await, 6, "crawl: retry_count=2 must send 3 more requests");

    let _ = map_urls(&handle, &url).await;
    assert_eq!(requests().await, 9, "map_urls: retry_count=2 must send 3 more requests");
}

// --- crawlberg#67: one converged, configurable backoff ---

#[tokio::test]
async fn large_retry_count_keeps_every_gap_at_or_below_the_configured_ceiling() {
    let ceiling = Duration::from_millis(20);
    let config = CrawlConfig {
        retry_count: 10,
        retry_initial_delay_ms: 5,
        retry_max_delay_ms: u64::try_from(ceiling.as_millis()).expect("fits u64"),
        ..base_config()
    };

    let timestamps = scrape_against_always_503("/ceiling", config).await;
    assert_eq!(
        timestamps.len(),
        11,
        "retry_count=10 must send exactly 11 requests, got {}",
        timestamps.len()
    );

    // ~keep A generous tolerance above the ceiling absorbs scheduler jitter under CI load;
    // ~keep this check exists to catch an uncapped or overflowing backoff (crawlberg#67), not
    // ~keep to assert microsecond-exact timer precision.
    let tolerance = Duration::from_millis(300);
    for pair in timestamps.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert!(
            gap <= ceiling + tolerance,
            "gap {gap:?} between attempts must stay near the configured {ceiling:?} ceiling"
        );
    }
}

/// ~keep A paused tokio clock (`#[tokio::test(start_paused = true)]`) was tried here first and
/// ~keep rejected: it makes `wiremock`'s real TCP round trip never complete (0 requests
/// ~keep observed), because reqwest's own internal timers race the paused clock's auto-advance
/// ~keep and fire before the real I/O is ready. So this stays wall-clock, but with a configured
/// ~keep delay (2s) an order of magnitude above the hardcoded 100ms default this test exists to
/// ~keep catch: the previous 150ms/50ms-margin version passed green on 3/3 runs even with the
/// ~keep config read cut, because wiremock+reqwest overhead alone bridged that 50ms gap. A
/// ~keep 1900ms lower-bound margin cannot be bridged by request overhead on any machine this
/// ~keep suite runs on.
#[tokio::test]
async fn configured_initial_delay_is_honoured_for_the_first_retry() {
    let initial_delay = Duration::from_millis(2000);
    let config = CrawlConfig {
        retry_count: 1,
        retry_initial_delay_ms: u64::try_from(initial_delay.as_millis()).expect("fits u64"),
        retry_max_delay_ms: 60_000,
        ..base_config()
    };

    let timestamps = scrape_against_always_503("/initial-delay", config).await;
    assert_eq!(
        timestamps.len(),
        2,
        "retry_count=1 must send exactly 2 requests, got {}",
        timestamps.len()
    );

    let gap = timestamps[1].duration_since(timestamps[0]);
    assert!(
        gap >= Duration::from_millis(1900),
        "the gap before the first retry ({gap:?}) must be at least the configured initial \
         delay ({initial_delay:?}), less a 100ms tolerance; a hardcoded 100ms default would \
         show up here as a gap far below this bound regardless of request overhead"
    );
    assert!(
        gap < Duration::from_millis(4000),
        "the gap ({gap:?}) is far larger than the configured initial delay ({initial_delay:?}); \
         backoff appears to be ignoring retry_initial_delay_ms"
    );
}

// --- validate_retry_count ---

#[test]
fn validate_rejects_an_absurd_retry_count_at_engine_creation() {
    let config = CrawlConfig {
        retry_count: 1_000_000,
        ..CrawlConfig::default()
    };
    // ~keep Not `.expect_err(...)`: `CrawlEngineHandle` (the `Ok` type) does not implement
    // ~keep `Debug`, and `Result::expect_err` requires it to format the unexpected `Ok` case.
    let error = match create_engine(Some(config)) {
        Err(error) => error,
        Ok(_) => panic!("an absurd retry_count must be rejected"),
    };
    let message = error.to_string();
    assert!(
        message.contains("retry_count"),
        "expected a retry_count validation error, got: {message}"
    );
}
