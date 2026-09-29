//! Load tests: many concurrent callers on one configuration, each seed behind its own redirecting server.
//!
//! Every call runs under a deadline, so a call that stalls fails the test by name instead of hanging it.

use std::future::Future;
use std::time::Duration;

use crawlberg::{CrawlConfig, CrawlEngineHandle, batch_crawl, create_engine, map_urls};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

/// Concurrent callers per test: the load that was reported to hang (crawlberg#406).
const CALLERS: usize = 40;

/// Deadline for each call. An unloaded call returns in well under a second.
const CALL_DEADLINE: Duration = Duration::from_secs(30);

/// Start one server per caller. Each redirects `/start` to a page of its own that links to `x.html`.
async fn redirecting_sites() -> Vec<MockServer> {
    let mut sites = Vec::with_capacity(CALLERS);
    for i in 0..CALLERS {
        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).append_header("location", format!("/dir{i}/page.html")))
            .mount(&site)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/dir{i}/page.html")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"<html><body><a href="x.html">next</a></body></html>"#)
                    .append_header("content-type", "text/html"),
            )
            .mount(&site)
            .await;
        sites.push(site);
    }
    sites
}

fn seeds(sites: &[MockServer]) -> Vec<String> {
    sites.iter().map(|site| format!("{}/start", site.uri())).collect()
}

/// Drop the servers with no cooperative budget limit.
///
/// ~keep wiremock 0.6.5 `MockServer::drop` runs `futures::executor::block_on` over a tokio
/// ~keep `RwLock` read, and each read spends tokio budget. Once the task's budget is spent
/// ~keep the read returns `Pending` and wakes itself at once, so the nested `block_on` spins
/// ~keep at full CPU and never returns. On a multi-thread runtime, dropping these servers in
/// ~keep the same poll that finished the concurrent calls reaches that state.
async fn drop_sites(sites: Vec<MockServer>) {
    tokio::task::unconstrained(async move { drop(sites) }).await;
}

fn engine() -> CrawlEngineHandle {
    let config = CrawlConfig {
        max_depth: Some(0),
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    create_engine(Some(config)).expect("the engine builds")
}

/// Run every call at once, each under [`CALL_DEADLINE`], and panic naming each call that did not return.
async fn all_within_deadline<T, F>(calls: Vec<F>) -> Vec<T>
where
    F: Future<Output = T>,
{
    let outcomes =
        futures::future::join_all(calls.into_iter().map(|call| tokio::time::timeout(CALL_DEADLINE, call))).await;
    let stalled: Vec<usize> = outcomes
        .iter()
        .enumerate()
        .filter_map(|(index, outcome)| outcome.is_err().then_some(index))
        .collect();
    assert!(
        stalled.is_empty(),
        "{} of {} concurrent calls did not return within {CALL_DEADLINE:?}: calls {stalled:?}",
        stalled.len(),
        outcomes.len()
    );
    outcomes
        .into_iter()
        .map(|outcome| outcome.expect("checked above"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_maps_on_one_engine_all_return() {
    let sites = redirecting_sites().await;
    let engine = engine();

    let results = all_within_deadline(seeds(&sites).iter().map(|seed| map_urls(&engine, seed)).collect()).await;

    for ((site, seed), result) in sites.iter().zip(seeds(&sites)).zip(results) {
        let urls: Vec<String> = result
            .unwrap_or_else(|error| panic!("map of {seed} failed: {error}"))
            .urls
            .into_iter()
            .map(|entry| entry.url)
            .collect();
        assert!(
            !urls.is_empty() && urls.iter().all(|url| url.starts_with(&site.uri())),
            "map of {seed} must return the page's link on its own server, got {urls:?}"
        );
    }
    drop_sites(sites).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batch_crawl_of_many_redirecting_seeds_returns_every_seed() {
    let sites = redirecting_sites().await;
    let engine = engine();

    let mut results = all_within_deadline(vec![batch_crawl(&engine, seeds(&sites))]).await;
    let results = results.pop().expect("one batch").expect("batch_crawl succeeds");

    assert_eq!(results.total_count, CALLERS);
    for result in &results.results {
        let crawl = result.result.as_ref().unwrap_or_else(|| {
            panic!(
                "{} failed: {}",
                result.url,
                result.error.as_deref().unwrap_or("no error recorded")
            )
        });
        assert!(
            !crawl.pages.is_empty(),
            "{} must return its redirected page",
            result.url
        );
    }
    drop_sites(sites).await;
}

#[cfg(all(feature = "api", feature = "mcp"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_mcp_map_calls_all_return() {
    use axum::http::StatusCode;
    use common::mcp::{json_rpc_frame, mcp_request};
    use tower::ServiceExt;

    let sites = redirecting_sites().await;
    let config = CrawlConfig::builder().allow_private_networks(true).build();
    let app = axum::Router::new().nest_service("/mcp", crawlberg::streamable_http_service(config));

    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    let init_response = app
        .clone()
        .oneshot(mcp_request(init.to_owned()))
        .await
        .expect("initialize responds");
    assert_eq!(init_response.status(), StatusCode::OK, "initialize must succeed");

    let calls = seeds(&sites)
        .into_iter()
        .enumerate()
        .map(|(id, seed)| {
            let app = app.clone();
            async move {
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id + 2,
                    "method": "tools/call",
                    "params": { "name": "map", "arguments": { "url": seed, "respect_robots_txt": false } },
                })
                .to_string();
                let response = app.oneshot(mcp_request(body)).await.expect("tools/call responds");
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body collects");
                (seed, json_rpc_frame(&String::from_utf8_lossy(&bytes)))
            }
        })
        .collect();
    let frames = all_within_deadline(calls).await;

    for (seed, frame) in frames {
        let result = &frame["result"];
        assert!(
            frame.get("error").is_none() && result["isError"] != serde_json::json!(true),
            "the map tool must succeed for {seed}: {frame}"
        );
        let urls = result["structuredContent"]["urls"]
            .as_array()
            .unwrap_or_else(|| panic!("the map tool must return a URL list for {seed}: {frame}"));
        assert!(
            !urls.is_empty(),
            "the map tool must find the page's link for {seed}: {frame}"
        );
    }
    drop_sites(sites).await;
}
