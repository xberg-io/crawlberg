use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn should_suspend_redirects_between_hops_preserving_cookie_headers_and_owned_claims() {
    let site = MockServer::start().await;
    Mock::given(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/alias")
                .insert_header("set-cookie", "session=cursor; Path=/"),
        )
        .mount(&site)
        .await;
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    Mock::given(path("/alias"))
        .respond_with(move |request: &wiremock::Request| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(302)
                    .insert_header("location", "/alias#top")
                    .insert_header("x-hop", "middle")
            } else {
                let cookie = request
                    .headers
                    .get("cookie")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("");
                ResponseTemplate::new(200).set_body_raw(format!("<p>{cookie}</p>"), "text/html")
            }
        })
        .mount(&site)
        .await;
    let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
    config.cookies_enabled = true;
    config.respect_robots_txt = false;
    config.rate_limit_ms = Some(0);
    let engine = CrawlEngine::builder().config(config).build().unwrap();
    let client = crate::http::build_client(&engine.config).unwrap();
    let initial = format!("{}/start?raw=%2f", site.uri());
    let mut policy = RedirectPolicy::new(&engine, &client, &[], &[]);
    let cursor = RedirectCursor::new(&initial, 5);
    assert_eq!(cursor.url(), initial);
    let cursor = match cursor.step(&engine, Some(&mut policy), None, Hop::Fetch).await.unwrap() {
        RedirectStep::Continue(cursor) => cursor,
        RedirectStep::Done(_) => panic!("a single step must stop before the next host turn"),
    };
    assert_eq!(site.received_requests().await.unwrap().len(), 1);
    let state = policy.into_state();
    let mut policy = RedirectPolicy::from_state(&engine, &client, &[], &[], state);
    let cursor = match cursor.step(&engine, Some(&mut policy), None, Hop::Fetch).await.unwrap() {
        RedirectStep::Continue(cursor) => cursor,
        RedirectStep::Done(_) => panic!("second hop must suspend before the self-owned alias"),
    };
    assert_eq!(site.received_requests().await.unwrap().len(), 2);
    let state = policy.into_state();
    let mut policy = RedirectPolicy::from_state(&engine, &client, &[], &[], state);
    let outcome = match cursor.step(&engine, Some(&mut policy), None, Hop::Fetch).await.unwrap() {
        RedirectStep::Done(RedirectResolution::Fetched(outcome)) => outcome,
        _ => panic!("the third hop must finish on its own previously claimed alias"),
    };
    assert_eq!(outcome.final_url, format!("{}/alias#top", site.uri()));
    assert_eq!(outcome.redirect_count, 2);
    assert_eq!(
        outcome.intermediate_headers[0].1["set-cookie"],
        ["session=cursor; Path=/"]
    );
    assert_eq!(outcome.intermediate_headers[1].1["x-hop"], ["middle"]);
    assert!(outcome.final_response.body.contains("session=cursor"));
    assert_eq!(site.received_requests().await.unwrap().len(), 3);
}
