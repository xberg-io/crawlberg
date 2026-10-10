//! Pages that browser mode lost after the full browser timeout: a page that opens a JavaScript
//! dialog, and a page whose text holds half of a surrogate pair. Each is returned at once, with
//! what a reader of the page gets, by a scrape, by a crawl and by an interact session.
//!
//! Each test runs its rows to the end and prints one `ROW` line for each, with the time the row
//! took, so that one run shows every row of a browser.
//!
//! Requires a real Chrome binary and the `browser` feature; skipped (not failed) when Chrome
//! is unavailable, matching the other browser tests.

#![cfg(feature = "browser")]

use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, PageAction, crawl, create_engine, interact,
    scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// The browser timeout of every fetch here. A result well inside it proves no wait for it.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a browser fetch may take and still count as prompt.
const MAX_PROMPT_RETURN: Duration = Duration::from_secs(10);

/// The three dialogs a script opens with a call, and what the call returns when the dialog is
/// dismissed: nothing from an alert, `false` from a confirm, `null` from a prompt.
const DIALOG_CALLS: [(&str, &str); 3] = [
    ("alert('a')", "undefined"),
    ("confirm('a')", "false"),
    ("prompt('a', 'the default')", "null"),
];

fn config(mode: BrowserMode) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode,
            timeout: BROWSER_TIMEOUT,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// A site that answers each route with its HTML body.
async fn html_site(pages: &[(&str, String)]) -> MockServer {
    let site = MockServer::start().await;
    for (route, body) in pages {
        Mock::given(method("GET"))
            .and(path(*route))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.clone(), "text/html; charset=utf-8"))
            .mount(&site)
            .await;
    }
    site
}

/// The address of `site` under its other name, which makes it another site for the browser.
fn as_another_site(site: &MockServer) -> String {
    site.uri().replace("127.0.0.1", "localhost")
}

/// The rows of one test: each is run and recorded, and the test fails at its end if a row failed.
struct Rows {
    test: &'static str,
    failures: Vec<String>,
}

impl Rows {
    fn new(test: &'static str) -> Self {
        Self {
            test,
            failures: Vec::new(),
        }
    }

    /// Record a row that returned `got` after `elapsed`. The row passes when it returned inside
    /// the prompt limit and `got` holds every string of `expected`.
    #[allow(clippy::print_stderr, reason = "the row and its time are the record of a run")]
    fn record(&mut self, row: &str, elapsed: Duration, got: Result<String, CrawlError>, expected: &[&str]) {
        let failure = match &got {
            Err(error) => Some(format!("the caller got the error {error:?}")),
            Ok(_) if elapsed >= MAX_PROMPT_RETURN => Some(format!(
                "it waited {elapsed:.1?}; the browser timeout is {BROWSER_TIMEOUT:?}"
            )),
            Ok(got) => expected
                .iter()
                .find(|expected| !got.contains(**expected))
                .map(|missing| format!("{missing:?} is missing from {got}")),
        };
        let verdict = if failure.is_some() { "FAIL" } else { "ok" };
        eprintln!("ROW {} | {row} | {elapsed:.1?} | {verdict}", self.test);
        if let Some(failure) = failure {
            self.failures.push(format!("{row}: {failure}"));
        }
    }

    /// Print the skip line when `result` says that no usable Chrome exists here.
    fn skipped<T>(&self, result: &Result<T, CrawlError>) -> bool {
        match result {
            Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(message) => {
                announce_chrome_skip(self.test, message);
                true
            }
            _ => false,
        }
    }

    /// Scrape `url` in browser mode as one row. `false` when no usable Chrome exists here.
    async fn scrape(&mut self, row: &str, config: CrawlConfig, url: &str, expected: &[&str]) -> bool {
        let engine = create_engine(Some(config)).expect("engine must build");
        let started = Instant::now();
        let result = scrape(&engine, url).await;
        let elapsed = started.elapsed();
        if self.skipped(&result) {
            return false;
        }
        self.record(row, elapsed, result.map(|page| page.html), expected);
        true
    }

    /// Run `actions` on `url` in an interact session as one row. The row is checked against the
    /// final address, the final HTML and the data of each action. `false` when no usable Chrome
    /// exists here.
    async fn interact(&mut self, row: &str, url: &str, actions: Vec<PageAction>, expected: &[&str]) -> bool {
        let engine = create_engine(Some(config(BrowserMode::Auto))).expect("engine must build");
        let started = Instant::now();
        let result = interact(&engine, url, actions).await;
        let elapsed = started.elapsed();
        if self.skipped(&result) {
            return false;
        }
        let got = result.map(|session| {
            let actions: Vec<String> = session
                .action_results
                .iter()
                .map(|action| match (&action.data, &action.error) {
                    (_, Some(error)) => format!("error={error}"),
                    (Some(data), None) => format!("data={data}"),
                    (None, None) => "done".to_owned(),
                })
                .collect();
            format!(
                "url={} actions=[{}] html={}",
                session.final_url,
                actions.join("; "),
                session.final_html
            )
        });
        self.record(row, elapsed, got, expected);
        true
    }

    fn finish(self) {
        assert!(
            self.failures.is_empty(),
            "{}: {} rows failed:\n{}",
            self.test,
            self.failures.len(),
            self.failures.join("\n")
        );
    }
}

/// A page that opens a dialog with `call` while it loads and writes what the call returned.
fn dialog_page(call: &str) -> String {
    format!(
        "<!doctype html><html><head><title>t</title></head><body><h1>Dialog page</h1>\
         <p>text before the dialog</p>\
         <script>document.write('<p id=\"answer\">answer=' + String({call}) + '</p>');</script>\
         <p>text after the dialog</p></body></html>"
    )
}

/// A dialog that opens while the page loads is closed, and the page is returned with the text
/// after the script. The page sees what a reader who dismisses the dialog gives it. A page that
/// opens one dialog after another is returned too: each dialog is closed.
#[tokio::test]
async fn a_page_that_opens_a_dialog_while_it_loads_is_returned() {
    let mut rows = Rows::new("a_page_that_opens_a_dialog_while_it_loads_is_returned");
    let two = "[alert('a'), confirm('b')].length";
    let four = "[alert('a'), confirm('b'), prompt('c'), alert('d')].length";
    let cases = DIALOG_CALLS.into_iter().chain([(two, "2"), (four, "4")]);
    for (call, answer) in cases {
        let site = html_site(&[("/", dialog_page(call))]).await;
        let answer = format!("answer={answer}<");
        let expected = ["text before the dialog", answer.as_str(), "text after the dialog"];
        if !rows
            .scrape(call, config(BrowserMode::Always), &site.uri(), &expected)
            .await
        {
            return;
        }
    }
    rows.finish();
}

/// A dialog that a timer opens after the page has loaded, during the extra wait, does not hold
/// the read of the page.
#[tokio::test]
async fn a_dialog_that_opens_in_a_timer_does_not_hold_the_page() {
    let mut rows = Rows::new("a_dialog_that_opens_in_a_timer_does_not_hold_the_page");
    for (call, answer) in DIALOG_CALLS {
        let page = format!(
            "<!doctype html><html><body><p>start</p><script>setTimeout(() => {{ const answer = String({call}); \
             document.body.insertAdjacentHTML('beforeend', '<p>after the late dialog: ' + answer + '</p>'); }}, 300);\
             </script></body></html>"
        );
        let site = html_site(&[("/", page)]).await;
        let mut config = config(BrowserMode::Always);
        config.browser.extra_wait = Some(Duration::from_millis(1500));
        let late = format!("after the late dialog: {answer}<");
        if !rows.scrape(call, config, &site.uri(), &["start", late.as_str()]).await {
            return;
        }
    }
    rows.finish();
}

/// A dialog that a frame opens, of the same site or of another site, does not hold the page that
/// embeds the frame.
#[tokio::test]
async fn a_dialog_in_a_frame_does_not_hold_the_page() {
    let mut rows = Rows::new("a_dialog_in_a_frame_does_not_hold_the_page");
    let cases = DIALOG_CALLS
        .into_iter()
        .map(|(call, _)| (call, true))
        .chain([("alert('a')", false)]);
    for (call, another_site) in cases {
        let site = MockServer::start().await;
        let frame_site = if another_site {
            as_another_site(&site)
        } else {
            site.uri()
        };
        let host = format!(
            "<!doctype html><html><body><p>host text</p><iframe src=\"{frame_site}/frame\"></iframe>\
             <p>text after the frame</p></body></html>"
        );
        for (route, body) in [("/", host), ("/frame", dialog_page(call))] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html; charset=utf-8"))
                .mount(&site)
                .await;
        }
        let row = format!(
            "{call} in a frame of {}",
            if another_site { "another site" } else { "the same site" }
        );
        let expected = ["host text", "text after the frame"];
        if !rows
            .scrape(&row, config(BrowserMode::Always), &site.uri(), &expected)
            .await
        {
            return;
        }
    }
    rows.finish();
}

/// A page that asks before it unloads opens no dialog while nobody has clicked in it: the page is
/// returned, as it was before dialogs were closed.
#[tokio::test]
async fn a_page_that_asks_before_it_unloads_is_returned() {
    let mut rows = Rows::new("a_page_that_asks_before_it_unloads_is_returned");
    let page = "<!doctype html><html><body><p>a page that asks before it unloads</p>\
                <script>addEventListener('beforeunload', (event) => { event.preventDefault(); \
                event.returnValue = 'stay?'; });</script></body></html>";
    let site = html_site(&[("/", page.to_owned())]).await;
    let expected = ["a page that asks before it unloads"];
    if !rows
        .scrape(
            "beforeunload, no click",
            config(BrowserMode::Always),
            &site.uri(),
            &expected,
        )
        .await
    {
        return;
    }
    rows.finish();
}

/// A page whose script wrote `text` into its text and into an attribute.
fn surrogate_page(text: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset='utf-8'><title>t</title></head><body><h1>Text</h1>\
         <p id='out'></p><script>const out = document.getElementById('out'); out.textContent = '{text}'; \
         out.setAttribute('data-text', '{text}');</script>\
         <p>Some ordinary words so that the page is not empty.</p></body></html>"
    )
}

/// A page whose text and whose attribute hold half of a surrogate pair is returned, with the
/// replacement character in place of the unpaired half, as a browser shows it. A whole pair stays
/// the character it is.
#[tokio::test]
async fn a_page_whose_text_holds_half_of_a_surrogate_pair_is_returned() {
    let mut rows = Rows::new("a_page_whose_text_holds_half_of_a_surrogate_pair_is_returned");
    for (text, shown) in [
        (r"before 😀 after", "before \u{1F600} after"),
        (concat!(r"before \ud83d", r"\ude00 after"), "before \u{1F600} after"),
        (r"before \ud83d after", "before \u{FFFD} after"),
        (r"before \ude00 after", "before \u{FFFD} after"),
        (r"before \ude00\ud83d after", "before \u{FFFD}\u{FFFD} after"),
        (r"at the end \ud83d", "at the end \u{FFFD}"),
    ] {
        let site = html_site(&[("/", surrogate_page(text))]).await;
        let in_text = format!(">{shown}</p>");
        let in_attribute = format!("data-text=\"{shown}\"");
        let expected = [in_text.as_str(), in_attribute.as_str(), "Some ordinary words"];
        if !rows
            .scrape(text, config(BrowserMode::Always), &site.uri(), &expected)
            .await
        {
            return;
        }
    }
    rows.finish();
}

/// A page whose title, and whose frame of another site's title, hold half of a surrogate pair is
/// returned: Chrome reports a title in its target events as well as in the page.
#[tokio::test]
async fn a_title_that_holds_half_of_a_surrogate_pair_does_not_lose_the_page() {
    let mut rows = Rows::new("a_title_that_holds_half_of_a_surrogate_pair_does_not_lose_the_page");
    let site = MockServer::start().await;
    let other_site = as_another_site(&site);
    let titled = |body: String| {
        format!(
            "<!doctype html><html><head><title>t</title></head><body>{body}\
             <script>document.title = 'half \\ud83d title';</script></body></html>"
        )
    };
    let host = titled(format!("<p>host text</p><iframe src=\"{other_site}/frame\"></iframe>"));
    for (route, body) in [("/", host), ("/frame", titled("<p>frame text</p>".to_owned()))] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html; charset=utf-8"))
            .mount(&site)
            .await;
    }
    let expected = ["host text", "<title>half \u{FFFD} title</title>"];
    if !rows
        .scrape(
            "the title of a page and of its frame",
            config(BrowserMode::Always),
            &site.uri(),
            &expected,
        )
        .await
    {
        return;
    }
    rows.finish();
}

/// A dialog whose message holds half of a surrogate pair is closed like any other dialog.
#[tokio::test]
async fn a_dialog_whose_message_holds_half_of_a_surrogate_pair_is_closed() {
    let mut rows = Rows::new("a_dialog_whose_message_holds_half_of_a_surrogate_pair_is_closed");
    let site = html_site(&[("/", dialog_page(r"alert('half \ud83d')"))]).await;
    let expected = ["answer=undefined<", "text after the dialog"];
    if !rows
        .scrape(
            "alert with half a pair",
            config(BrowserMode::Always),
            &site.uri(),
            &expected,
        )
        .await
    {
        return;
    }
    rows.finish();
}

/// A crawl returns a page that opens a dialog and the page it links to, whose text holds half of
/// a surrogate pair.
#[tokio::test]
async fn a_crawl_returns_the_pages_that_a_scrape_returns() {
    let mut rows = Rows::new("a_crawl_returns_the_pages_that_a_scrape_returns");
    let first = "<!doctype html><html><body><p>first page</p><script>alert('a');</script>\
                 <p>text after the dialog</p><a href=\"/second\">second</a></body></html>";
    let site = html_site(&[
        ("/", first.to_owned()),
        ("/second", surrogate_page(r"before \ud83d after")),
    ])
    .await;
    let mut config = config(BrowserMode::Always);
    config.max_depth = Some(1);
    let engine = create_engine(Some(config)).expect("engine must build");
    let started = Instant::now();
    let result = crawl(&engine, &site.uri()).await;
    let elapsed = started.elapsed();
    if rows.skipped(&result) {
        return;
    }
    let got = result.map(|crawled| {
        let pages: Vec<String> = crawled.pages.iter().map(|page| page.html.clone()).collect();
        format!("pages={} {}", pages.len(), pages.join("\n"))
    });
    let expected = ["pages=2 ", "text after the dialog", ">before \u{FFFD} after</p>"];
    rows.record(
        "a dialog page that links to a page with half a pair",
        elapsed,
        got,
        &expected,
    );
    rows.finish();
}

/// A page with a button that runs `script` when it is clicked.
fn button_page(script: &str) -> String {
    format!(
        "<!doctype html><html><body><button id=\"go\">go</button><p id=\"answer\"></p>\
         <script>document.getElementById('go').addEventListener('click', () => {{ {script} }});</script>\
         </body></html>"
    )
}

fn click_then_scrape() -> Vec<PageAction> {
    vec![
        PageAction::Click {
            selector: "#go".to_owned(),
        },
        PageAction::Scrape,
    ]
}

/// A dialog that an interact session meets is closed, so the action and the session end: a dialog
/// that a click opens, two in a row, one that the page opens while it loads and one that a script
/// of the session opens.
#[tokio::test]
async fn a_dialog_does_not_hold_the_interact_session() {
    let mut rows = Rows::new("a_dialog_does_not_hold_the_interact_session");
    let two = "[alert('a'), confirm('b')].length";
    for (call, answer) in DIALOG_CALLS.into_iter().chain([(two, "2")]) {
        let script = format!("document.getElementById('answer').textContent = 'clicked=' + String({call});");
        let site = html_site(&[("/", button_page(&script))]).await;
        let clicked = format!("clicked={answer}<");
        let row = format!("{call} after a click");
        if !rows
            .interact(&row, &site.uri(), click_then_scrape(), &[clicked.as_str()])
            .await
        {
            return;
        }
    }

    let site = html_site(&[("/", dialog_page("alert('a')"))]).await;
    rows.interact(
        "alert('a') while the page loads",
        &site.uri(),
        vec![PageAction::Scrape],
        &["answer=undefined<", "text after the dialog"],
    )
    .await;

    let site = html_site(&[("/", button_page(""))]).await;
    rows.interact(
        "confirm('a') in a script of the session",
        &site.uri(),
        vec![PageAction::ExecuteJs {
            script: "'the script went on: ' + confirm('a')".to_owned(),
        }],
        &["data=\"the script went on: false\""],
    )
    .await;
    rows.finish();
}

/// A page that asks before it unloads, after a click in it, opens a `beforeunload` dialog when it
/// navigates. The dialog is accepted, so the navigation goes on: to dismiss it keeps the session
/// on the page it wanted to leave.
#[tokio::test]
async fn a_page_that_asks_before_it_unloads_lets_the_interact_session_navigate() {
    let mut rows = Rows::new("a_page_that_asks_before_it_unloads_lets_the_interact_session_navigate");
    let leave = "addEventListener('beforeunload', (event) => { event.preventDefault(); \
                 event.returnValue = 'stay?'; }); location.href = '/next';";
    let next = "<!doctype html><html><body><p id=\"next\">the next page</p></body></html>";
    let site = html_site(&[("/", button_page(leave)), ("/next", next.to_owned())]).await;
    let actions = vec![
        PageAction::Click {
            selector: "#go".to_owned(),
        },
        PageAction::Wait {
            milliseconds: None,
            selector: Some("#next".to_owned()),
        },
        PageAction::Scrape,
    ];
    if !rows
        .interact(
            "beforeunload after a click",
            &site.uri(),
            actions,
            &["/next actions=", "the next page"],
        )
        .await
    {
        return;
    }
    rows.finish();
}

/// The result of a script of an interact session that holds half of a surrogate pair is returned,
/// with the replacement character in place of the unpaired half.
#[tokio::test]
async fn a_script_result_that_holds_half_of_a_surrogate_pair_is_returned() {
    let mut rows = Rows::new("a_script_result_that_holds_half_of_a_surrogate_pair_is_returned");
    let site = html_site(&[("/", surrogate_page("plain"))]).await;
    for (script, shown) in [
        (r"'half \ud83d result'", "data=\"half \u{FFFD} result\""),
        (
            concat!(r"'whole \ud83d", r"\ude00 result'"),
            "data=\"whole \u{1F600} result\"",
        ),
    ] {
        let actions = vec![PageAction::ExecuteJs {
            script: script.to_owned(),
        }];
        if !rows.interact(script, &site.uri(), actions, &[shown]).await {
            return;
        }
    }
    rows.finish();
}
