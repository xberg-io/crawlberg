//! The commands a browser scrape sends to Chrome for the blocked URL patterns.
//!
//! ~keep The tests read the trace line `Sending MethodCall { .. }` of the vendored chromiumoxide
//! ~keep connection. The trace subscriber is global, so the two scrapes are in one test.
#![cfg(feature = "browser")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crawlberg::{BrowserConfig, BrowserMode, CrawlConfig, create_engine, scrape};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const BLOCKED_URLS_COMMAND: &str = "Network.setBlockedURLs";

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The names of the commands that one scrape of `site` with `patterns` sent to Chrome.
async fn commands_of_a_scrape(capture: &Capture, site: &MockServer, patterns: Vec<String>) -> Vec<String> {
    let config = CrawlConfig {
        browser: BrowserConfig {
            mode: BrowserMode::Always,
            block_url_patterns: patterns,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let engine = create_engine(Some(config)).expect("engine");
    capture.0.lock().expect("capture").clear();
    scrape(&engine, &format!("{}/page", site.uri())).await.expect("page");
    // ~keep The teardown of the browser writes its last trace lines after the scrape returns.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let text = String::from_utf8_lossy(&capture.0.lock().expect("capture")).into_owned();
    text.lines()
        .filter(|line| line.contains("Sending "))
        .filter_map(|line| {
            let rest = &line[line.find("method: \"")? + 9..];
            Some(rest[..rest.find('"')?].to_owned())
        })
        .collect()
}

/// A scrape with the default settings sends no command for the blocked URL patterns, so an
/// endpoint that refuses the command cannot fail it. A scrape with patterns sends the command
/// once.
#[tokio::test]
async fn a_default_scrape_sends_no_blocked_url_command() {
    let capture = Capture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body><p>page</p></body></html>", "text/html"))
        .mount(&site)
        .await;

    let default_commands = commands_of_a_scrape(&capture, &site, Vec::new()).await;
    assert!(
        default_commands.iter().any(|name| name == "Page.navigate"),
        "the trace must show the commands of the scrape: {default_commands:?}"
    );
    assert!(
        !default_commands.iter().any(|name| name == BLOCKED_URLS_COMMAND),
        "a default scrape must send no command for the blocked URL patterns: {default_commands:?}"
    );

    let with_patterns = commands_of_a_scrape(&capture, &site, vec!["*.png".to_owned()]).await;
    let sent = with_patterns
        .iter()
        .filter(|name| *name == BLOCKED_URLS_COMMAND)
        .count();
    assert_eq!(sent, 1, "a scrape with patterns must set them once: {with_patterns:?}");
    let mut setup_with_patterns = page_setup(&with_patterns);
    setup_with_patterns.retain(|name| name != BLOCKED_URLS_COMMAND);
    assert_eq!(
        setup_with_patterns,
        page_setup(&default_commands),
        "the patterns must add one command to the setup of the page and no other"
    );
}

/// The commands from the creation of the page to its navigation.
///
/// ~keep The capture of one scrape can start with the last teardown commands of the browser of
/// ~keep the scrape before it, so the whole lists are not compared.
fn page_setup(commands: &[String]) -> Vec<String> {
    let from = commands
        .iter()
        .position(|name| name == "Target.createTarget")
        .expect("the scrape creates a page");
    let to = commands
        .iter()
        .position(|name| name == "Page.navigate")
        .expect("the scrape navigates the page");
    commands[from..=to].to_vec()
}
