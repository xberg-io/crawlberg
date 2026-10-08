//! Real-Chrome acceptance coverage for SSRF policy inheritance by browser child targets. ~keep

#![cfg(feature = "browser")]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::target::GetTargetsParams;
use crawlberg::{BrowserConfig, BrowserPool, BrowserPoolConfig, CrawlConfig, HostMatcher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html")
}

fn javascript(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/javascript")
}

async fn denied_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("denied"))
        .mount(&server)
        .await;
    server
}

fn denied_url(server: &MockServer) -> String {
    format!("http://127.0.0.1:{}/secret", server.address().port())
}

async fn cross_site_frame(body: String) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("[::1]:0")
        .await
        .expect("cross-site server must bind");
    let port = listener.local_addr().expect("bound server must have an address").port();
    let marker_count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&marker_count);
    let body = Arc::new(body);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let counted = Arc::clone(&counted);
            let body = Arc::clone(&body);
            tokio::spawn(async move {
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).await.unwrap_or(0);
                if request[..read].starts_with(b"GET /allowed?oopif=refused ") {
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://[::1]:{port}/frame"), marker_count)
}

async fn mount_script(site: &MockServer, route: &str, source: String) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(javascript(&source))
        .mount(site)
        .await;
}

fn config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            chrome_path: std::env::var_os("CRAWLBERG_TEST_CHROME_PATH").map(Into::into),
            chrome_args: vec!["--site-per-process".to_owned()],
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .ssrf_allowlist_host(HostMatcher::cidr("::1/128").expect("the OOPIF CIDR must be valid"))
            .build()
    }
}

/// ~keep Each descendant reports whether its denied `no-cors` fetch rejected: Fetch-domain
/// ~keep interception fails it with `BlockedByClient`, while the socket proxy's HTTP 403 is an
/// ~keep opaque successful response. The marker therefore proves interception, not only egress.
#[tokio::test(flavor = "multi_thread")]
async fn child_targets_should_inherit_the_page_ssrf_policy() {
    let test_name = "child_targets_should_inherit_the_page_ssrf_policy";
    let denied_server = denied_server().await;
    let denied = denied_url(&denied_server);
    let site = MockServer::start().await;

    mount_script(
        &site,
        "/shared.js",
        format!(
            "self.onconnect = async event => {{ const outcome = await fetch({denied:?}, {{mode: 'no-cors'}}).then(() => 'reached', () => 'refused'); await fetch('/allowed?shared-worker=' + outcome); event.ports[0].postMessage(outcome); }};"
        ),
    )
    .await;
    mount_script(
        &site,
        "/service.js",
        format!(
            "self.addEventListener('install', event => event.waitUntil((async () => {{ const outcome = await fetch({denied:?}, {{mode: 'no-cors'}}).then(() => 'reached', () => 'refused'); await fetch('/allowed?service-worker=' + outcome); await new Promise(resolve => setTimeout(resolve, 3000)); }})()));"
        ),
    )
    .await;
    mount_script(
        &site,
        "/outer.js",
        "new Worker('/inner.js'); setInterval(() => {}, 1000);".to_owned(),
    )
    .await;
    mount_script(
        &site,
        "/inner.js",
        format!(
            "(async () => {{ const outcome = await fetch({denied:?}, {{mode: 'no-cors'}}).then(() => 'reached', () => 'refused'); await fetch('/allowed?nested-worker=' + outcome); }})(); setInterval(() => {{}}, 1000);"
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/same-frame"))
        .respond_with(html(&format!(
            "<script>(async () => {{ const outcome = await fetch({denied:?}, {{mode: 'no-cors'}}).then(() => 'reached', () => 'refused'); await fetch('/allowed?same-origin-frame=' + outcome); }})();</script>"
        )))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/allowed"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&site)
        .await;

    let (cross_frame, oopif_markers) = cross_site_frame(format!(
        "<script>(async () => {{ const outcome = await fetch({denied:?}, {{mode: 'no-cors'}}).then(() => 'reached', () => 'refused'); await fetch('/allowed?oopif=' + outcome); }})();</script>"
    ))
    .await;
    let body = format!(
        "<iframe src='/same-frame'></iframe><iframe src={cross_frame:?}></iframe><script>const shared = new SharedWorker('/shared.js'); shared.port.start(); navigator.serviceWorker.register('/service.js'); new Worker('/outer.js');</script>"
    );
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html(&body))
        .mount(&site)
        .await;
    let seed = format!("http://localhost:{}/", site.address().port());

    let pool = BrowserPool::new(BrowserPoolConfig {
        max_pages: 1,
        chrome_path: std::env::var_os("CRAWLBERG_TEST_CHROME_PATH").map(Into::into),
        chrome_args: vec!["--site-per-process".to_owned()],
        ..BrowserPoolConfig::default()
    });
    let page = match pool.acquire_page_with_config(&config()).await {
        Ok(page) => page,
        Err(error) if is_missing_chrome_message(&error.to_string()) => {
            announce_chrome_skip(test_name, &error.to_string());
            pool.shutdown().await;
            return;
        }
        Err(error) => panic!("{test_name}: protected page acquisition must succeed: {error:?}"),
    };
    page.page()
        .goto(seed.as_str())
        .await
        .expect("the protected page must navigate to the seed");

    let mut observed_targets = BTreeSet::new();
    for _ in 0..80 {
        let targets = page
            .page()
            .execute(GetTargetsParams::default())
            .await
            .expect("the page connection must expose the browser target census")
            .result
            .target_infos;
        for target in targets {
            match (target.r#type.as_str(), target.url.as_str()) {
                ("shared_worker", url) if url.contains("/shared.js") => {
                    observed_targets.insert("shared-worker");
                }
                ("service_worker", url) if url.contains("/service.js") => {
                    observed_targets.insert("service-worker");
                }
                ("worker", url) if url.contains("/inner.js") => {
                    observed_targets.insert("nested-worker");
                }
                ("iframe", url) if url.starts_with("http://[::1]:") => {
                    observed_targets.insert("oopif");
                }
                _ => {}
            }
        }
        if observed_targets.len() == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        observed_targets,
        BTreeSet::from(["nested-worker", "oopif", "service-worker", "shared-worker"]),
        "{test_name}: Chrome must expose each descendant under its intended Target-domain type"
    );

    let expected_queries = [
        "shared-worker=refused",
        "service-worker=refused",
        "nested-worker=refused",
        "same-origin-frame=refused",
    ];
    let mut queries = Vec::new();
    for _ in 0..80 {
        queries = site
            .received_requests()
            .await
            .expect("request recording must be enabled")
            .iter()
            .filter_map(|request| request.url.query().map(str::to_owned))
            .collect();
        if expected_queries
            .iter()
            .all(|expected| queries.iter().any(|query| query == expected))
            && oopif_markers.load(Ordering::SeqCst) == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for marker in ["shared-worker", "service-worker", "nested-worker", "same-origin-frame"] {
        let expected = format!("{marker}=refused");
        assert_eq!(
            queries.iter().filter(|query| query.as_str() == expected).count(),
            1,
            "{test_name}: {marker} must observe Fetch interception's BlockedByClient failure exactly once; observed queries: {queries:?}"
        );
    }
    assert_eq!(
        oopif_markers.load(Ordering::SeqCst),
        1,
        "{test_name}: the OOPIF must observe Fetch interception's BlockedByClient failure exactly once"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let denied_requests = denied_server
        .received_requests()
        .await
        .expect("denied request recording must be enabled");
    assert_eq!(
        denied_requests.len(),
        0,
        "{test_name}: denied delivery count must be exact"
    );
    page.close().await;
    pool.shutdown().await;
}
