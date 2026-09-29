//! Integration tests for BrowserBackend::Native via wiremock.
//!
//! The native HTTP client rejects RFC1918/loopback unless the crawl config's SSRF policy
//! permits private networks, which every config below opts into.

#![cfg(feature = "browser-native")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserWait, CrawlConfig, HostMatcher, batch_scrape, create_engine, scrape,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers and the loopback test server are reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read (`SsrfPolicy::from_env`, reached from
// `CrawlConfig::default()`) in this binary's other tests, aborting the process on glibc
// with no failing test name.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

fn native_config(extra: impl FnOnce(BrowserConfig) -> BrowserConfig) -> CrawlConfig {
    let browser = extra(BrowserConfig {
        backend: BrowserBackend::Native,
        mode: crawlberg::BrowserMode::Always,
        timeout: Duration::from_secs(15),
        ..BrowserConfig::default()
    });
    CrawlConfig {
        browser,
        ..allow_private_config()
    }
}

fn engine_with(config: CrawlConfig) -> crawlberg::CrawlEngineHandle {
    create_engine(Some(config)).expect("engine must build")
}

#[tokio::test]
async fn native_renders_simple_html() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Hello</h1></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let url = mock.uri();
    let result = scrape(&engine_with(native_config(|c| c)), &url).await;
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
    assert!(result.unwrap().html.contains("Hello"));
}

#[tokio::test]
async fn native_runs_a_module_script_loaded_from_a_src() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><script type=\"module\" src=\"app.js\"></script></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/app.js"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    "const p = document.createElement('p');\
                     p.setAttribute('id', 'from-module');\
                     p.textContent = 'module ran';\
                     document.body.appendChild(p);",
                )
                .append_header("content-type", "text/javascript"),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let result = scrape(&engine_with(native_config(|c| c)), &mock.uri())
        .await
        .expect("the scrape must succeed");
    assert!(
        result.html.contains("<p id=\"from-module\">module ran</p>"),
        "the rendered page must contain the element the module adds: {}",
        result.html
    );
}

#[tokio::test]
async fn native_follows_redirect() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/final")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/final"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Redirected</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let url = format!("{}/start", mock.uri());
    let result = scrape(&engine_with(native_config(|c| c)), &url).await;
    assert!(result.is_ok(), "should succeed after redirect: {:?}", result.err());
    let page = result.unwrap();
    assert!(page.html.contains("Redirected"), "final body expected");
    assert_eq!(
        page.final_url,
        format!("{}/final", mock.uri()),
        "the result must report the URL the redirect landed on"
    );
}

/// The native backend counts the one redirect of a one-hop chain, as HTTP mode does.
#[tokio::test]
async fn native_counts_a_landing_on_another_url_as_one_redirect() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(301).append_header("location", "/final"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/final"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Redirected</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        max_depth: Some(0),
        respect_robots_txt: false,
        ..native_config(|c| c)
    };
    let result = crawlberg::crawl(&engine_with(config), &format!("{}/", mock.uri()))
        .await
        .expect("the crawl must succeed");
    assert_eq!(
        (result.redirect_count, result.final_url.as_str()),
        (1, format!("{}/final", mock.uri()).as_str()),
        "error={:?}",
        result.error
    );
}

#[tokio::test]
async fn native_respects_timeout() {
    let url = "http://192.0.2.1:80/timeout-target";
    let config = native_config(|mut c| {
        c.timeout = Duration::from_millis(500);
        c
    });
    let start = std::time::Instant::now();
    let result = scrape(&engine_with(config), url).await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "should have timed out well before 5s, took {:?}",
        elapsed
    );
    assert!(result.is_err(), "should return an error on timeout/connection failure");
}

#[tokio::test]
async fn native_forwards_extra_headers() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("x-custom", "value"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>OK</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let url = mock.uri();
    let config = {
        let mut headers = std::collections::HashMap::new();
        headers.insert("x-custom".to_string(), "value".to_string());
        CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Native,
                mode: crawlberg::BrowserMode::Always,
                timeout: Duration::from_secs(15),
                ..BrowserConfig::default()
            },
            custom_headers: headers,
            ..allow_private_config()
        }
    };
    let result = scrape(&engine_with(config), &url).await;
    assert!(result.is_ok(), "should succeed with custom header: {:?}", result.err());
}

#[tokio::test]
async fn native_sends_custom_headers_to_the_seed_host_only() {
    let seed = MockServer::start().await;
    let other = MockServer::start().await;
    let page = format!(
        r#"<html><body><p>seed</p><script src="/own.js"></script><script src="http://localhost:{}/third.js"></script></body></html>"#,
        other.address().port()
    );
    for (mock, route, body, content_type) in [
        (&seed, "/", page.as_str(), "text/html"),
        (&seed, "/own.js", "globalThis.own = 1;", "text/javascript"),
        (&other, "/third.js", "globalThis.third = 1;", "text/javascript"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_owned(), content_type))
            .mount(mock)
            .await;
    }
    let config = CrawlConfig {
        custom_headers: std::collections::HashMap::from([("x-canary-header".to_owned(), "custom-canary".to_owned())]),
        ..native_config(|browser| browser)
    };

    scrape(&engine_with(config), &format!("{}/", seed.uri()))
        .await
        .expect("scrape must succeed");

    let custom_header = |request: &wiremock::Request| {
        request
            .headers
            .get("x-canary-header")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let seed_requests = seed.received_requests().await.expect("request recording is on");
    for route in ["/", "/own.js"] {
        let request = seed_requests
            .iter()
            .find(|request| request.url.path() == route)
            .unwrap_or_else(|| panic!("{route} on the seed host must have been requested"));
        assert_eq!(
            custom_header(request).as_deref(),
            Some("custom-canary"),
            "{route} on the seed host carries the custom header"
        );
    }
    let other_requests = other.received_requests().await.expect("request recording is on");
    let third = other_requests
        .iter()
        .find(|request| request.url.path() == "/third.js")
        .expect("the third-party script must have been requested");
    assert_eq!(
        custom_header(third),
        None,
        "a third-party request never gets the custom header"
    );
}

#[tokio::test]
async fn native_errors_on_connection_refused() {
    let url = "http://127.0.0.1:1/unreachable";
    let result = scrape(&engine_with(native_config(|c| c)), url).await;
    assert!(result.is_err(), "should return error, not panic");
}

#[tokio::test]
async fn native_block_url_patterns_blocks_match() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><head><script src="/track.js"></script></head><body>Page</body></html>"#)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/track.js"))
        .respond_with(ResponseTemplate::new(200).set_body_string("// tracker"))
        .expect(0)
        .mount(&mock)
        .await;

    let url = mock.uri();
    let config = native_config(|mut c| {
        c.block_url_patterns = vec!["*track*".to_string()];
        c
    });
    let result = scrape(&engine_with(config), &url).await;
    assert!(result.is_ok(), "page should still render: {:?}", result.err());
}

#[tokio::test]
async fn native_eval_script_returns_value() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><head><title>Example</title></head><body></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let url = mock.uri();
    let config = native_config(|mut c| {
        c.eval_script = Some("document.title".to_string());
        c
    });
    let result = scrape(&engine_with(config), &url).await;
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
    let page = result.unwrap();
    let browser = page.browser.expect("browser extras must be present");
    let eval = browser.eval_result.expect("eval_result must be set");
    assert_eq!(eval.as_str(), Some("Example"), "eval result should be page title");
}

#[tokio::test]
async fn native_capture_network_events_includes_document() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Events</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let url = mock.uri();
    let config = native_config(|mut c| {
        c.capture_network_events = true;
        c
    });
    let result = scrape(&engine_with(config), &url).await;
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
    let page = result.unwrap();
    let browser = page.browser.expect("browser extras must be present");
    assert!(
        !browser.network_events.is_empty(),
        "at least the Document event should be captured"
    );
}

#[tokio::test]
async fn native_prior_cookies_sent_on_request() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("cookie", "session=abc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Authenticated</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let url = mock.uri();
    let config = {
        let mut headers = std::collections::HashMap::new();
        headers.insert("cookie".to_string(), "session=abc".to_string());
        CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Native,
                mode: crawlberg::BrowserMode::Always,
                timeout: Duration::from_secs(15),
                ..BrowserConfig::default()
            },
            custom_headers: headers,
            ..allow_private_config()
        }
    };
    let result = scrape(&engine_with(config), &url).await;
    assert!(result.is_ok(), "should succeed with cookie: {:?}", result.err());
}

#[tokio::test]
async fn native_post_render_cookies_capture_set_cookie() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Cookie test</body></html>")
                .append_header("content-type", "text/html")
                .append_header("set-cookie", "tracker=xyz; Path=/"),
        )
        .mount(&mock)
        .await;

    let url = mock.uri();
    let config = native_config(|mut c| {
        c.capture_network_events = true;
        c
    });
    let result = scrape(&engine_with(config), &url).await;
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
    let page = result.unwrap();
    let browser = page.browser.expect("browser extras must be present");
    assert!(
        browser.cookies.iter().any(|c| c.name == "tracker" && c.value == "xyz"),
        "tracker=xyz cookie should be captured; got: {:?}",
        browser.cookies
    );
}

#[tokio::test]
async fn native_wait_selector_succeeds() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body><div id="ready">Ready</div></body></html>"#)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let url = mock.uri();
    let config = native_config(|mut c| {
        c.wait = BrowserWait::Selector;
        c.wait_selector = Some("#ready".to_string());
        c
    });
    let result = scrape(&engine_with(config), &url).await;
    assert!(result.is_ok(), "wait_selector should succeed: {:?}", result.err());
}

#[tokio::test]
async fn native_batch_scrape_uses_shared_executor_concurrently() {
    let server = TestServer::start().await;
    let config = native_config(|mut browser| {
        browser.timeout = Duration::from_secs(15);
        browser
    });
    let config = CrawlConfig {
        max_concurrent: Some(4),
        ..config
    };
    let engine = engine_with(config);

    let urls = (0..12)
        .map(|index| format!("{}/page-{index}", server.base_url))
        .collect::<Vec<_>>();
    let results = batch_scrape(&engine, urls).await.expect("batch scrape should run");

    assert_eq!(results.total_count, 12);
    for result in results.results {
        let page = result.result.expect("native scrape should succeed");
        assert!(page.html.contains("Native executor"));
        assert!(page.html.contains("data-rendered=\"true\""));
    }
    assert!(
        server.max_in_flight.load(Ordering::SeqCst) >= 2,
        "server should observe parallel native requests"
    );
}

struct TestServer {
    base_url: String,
    max_in_flight: Arc<AtomicUsize>,
}

impl TestServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
        let addr = listener.local_addr().expect("test server should have local addr");
        let current = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let current_for_task = current.clone();
        let max_for_task = max_in_flight.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let current = current_for_task.clone();
                let max_in_flight = max_for_task.clone();
                tokio::spawn(async move {
                    let active = current.fetch_add(1, Ordering::SeqCst) + 1;
                    max_in_flight.fetch_max(active, Ordering::SeqCst);

                    let mut buffer = [0_u8; 1024];
                    let _ = stream.read(&mut buffer).await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let body = r#"
                        <html>
                          <body>
                            <div id="status">Native executor</div>
                            <script>
                              document.body.setAttribute('data-rendered', 'true');
                            </script>
                          </body>
                        </html>
                    "#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                    current.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            max_in_flight,
        }
    }
}

/// A script and a `fetch()` at an address the policy denies keep the page, and the result
/// lists both addresses. The page is served on `localhost`, which the policy allowlists; the
/// denied address is the literal loopback IP of a second server.
#[tokio::test]
async fn native_keeps_the_page_and_lists_the_refused_requests() {
    let site = MockServer::start().await;
    let denied = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("// denied"))
        .mount(&denied)
        .await;
    let script = format!("http://127.0.0.1:{}/denied.js", denied.address().port());
    let fetched = format!("http://127.0.0.1:{}/secret", denied.address().port());
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!(
                    "<html><head><script src={script:?}></script></head><body><p>start</p>\
                     <script>fetch({fetched:?}).catch(() => {{}});</script></body></html>"
                ))
                .append_header("content-type", "text/html"),
        )
        .mount(&site)
        .await;
    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: crawlberg::BrowserMode::Always,
            timeout: Duration::from_secs(15),
            extra_wait: Some(Duration::from_millis(500)),
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        max_depth: Some(0),
        ..CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .build()
    };
    let seed = format!("http://localhost:{}/", site.address().port());
    let engine = engine_with(config);
    let mut expected = vec![script, fetched];
    expected.sort();
    let result = scrape(&engine, &seed).await.expect("the page must be kept");
    assert!(result.html.contains("start"), "the page must be kept: {}", result.html);
    let mut listed = result.ssrf_refused_urls.clone();
    listed.sort();
    assert_eq!(listed, expected, "the scrape result must list every refused address");
    let crawled = crawlberg::crawl(&engine, &seed)
        .await
        .expect("the crawl must keep the page");
    let page = crawled.pages.first().expect("the crawl must return the page");
    let mut listed = page.ssrf_refused_urls.clone();
    listed.sort();
    assert_eq!(
        listed, expected,
        "the crawl page result must list every refused address"
    );
    let received = denied.received_requests().await.expect("request recording is on");
    assert!(
        received.is_empty(),
        "the denied address must receive nothing: {received:?}"
    );
}

/// `/` answers 301 to `/r1`, `/r1` to `/r2`, and so on for `hops` redirects; the last hop
/// lands on a 200 page.
async fn native_redirect_chain(hops: usize) -> MockServer {
    let mock = MockServer::start().await;
    for hop in 0..hops {
        let from = if hop == 0 { "/".to_owned() } else { format!("/r{hop}") };
        Mock::given(method("GET"))
            .and(path(from))
            .respond_with(ResponseTemplate::new(301).append_header("location", format!("/r{}", hop + 1)))
            .mount(&mock)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/r{hops}")))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>landed</body></html>", "text/html"))
        .mount(&mock)
        .await;
    mock
}

async fn native_requested_paths(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

/// A crawl of `site` at `max_redirects` with `mode`, as (redirect count, final path, status).
async fn native_chain_outcome(
    site: &MockServer,
    mode: crawlberg::BrowserMode,
    max_redirects: usize,
) -> (usize, String, u16) {
    let config = CrawlConfig {
        max_depth: Some(0),
        max_redirects,
        respect_robots_txt: false,
        ..native_config(|c| BrowserConfig { mode, ..c })
    };
    let result = crawlberg::crawl(&engine_with(config), &format!("{}/", site.uri()))
        .await
        .expect("the crawl must succeed");
    let status = result.pages.first().map_or(0, |page| page.status_code);
    (
        result.redirect_count,
        result.final_url.trim_start_matches(&site.uri()).to_owned(),
        status,
    )
}

/// The native backend stops a chain longer than `max_redirects` where HTTP mode stops it, in
/// crawl and in scrape (#115).
#[tokio::test]
async fn native_stops_a_chain_longer_than_max_redirects_where_http_mode_does() {
    let http = native_chain_outcome(&native_redirect_chain(5).await, crawlberg::BrowserMode::Never, 2).await;
    assert_eq!(
        http,
        (2, "/r2".to_owned(), 301),
        "HTTP mode stops on the 3xx at the limit"
    );

    let site = native_redirect_chain(5).await;
    assert_eq!(
        native_chain_outcome(&site, crawlberg::BrowserMode::Always, 2).await,
        http,
        "the native backend must report the chain the way HTTP mode does"
    );
    let requested = native_requested_paths(&site).await;
    assert!(
        !requested.iter().any(|p| ["/r3", "/r4", "/r5"].contains(&p.as_str())),
        "the native backend must not request past the limit, requested: {requested:?}"
    );

    let site = native_redirect_chain(5).await;
    let config = CrawlConfig {
        max_redirects: 2,
        respect_robots_txt: false,
        ..native_config(|c| c)
    };
    let scraped = scrape(&engine_with(config), &format!("{}/", site.uri()))
        .await
        .expect("the scrape must succeed");
    assert_eq!(
        (scraped.status_code, scraped.final_url.trim_start_matches(&site.uri())),
        (301, "/r2"),
        "the scrape must stop on the 3xx at the limit"
    );
    let requested = native_requested_paths(&site).await;
    assert!(
        !requested.iter().any(|p| p == "/r3"),
        "the scrape must not request past the limit, requested: {requested:?}"
    );
}

/// A navigation the page's script starts counts as one redirect in the native backend, as it
/// does in Chrome: past the limit the page stays where it is (#115).
#[tokio::test]
async fn native_counts_a_script_navigation_against_max_redirects() {
    for (max_redirects, expected) in [(0, (0, "/".to_owned(), 200)), (1, (1, "/after".to_owned(), 200))] {
        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<html><body><p>start</p><script>location.replace('/after')</script></body></html>",
                "text/html",
            ))
            .mount(&site)
            .await;
        Mock::given(method("GET"))
            .and(path("/after"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>after</body></html>", "text/html"))
            .mount(&site)
            .await;
        assert_eq!(
            native_chain_outcome(&site, crawlberg::BrowserMode::Always, max_redirects).await,
            expected,
            "max_redirects={max_redirects}"
        );
        if max_redirects == 0 {
            let requested = native_requested_paths(&site).await;
            assert!(
                !requested.iter().any(|p| p == "/after"),
                "the native backend must not follow the script past the limit, requested: {requested:?}"
            );
        }
    }
}

/// A native scrape follows a meta refresh within `max_redirects` and ends where HTTP mode ends:
/// on the refresh target within the limit, on the refresh page past it (#530).
#[tokio::test]
async fn native_scrape_follows_a_meta_refresh_where_http_mode_does() {
    for (max_redirects, expected) in [(0, (200, "/".to_owned())), (1, (200, "/n".to_owned()))] {
        for mode in [crawlberg::BrowserMode::Never, crawlberg::BrowserMode::Always] {
            let site = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/"))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    r#"<html><head><meta http-equiv="refresh" content="0; url=/n"></head><body>refresh</body></html>"#,
                    "text/html",
                ))
                .mount(&site)
                .await;
            Mock::given(method("GET"))
                .and(path("/n"))
                .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>landed</body></html>", "text/html"))
                .mount(&site)
                .await;
            let config = CrawlConfig {
                max_redirects,
                respect_robots_txt: false,
                ..native_config(|c| BrowserConfig {
                    mode: mode.clone(),
                    ..c
                })
            };
            let scraped = scrape(&engine_with(config), &format!("{}/", site.uri()))
                .await
                .expect("the scrape must succeed");
            assert_eq!(
                (
                    scraped.status_code,
                    scraped.final_url.trim_start_matches(&site.uri()).to_owned()
                ),
                expected,
                "{mode:?} at max_redirects={max_redirects}: the scrape must end where HTTP mode ends"
            );
        }
    }
}

/// A native scrape through a meta refresh keeps the cookies the refresh page set: the request to
/// the refresh target carries them, the result lists them, and an HttpOnly cookie keeps its flag
/// on the next hop. A Secure cookie that the page set over plain http is not stored.
#[tokio::test]
async fn native_scrape_through_a_meta_refresh_keeps_the_refresh_page_cookies() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(
                    r#"<html><head><meta http-equiv="refresh" content="0; url=/n"></head><body>refresh</body></html>"#,
                    "text/html",
                )
                .append_header("set-cookie", "first=1; Path=/")
                .append_header("set-cookie", "hidden=h; Path=/; HttpOnly")
                .append_header("set-cookie", "sec=s; Path=/; Secure"),
        )
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/n"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<html><body>landed</body></html>", "text/html")
                .append_header("set-cookie", "second=2; Path=/"),
        )
        .mount(&site)
        .await;
    let config = CrawlConfig {
        max_redirects: 1,
        respect_robots_txt: false,
        ..native_config(|c| BrowserConfig {
            eval_script: Some("document.cookie".to_owned()),
            ..c
        })
    };

    let scraped = scrape(&engine_with(config), &format!("{}/", site.uri()))
        .await
        .expect("the scrape must succeed");

    assert_eq!(scraped.final_url.trim_start_matches(&site.uri()), "/n");
    let target_request = site
        .received_requests()
        .await
        .expect("request recording is on")
        .into_iter()
        .find(|r| r.url.path() == "/n")
        .expect("the scrape must request the refresh target");
    let sent: Vec<String> = target_request
        .headers
        .get_all("cookie")
        .iter()
        .flat_map(|v| v.to_str().unwrap_or_default().split("; ").map(str::to_owned))
        .collect();
    assert!(
        sent.iter().any(|c| c == "first=1") && sent.iter().any(|c| c == "hidden=h"),
        "the request to the refresh target must carry the refresh page's cookies, sent {sent:?}"
    );
    assert!(
        !sent.iter().any(|c| c.starts_with("sec=")),
        "a Secure cookie must not go out over plain http, sent {sent:?}"
    );
    let browser = scraped.browser.expect("a native render reports browser extras");
    let mut names: Vec<&str> = browser.cookies.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["first", "hidden", "second"],
        "the result must list every cookie of the scrape, and no Secure cookie set over http"
    );
    let script_cookies = browser
        .eval_result
        .as_ref()
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    assert!(
        script_cookies.contains("first=1") && script_cookies.contains("second=2"),
        "the target page's script must see the carried cookie and its own, saw {script_cookies:?}"
    );
    assert!(
        !script_cookies.contains("hidden="),
        "an HttpOnly cookie must stay hidden from script on the next hop, saw {script_cookies:?}"
    );
}

/// The names of the cookies that the request for `at` sent to `site`.
async fn cookies_sent_to(site: &MockServer, at: &str) -> Vec<String> {
    let request = site
        .received_requests()
        .await
        .expect("request recording is on")
        .into_iter()
        .find(|r| r.url.path() == at)
        .unwrap_or_else(|| panic!("the scrape must request {at}"));
    request
        .headers
        .get_all("cookie")
        .iter()
        .flat_map(|v| v.to_str().unwrap_or_default().split("; ").map(str::to_owned))
        .collect()
}

/// A page on `127.0.0.1` sets a cookie for `Domain=localhost` and sends the scrape to
/// `localhost/n` with the response that `first` builds for that target. The request to
/// `localhost` must not carry the cookie, because the host that set it is not in that domain
/// (RFC 6265 section 5.3).
async fn assert_a_foreign_domain_cookie_does_not_reach_the_next_host(first: impl FnOnce(&str) -> ResponseTemplate) {
    let site = MockServer::start().await;
    let port = site.address().port();
    let target = format!("http://localhost:{port}/n");
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            first(&target)
                .append_header("set-cookie", "own=1; Path=/")
                .append_header("set-cookie", "inj=1; Path=/; Domain=localhost"),
        )
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/n"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>landed</body></html>", "text/html"))
        .mount(&site)
        .await;
    let config = CrawlConfig {
        max_redirects: 1,
        respect_robots_txt: false,
        ..native_config(|c| c)
    };

    let scraped = scrape(&engine_with(config), &format!("http://127.0.0.1:{port}/"))
        .await
        .expect("the scrape must succeed");

    assert_eq!(scraped.final_url, target);
    let sent = cookies_sent_to(&site, "/n").await;
    assert!(
        !sent.iter().any(|c| c.starts_with("inj=")),
        "a cookie that 127.0.0.1 set for Domain=localhost must not reach localhost, sent {sent:?}"
    );
    let browser = scraped.browser.expect("a native render reports browser extras");
    let names: Vec<&str> = browser.cookies.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["own"], "the jar must not store the foreign-domain cookie");
}

#[tokio::test]
async fn native_scrape_refuses_a_domain_cookie_for_another_host_through_a_302() {
    assert_a_foreign_domain_cookie_does_not_reach_the_next_host(|target| {
        ResponseTemplate::new(302).append_header("location", target)
    })
    .await;
}

#[tokio::test]
async fn native_scrape_refuses_a_domain_cookie_for_another_host_through_a_meta_refresh() {
    assert_a_foreign_domain_cookie_does_not_reach_the_next_host(|target| {
        ResponseTemplate::new(200).set_body_raw(
            format!(r#"<html><head><meta http-equiv="refresh" content="0; url={target}"></head></html>"#),
            "text/html",
        )
    })
    .await;
}

/// A cookie that `a.localhost` sets without a `Domain` attribute is host-only: the carried jar
/// sends it back to `a.localhost` only, not to `b.a.localhost` after a meta refresh. The cookie
/// set with `Domain=a.localhost` goes to both (RFC 6265 section 5.3 step 6).
#[tokio::test]
async fn native_scrape_sends_a_host_only_cookie_to_its_own_host_only_across_a_meta_refresh() {
    let site = MockServer::start().await;
    let port = site.address().port();
    let target = format!("http://b.a.localhost:{port}/n");
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(
                    format!(r#"<html><head><meta http-equiv="refresh" content="0; url={target}"></head></html>"#),
                    "text/html",
                )
                .append_header("set-cookie", "host=1; Path=/")
                .append_header("set-cookie", "domain=1; Path=/; Domain=a.localhost"),
        )
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/n"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>landed</body></html>", "text/html"))
        .mount(&site)
        .await;
    let config = CrawlConfig {
        max_redirects: 1,
        respect_robots_txt: false,
        ..native_config(|c| c)
    };

    let scraped = scrape(&engine_with(config), &format!("http://a.localhost:{port}/"))
        .await
        .expect("the scrape must succeed");

    assert_eq!(scraped.final_url, target);
    let sent = cookies_sent_to(&site, "/n").await;
    assert_eq!(
        sent,
        ["domain=1"],
        "only the Domain=a.localhost cookie may reach b.a.localhost"
    );
}

/// A native scrape through a meta refresh lists the addresses the SSRF policy refused on every
/// hop: the refresh page's script and the landing page's `fetch()`.
#[tokio::test]
async fn native_scrape_through_a_meta_refresh_lists_the_refused_requests_of_every_hop() {
    let site = MockServer::start().await;
    let denied = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("// denied"))
        .mount(&denied)
        .await;
    let script = format!("http://127.0.0.1:{}/denied.js", denied.address().port());
    let fetched = format!("http://127.0.0.1:{}/secret", denied.address().port());
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                r#"<html><head><meta http-equiv="refresh" content="0; url=/n"><script src={script:?}></script></head><body>refresh</body></html>"#
            ),
            "text/html",
        ))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/n"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!("<html><body><p>landed</p><script>fetch({fetched:?}).catch(() => {{}});</script></body></html>"),
            "text/html",
        ))
        .mount(&site)
        .await;
    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: crawlberg::BrowserMode::Always,
            timeout: Duration::from_secs(15),
            extra_wait: Some(Duration::from_millis(500)),
            ..BrowserConfig::default()
        },
        max_redirects: 1,
        respect_robots_txt: false,
        ..CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .build()
    };
    let seed = format!("http://localhost:{}/", site.address().port());

    let result = scrape(&engine_with(config), &seed)
        .await
        .expect("the scrape must succeed");

    assert!(
        result.html.contains("landed"),
        "the scrape must land on /n: {}",
        result.html
    );
    let mut listed = result.ssrf_refused_urls.clone();
    listed.sort();
    let mut expected = vec![script, fetched];
    expected.sort();
    assert_eq!(
        listed, expected,
        "the result must list the refused addresses of both hops"
    );
}

/// A native scrape of a seed that answers 404 fails with `not_found` as HTTP mode does, also
/// when the seed has no trailing slash and the page's URL gains one (#529).
#[tokio::test]
async fn native_scrape_of_a_missing_seed_without_a_trailing_slash_is_not_found() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(404).set_body_raw("<html><body>missing</body></html>", "text/html"))
        .mount(&site)
        .await;
    let seed = site.uri();
    assert!(!seed.ends_with('/'), "the seed must have no trailing slash: {seed}");

    for mode in [crawlberg::BrowserMode::Never, crawlberg::BrowserMode::Always] {
        let config = CrawlConfig {
            respect_robots_txt: false,
            ..native_config(|c| BrowserConfig {
                mode: mode.clone(),
                ..c
            })
        };
        match scrape(&engine_with(config), &seed).await {
            Err(crawlberg::CrawlError::NotFound { .. }) => {}
            other => panic!("{mode:?}: a missing seed must fail with not_found, got {other:?}"),
        }
    }
}
