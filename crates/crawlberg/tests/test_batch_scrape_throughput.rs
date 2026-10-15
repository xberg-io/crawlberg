#![allow(clippy::print_stdout)]

use std::time::Instant;

use crawlberg::{BrowserMode, CrawlConfig, CrawlEngine, HostMatcher, ProxyConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual local throughput comparison with a preserved baseline binary"]
async fn measure_default_batch_scrape_for_forty_large_distinct_hosts() {
    measure("large40", 40, 927_000, 10).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual local throughput comparison with a preserved baseline binary"]
async fn measure_default_batch_scrape_for_two_thousand_distinct_hosts() {
    measure("small2000", 2_000, 128, 10).await;
}

fn html_body(bytes: usize) -> String {
    let mut body = "<html><body>".to_owned();
    let chunk = "<p>A meaningful benchmark sentence with words.</p>";
    while body.len() + chunk.len() + "</body></html>".len() <= bytes {
        body.push_str(chunk);
    }
    body.extend(std::iter::repeat_n(' ', bytes - body.len() - "</body></html>".len()));
    body.push_str("</body></html>");
    assert_eq!(body.len(), bytes);
    body
}

async fn measure(name: &str, hosts: usize, bytes: usize, runs: usize) {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bench"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html_body(bytes), "text/html"))
        .mount(&site)
        .await;
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.browser.mode = BrowserMode::Never;
    config.respect_robots_txt = false;
    config.ssrf.allowlist = vec![HostMatcher::suffix(".example.test")];
    config.proxy = Some(ProxyConfig {
        url: site.uri(),
        username: None,
        password: None,
    });
    assert_eq!(config.max_concurrent, None);
    assert_eq!(config.rate_limit_ms, None);
    let urls = (0..hosts)
        .map(|host| format!("http://host{host}.example.test/bench"))
        .collect::<Vec<_>>();
    let refs = urls.iter().map(String::as_str).collect::<Vec<_>>();
    let mut samples = Vec::new();
    for run in 0..runs {
        let engine = CrawlEngine::builder().config(config.clone()).build().unwrap();
        let began = Instant::now();
        let results = engine.batch_scrape(&refs).await;
        samples.push(began.elapsed().as_millis());
        assert_eq!(results.len(), hosts);
        for (_, result) in results {
            let page = result.unwrap();
            assert_eq!(page.status_code, 200);
            assert_eq!(page.html.len(), bytes);
        }
        assert_eq!(site.received_requests().await.unwrap().len(), (run + 1) * hosts);
    }
    let mut ordered = samples.clone();
    ordered.sort_unstable();
    println!(
        "BATCH_THROUGHPUT {}",
        serde_json::json!({"name": name, "hosts": hosts, "bytes": bytes, "runs": runs, "samples_ms": samples, "median_ms": (ordered[runs / 2 - 1] + ordered[runs / 2]) / 2})
    );
}
