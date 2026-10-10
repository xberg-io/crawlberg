//! Each engine has a cookie store of its own in HTTP mode.
//!
//! One engine sends the cookies it received on its later requests. A second engine starts with no
//! cookie, whether its configuration equals the first engine's or not.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crawlberg::{
    BrowserConfig, BrowserMode, CrawlConfig, CrawlEngineHandle, batch_crawl, batch_crawl_stream, batch_scrape, crawl,
    crawl_stream, create_engine, map_urls, scrape,
};
use futures::StreamExt as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(cookies_enabled: bool) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            mode: BrowserMode::Never,
            ..BrowserConfig::default()
        },
        cookies_enabled,
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

fn engine() -> CrawlEngineHandle {
    create_engine(Some(config(true))).expect("the engine must build")
}

/// A site where `/set-a` sets the cookie `a=1`, `/set-b` sets `b=1`, and every other page sets
/// nothing.
async fn site() -> MockServer {
    let site = MockServer::start().await;
    let html =
        |body: &str| ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html");
    for (at, cookie) in [("/set-a", "a=1; Path=/"), ("/set-b", "b=1; Path=/")] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(html("set").append_header("set-cookie", cookie))
            .mount(&site)
            .await;
    }
    Mock::given(method("GET")).respond_with(html("page")).mount(&site).await;
    site
}

/// The number of requests the site has received.
async fn mark(site: &MockServer) -> usize {
    site.received_requests().await.expect("request recording is on").len()
}

/// The cookie header of each request the site received after `mark`, empty for a request with none.
async fn cookies_since(site: &MockServer, mark: usize) -> Vec<String> {
    site.received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .skip(mark)
        .map(|request| {
            let cookie = request.headers.get("cookie").and_then(|value| value.to_str().ok());
            cookie.unwrap_or_default().to_owned()
        })
        .collect()
}

/// Crawl `at` with `engine` and return the cookie header of each request that made.
async fn crawl_cookies(engine: &CrawlEngineHandle, site: &MockServer, at: &str) -> Vec<String> {
    let before = mark(site).await;
    crawl(engine, &format!("{}{at}", site.uri()))
        .await
        .expect("the crawl must succeed");
    cookies_since(site, before).await
}

#[tokio::test]
async fn a_new_engine_with_the_same_configuration_sends_no_cookie_of_a_live_engine() {
    let site = site().await;
    let first = engine();
    crawl_cookies(&first, &site, "/set-a").await;

    let second = engine();

    assert_eq!(crawl_cookies(&second, &site, "/page").await, [""]);
}

#[tokio::test]
async fn a_new_engine_with_the_same_configuration_sends_no_cookie_of_a_dropped_engine() {
    let site = site().await;
    let first = engine();
    crawl_cookies(&first, &site, "/set-a").await;
    drop(first);

    let second = engine();

    assert_eq!(crawl_cookies(&second, &site, "/page").await, [""]);
}

#[tokio::test]
async fn two_live_engines_each_send_only_their_own_cookie() {
    let site = site().await;
    let (first, second) = (engine(), engine());
    crawl_cookies(&first, &site, "/set-a").await;
    crawl_cookies(&second, &site, "/set-b").await;

    assert_eq!(crawl_cookies(&first, &site, "/page-1").await, ["a=1"]);
    assert_eq!(crawl_cookies(&second, &site, "/page-2").await, ["b=1"]);
}

#[tokio::test]
async fn one_engine_sends_the_cookie_of_its_first_crawl_on_its_second_crawl() {
    let site = site().await;
    let engine = engine();
    crawl_cookies(&engine, &site, "/set-a").await;

    assert_eq!(crawl_cookies(&engine, &site, "/page").await, ["a=1"]);
}

#[tokio::test]
async fn an_engine_with_cookies_off_stores_and_sends_no_cookie() {
    let site = site().await;
    let engine = create_engine(Some(config(false))).expect("the engine must build");
    crawl_cookies(&engine, &site, "/set-a").await;

    assert_eq!(crawl_cookies(&engine, &site, "/page").await, [""]);
}

#[tokio::test]
async fn a_cookie_header_of_the_caller_is_sent_in_place_of_the_stored_cookies() {
    let site = site().await;
    let mut config = config(true);
    config.custom_headers.insert("Cookie".to_owned(), "mine=1".to_owned());
    let engine = create_engine(Some(config)).expect("the engine must build");
    crawl_cookies(&engine, &site, "/set-a").await;

    assert_eq!(crawl_cookies(&engine, &site, "/page").await, ["mine=1"]);
}

/// A keep-alive HTTP server on a local port that answers every request with a page and counts the
/// connections it accepts.
async fn counting_site() -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = format!("http://{}", listener.local_addr().expect("local address"));
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let body = "<html><body>page</body></html>";
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                let mut request = Vec::new();
                let mut buf = [0_u8; 1024];
                while let Ok(read @ 1..) = stream.read(&mut buf).await {
                    request.extend_from_slice(&buf[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        request.clear();
                        if stream.write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    (address, accepted)
}

#[tokio::test]
async fn the_second_request_of_an_engine_and_a_second_engine_reuse_the_open_connection() {
    let (address, accepted) = counting_site().await;
    let first = engine();
    for at in ["/one", "/two"] {
        scrape(&first, &format!("{address}{at}"))
            .await
            .expect("the scrape must succeed");
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the second request of one engine must reuse the connection of the first"
    );

    let second = engine();
    scrape(&second, &format!("{address}/three"))
        .await
        .expect("the scrape must succeed");
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "an engine with the same configuration must reuse the open connection"
    );
}

/// The entry points of an engine that send requests in HTTP mode.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Scrape,
    Crawl,
    Map,
    BatchScrape,
    BatchCrawl,
    CrawlStream,
    BatchCrawlStream,
}

impl Entry {
    const ALL: [Self; 7] = [
        Self::Scrape,
        Self::Crawl,
        Self::Map,
        Self::BatchScrape,
        Self::BatchCrawl,
        Self::CrawlStream,
        Self::BatchCrawlStream,
    ];

    /// Request `url` through this entry point of `engine`, to the end of its work.
    async fn request(self, engine: &CrawlEngineHandle, url: String) {
        match self {
            Self::Scrape => {
                scrape(engine, &url).await.expect("scrape");
            }
            Self::Crawl => {
                crawl(engine, &url).await.expect("crawl");
            }
            Self::Map => {
                map_urls(engine, &url).await.expect("map");
            }
            Self::BatchScrape => {
                batch_scrape(engine, vec![url]).await.expect("batch scrape");
            }
            Self::BatchCrawl => {
                batch_crawl(engine, vec![url]).await.expect("batch crawl");
            }
            Self::CrawlStream => {
                let stream = crawl_stream(engine, &url).await.expect("crawl stream");
                stream.for_each(|_| async {}).await;
            }
            Self::BatchCrawlStream => {
                let stream = batch_crawl_stream(engine, vec![url]).await.expect("batch crawl stream");
                stream.for_each(|_| async {}).await;
            }
        }
    }
}

#[tokio::test]
async fn every_entry_point_sends_the_cookies_of_its_own_engine_only() {
    let mut wrong = Vec::new();
    for entry in Entry::ALL {
        let site = site().await;
        let first = engine();
        crawl_cookies(&first, &site, "/set-a").await;

        let before = mark(&site).await;
        entry.request(&first, format!("{}/own", site.uri())).await;
        let own = cookies_since(&site, before).await;
        if own.is_empty() || own.iter().any(|cookie| cookie != "a=1") {
            wrong.push(format!("{entry:?}: the engine that received the cookie sent {own:?}"));
        }

        let second = engine();
        let before = mark(&site).await;
        entry.request(&second, format!("{}/other", site.uri())).await;
        let other = cookies_since(&site, before).await;
        if other.is_empty() || !other.iter().all(String::is_empty) {
            wrong.push(format!("{entry:?}: a new engine sent {other:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "each entry point must send the cookie of its own engine and none of another engine:\n{}",
        wrong.join("\n")
    );
}
