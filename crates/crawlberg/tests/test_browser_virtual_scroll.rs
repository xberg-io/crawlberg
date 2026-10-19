//! A list that mounts only the items in view (a virtual list) loses the items below the first
//! screen unless `browser.scroll_containers` collects them as the container scrolls (#768).

#![cfg(feature = "browser")]

use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, ScrapeResult, create_engine, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

const VIRTUAL_LIST: &str = r##"<!doctype html><html><head><title>List</title><style>
html,body{margin:0;height:100%}
#list{height:100vh;overflow-y:auto;position:relative}
#inner{position:relative}
.item{position:absolute;left:0;height:40px;line-height:40px}
</style></head><body><main><h1>Docs</h1>
<nav id="list"><div id="inner"></div></nav></main>
<script>
const N = 40, H = 40;
const list = document.getElementById("list"), inner = document.getElementById("inner");
inner.style.height = (N * H) + "px";
function draw() {
  inner.textContent = "";
  const first = Math.floor(list.scrollTop / H);
  const last = Math.min(N, first + Math.ceil(list.clientHeight / H) + 1);
  for (let i = first; i < last; i++) {
    const a = document.createElement("a");
    a.className = "item"; a.style.top = (i * H) + "px"; a.href = "#i" + (i + 1);
    a.textContent = "Item " + (i + 1);
    inner.appendChild(a);
  }
}
list.addEventListener("scroll", draw);
draw();
</script></body></html>"##;

const STATIC_PAGE: &str = "<!doctype html><html><head><title>Static</title></head><body><main>\
<h1>Static</h1><p>Plain paragraph one.</p><p>Plain paragraph two.</p></main></body></html>";

fn config(scroll_containers: bool) -> CrawlConfig {
    let mut config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            scroll_containers,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    config.content.remove_navigation = false;
    config
}

async fn serve(body: &'static str) -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
        .mount(&site)
        .await;
    site
}

/// The scrape of `body`, or `None` when this machine has no Chrome.
async fn scrape_page(test_name: &str, body: &'static str, scroll_containers: bool) -> Option<(ScrapeResult, Duration)> {
    let site = serve(body).await;
    let engine = create_engine(Some(config(scroll_containers))).expect("the engine must build");
    let started = Instant::now();
    match scrape(&engine, &site.uri()).await {
        Ok(result) => Some((result, started.elapsed())),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("{test_name}: the scrape must succeed, got {error:?}"),
    }
}

fn markdown(result: &ScrapeResult) -> &str {
    &result.markdown.as_ref().expect("markdown").content
}

fn missing_items(text: &str) -> Vec<usize> {
    (1..=40).filter(|n| !text.contains(&format!("[Item {n}]"))).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn virtual_list_keeps_only_the_first_screen_when_scrolling_is_off() {
    let name = "virtual_list_keeps_only_the_first_screen_when_scrolling_is_off";
    let Some((result, _)) = scrape_page(name, VIRTUAL_LIST, false).await else {
        return;
    };
    let text = markdown(&result);
    assert!(text.contains("[Item 1]"), "the first screen must render: {text}");
    assert!(!missing_items(text).is_empty(), "the fixture must virtualise: {text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn virtual_list_returns_every_item_when_scroll_containers_is_on() {
    let name = "virtual_list_returns_every_item_when_scroll_containers_is_on";
    let Some((result, _)) = scrape_page(name, VIRTUAL_LIST, true).await else {
        return;
    };
    let text = markdown(&result);
    assert_eq!(missing_items(text), Vec::<usize>::new(), "markdown: {text}");
    let positions: Vec<usize> = (1..=40)
        .map(|n| text.find(&format!("[Item {n}]")).expect("item present"))
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "items must stay in order: {text}"
    );
    assert_eq!(text.matches("[Item 1]").count(), 1, "no duplicates: {text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn static_page_is_unchanged_by_scroll_containers() {
    let name = "static_page_is_unchanged_by_scroll_containers";
    let Some((off, off_time)) = scrape_page(name, STATIC_PAGE, false).await else {
        return;
    };
    let Some((on, on_time)) = scrape_page(name, STATIC_PAGE, true).await else {
        return;
    };
    assert_eq!(off.html, on.html);
    assert_eq!(markdown(&off), markdown(&on));
    assert!(
        on_time < off_time + Duration::from_secs(2),
        "scroll step overhead on a static page: off {off_time:?}, on {on_time:?}"
    );
}
