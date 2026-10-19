//! A list that mounts only the items in view (a virtual list) loses the items below the first
//! screen unless `browser.scroll_containers` collects them as the container scrolls (#768).

#![cfg(feature = "browser")]

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, ScrapeResult, create_engine, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

const VIRTUAL_LIST: &str = r##"<!doctype html><html><head><title>List</title><style>
html,body{margin:0;height:100%}
#list{height:100vh;overflow-y:auto;position:relative;{{CSS}}}
#inner{position:relative}
.item{position:absolute;left:0;height:40px;line-height:40px}
</style></head><body><main><h1>Docs</h1>
<nav id="list"><div id="inner"></div></nav></main>
<script>
const N = 40, H = 40;
const label = (i) => {{LABEL}};
const list = document.getElementById("list"), inner = document.getElementById("inner");
inner.style.height = (N * H) + "px";
function draw() {
  inner.textContent = "";
  const first = Math.floor(list.scrollTop / H);
  const last = Math.min(N, first + Math.ceil(list.clientHeight / H) + 1);
  for (let i = first; i < last; i++) {
    const a = document.createElement("a");
    a.className = "item"; a.style.top = (i * H) + "px"; a.href = "#i" + (i + 1);
    a.textContent = label(i);
    inner.appendChild(a);
  }
}
list.addEventListener("scroll", draw);
draw();
</script></body></html>"##;

const ITEM_LABEL: &str = r#""Item " + (i + 1)"#;

fn virtual_list(css: &str, label: &str) -> String {
    VIRTUAL_LIST.replace("{{CSS}}", css).replace("{{LABEL}}", label)
}

/// An overflowing list that mounts every item up front; items gain a class as they scroll into
/// view, so their markup changes between harvests.
const PLAIN_OVERFLOW_LIST: &str = r##"<!doctype html><html><head><title>Plain</title><style>
html,body{margin:0}
#list{height:200px;overflow-y:auto}
#list a{display:block;height:40px}
</style></head><body><main><h1>Docs</h1><nav id="list"></nav></main>
<script>
const list = document.getElementById("list");
for (let i = 1; i <= 40; i++) {
  const a = document.createElement("a");
  a.href = "#i" + i; a.textContent = "Item " + i;
  list.appendChild(a);
}
function mark() {
  for (const a of list.children) {
    if (a.offsetTop < list.scrollTop + list.clientHeight) a.className = "visible";
  }
}
list.addEventListener("scroll", mark);
mark();
</script></body></html>"##;

/// An overflowing list with no script at all.
const STATIC_OVERFLOW_PAGE: &str = "<!doctype html><html><head><title>Static</title>\
<style>#list{height:100px;overflow-y:auto}</style></head><body><main><h1>Static</h1>\
<nav id=\"list\"><p>One</p><p>Two</p><p>Three</p><p>Four</p><p>Five</p><p>Six</p><p>Seven</p><p>Eight</p>\
<p>Nine</p><p>Ten</p></nav></main></body></html>";

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

async fn serve(body: String) -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
        .mount(&site)
        .await;
    site
}

/// The scrape of `body`, or `None` when this machine has no Chrome.
async fn scrape_page(test_name: &str, body: &str, scroll_containers: bool) -> Option<ScrapeResult> {
    let site = serve(body.to_owned()).await;
    let engine = create_engine(Some(config(scroll_containers))).expect("the engine must build");
    match scrape(&engine, &site.uri()).await {
        Ok(result) => Some(result),
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

/// `text` with the port of the mock server removed, since each scrape serves from its own port.
fn without_port(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("http://127.0.0.1:") {
        let (before, after) = rest.split_at(at + "http://127.0.0.1".len());
        out.push_str(before);
        rest = after
            .trim_start_matches(':')
            .trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

fn missing_items(text: &str) -> Vec<usize> {
    (1..=40).filter(|n| !text.contains(&format!("[Item {n}]"))).collect()
}

fn assert_all_items_in_order(text: &str) {
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
async fn virtual_list_keeps_only_the_first_screen_when_scrolling_is_off() {
    let name = "virtual_list_keeps_only_the_first_screen_when_scrolling_is_off";
    let Some(result) = scrape_page(name, &virtual_list("", ITEM_LABEL), false).await else {
        return;
    };
    let text = markdown(&result);
    assert!(text.contains("[Item 1]"), "the first screen must render: {text}");
    assert!(!missing_items(text).is_empty(), "the fixture must virtualise: {text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn virtual_list_returns_every_item_when_scroll_containers_is_on() {
    let name = "virtual_list_returns_every_item_when_scroll_containers_is_on";
    let Some(result) = scrape_page(name, &virtual_list("", ITEM_LABEL), true).await else {
        return;
    };
    assert_all_items_in_order(markdown(&result));
}

#[tokio::test(flavor = "multi_thread")]
async fn smooth_scrolling_container_still_returns_every_item() {
    let name = "smooth_scrolling_container_still_returns_every_item";
    let Some(result) = scrape_page(name, &virtual_list("scroll-behavior:smooth", ITEM_LABEL), true).await else {
        return;
    };
    assert_all_items_in_order(markdown(&result));
}

#[tokio::test(flavor = "multi_thread")]
async fn identical_markup_items_at_different_positions_are_all_kept() {
    let name = "identical_markup_items_at_different_positions_are_all_kept";
    let Some(result) = scrape_page(name, &virtual_list("", "\"Same\""), true).await else {
        return;
    };
    let text = markdown(&result);
    assert_eq!(text.matches("[Same]").count(), 40, "markdown: {text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn non_virtual_list_whose_items_change_markup_is_left_alone() {
    let name = "non_virtual_list_whose_items_change_markup_is_left_alone";
    let Some(off) = scrape_page(name, PLAIN_OVERFLOW_LIST, false).await else {
        return;
    };
    let Some(on) = scrape_page(name, PLAIN_OVERFLOW_LIST, true).await else {
        return;
    };
    assert_all_items_in_order(markdown(&off));
    assert_eq!(without_port(markdown(&off)), without_port(markdown(&on)));
    assert_eq!(on.html.matches("<a ").count(), 40, "no duplicated items: {}", on.html);
}

#[tokio::test(flavor = "multi_thread")]
async fn overflowing_static_list_is_unchanged_by_scroll_containers() {
    let name = "overflowing_static_list_is_unchanged_by_scroll_containers";
    let Some(off) = scrape_page(name, STATIC_OVERFLOW_PAGE, false).await else {
        return;
    };
    let Some(on) = scrape_page(name, STATIC_OVERFLOW_PAGE, true).await else {
        return;
    };
    assert_eq!(off.html, on.html);
    assert_eq!(markdown(&off), markdown(&on));
}

/// A long list whose every redraw is slow, so scrolling it to the end cannot finish inside the
/// script's time budget.
const SLOW_LONG_LIST: &str = r##"<!doctype html><html><head><title>Slow</title><style>
html,body{margin:0;height:100%}
#list{height:100vh;overflow-y:auto;position:relative}
#inner{position:relative}
.item{position:absolute;left:0;height:40px;line-height:40px}
</style></head><body><main><h1>Docs</h1>
<nav id="list"><div id="inner"></div></nav></main>
<script>
const N = 4000, H = 40;
const list = document.getElementById("list"), inner = document.getElementById("inner");
inner.style.height = (N * H) + "px";
function draw() {
  const t = performance.now();
  while (performance.now() - t < 150) {}
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

#[tokio::test(flavor = "multi_thread")]
async fn list_that_outlasts_the_time_budget_still_returns_the_items_collected() {
    let name = "list_that_outlasts_the_time_budget_still_returns_the_items_collected";
    let Some(result) = scrape_page(name, SLOW_LONG_LIST, true).await else {
        return;
    };
    let text = markdown(&result);
    let count = text.matches("](#i").count() + text.matches("/#i").count();
    assert!(
        count > 40,
        "items beyond the first screen must be kept ({count}): {text}"
    );
    assert!(
        !text.contains("[Item 4000]"),
        "the fixture must outlast the budget: {text}"
    );
    assert_eq!(text.matches("[Item 1]").count(), 1, "no duplicates: {text}");
}

/// Nine scrollable elements with a single child each precede the real list.
const DECOYS_BEFORE_LIST: &str = r##"<!doctype html><html><head><title>Decoys</title><style>
html,body{margin:0}
.decoy{height:30px;overflow-y:auto}
.decoy div{height:200px}
#list{height:300px;overflow-y:auto;position:relative}
#inner{position:relative}
.item{position:absolute;left:0;height:40px;line-height:40px}
</style></head><body><main><h1>Docs</h1>
<div class="decoy"><div>a</div></div><div class="decoy"><div>b</div></div>
<div class="decoy"><div>c</div></div><div class="decoy"><div>d</div></div>
<div class="decoy"><div>e</div></div><div class="decoy"><div>f</div></div>
<div class="decoy"><div>g</div></div><div class="decoy"><div>h</div></div>
<div class="decoy"><div>i</div></div><div class="decoy"><div>j</div></div>
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

#[tokio::test(flavor = "multi_thread")]
async fn decoy_scrollables_before_the_list_do_not_starve_it() {
    let name = "decoy_scrollables_before_the_list_do_not_starve_it";
    let Some(result) = scrape_page(name, DECOYS_BEFORE_LIST, true).await else {
        return;
    };
    assert_all_items_in_order(markdown(&result));
}

/// Items are laid out in flow after a spacer sized from an estimated row height; a row takes its real
/// height only from its second mount, so the positions of rows seen earlier shift between harvests.
const DYNAMIC_HEIGHT_LIST: &str = r##"<!doctype html><html><head><title>Dynamic</title><style>
html,body{margin:0}
#list{height:300px;overflow-y:auto;overflow-anchor:none}
.item{display:block}
</style></head><body><main><h1>Docs</h1>
<nav id="list"><div id="top"></div><div id="rows"></div><div id="bottom"></div></nav></main>
<script>
const N = 40, EST = 40;
const real = (i) => 30 + (i % 4) * 15;
const measured = new Map(), draws = new Map();
const list = document.getElementById("list");
const head = document.getElementById("top"), rows = document.getElementById("rows"), bottom = document.getElementById("bottom");
function offsetOf(i) {
  let y = 0;
  for (let k = 0; k < i; k++) y += measured.has(k) ? measured.get(k) : EST;
  return y;
}
function draw() {
  let first = 0;
  while (first < N - 1 && offsetOf(first + 1) <= list.scrollTop) first++;
  const last = Math.min(N, first + 9);
  rows.textContent = "";
  for (let i = first; i < last; i++) {
    const a = document.createElement("a");
    a.className = "item"; a.href = "#i" + (i + 1); a.textContent = "Item " + (i + 1);
    const seen = (draws.get(i) || 0) + 1;
    draws.set(i, seen);
    const height = seen >= 2 ? real(i) : EST;
    a.style.height = height + "px";
    rows.appendChild(a);
    measured.set(i, height);
  }
  head.style.height = offsetOf(first) + "px";
  bottom.style.height = (offsetOf(N) - offsetOf(last)) + "px";
}
list.addEventListener("scroll", draw);
draw();
</script></body></html>"##;

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_height_list_has_no_duplicated_items() {
    let name = "dynamic_height_list_has_no_duplicated_items";
    let Some(result) = scrape_page(name, DYNAMIC_HEIGHT_LIST, true).await else {
        return;
    };
    let text = markdown(&result);
    assert_all_items_in_order(text);
    for n in 1..=40 {
        assert_eq!(
            text.matches(&format!("[Item {n}]")).count(),
            1,
            "item {n} duplicated: {text}"
        );
    }
}
