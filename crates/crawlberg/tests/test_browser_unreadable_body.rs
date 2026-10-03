//! ~keep A successful response whose encoded body Chrome cannot decode must fail in browser mode.

#![cfg(feature = "browser")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, InteractionResult, create_engine, interact,
    scrape,
};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn spawn_site() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let address = listener.local_addr().expect("test server should have an address");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            std::thread::spawn(move || {
                let Ok(mut writer) = stream.try_clone() else { return };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    return;
                }
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(0) | Err(_) => return,
                        Ok(_) if header == "\r\n" || header == "\n" => break,
                        Ok(_) => {}
                    }
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");
                let response = match path {
                    "/" => html_response(
                        "<p>start-marker</p><script>setTimeout(() => location.assign('/broken'), 100)</script>",
                    ),
                    // ~keep The declared encoding makes Chrome reject these complete bytes as a
                    // ~keep body-decoding failure rather than accepting them as truncated HTML.
                    "/broken" => "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\n\
                                  Content-Length: 8\r\nConnection: close\r\n\r\nnot-gzip"
                        .to_owned(),
                    "/redirect-broken" => "HTTP/1.1 302 Found\r\nLocation: mailto:someone@example.com\r\n\
                                            Content-Type: text/html\r\nContent-Encoding: gzip\r\n\
                                            Content-Length: 8\r\nConnection: close\r\n\r\nnot-gzip"
                        .to_owned(),
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                };
                let _ = writer.write_all(response.as_bytes());
                let _ = writer.flush();
            });
        }
    });
    format!("http://{address}")
}

fn assert_unreadable_interact_error(test_name: &str, result: Result<InteractionResult, CrawlError>) {
    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(CrawlError::BrowserError { message, .. }) => {
            assert!(
                message.contains("HTTP 200") && message.contains("/broken"),
                "{test_name}: {message}"
            );
        }
        other => panic!("{test_name}: the unreadable 200 response must fail before actions, got {other:?}"),
    }
}

fn html_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(5),
            extra_wait: Some(Duration::from_secs(1)),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

fn assert_unreadable_body_error(test_name: &str, result: Result<crawlberg::ScrapeResult, CrawlError>) {
    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(CrawlError::BrowserError { message, .. }) => {
            assert!(
                message.contains("HTTP 200") && message.contains("/broken"),
                "{test_name}: {message}"
            );
        }
        other => panic!("{test_name}: the unreadable 200 response must be a browser error, got {other:?}"),
    }
}

#[tokio::test]
async fn chromiumoxide_fails_when_the_seed_has_an_unreadable_success_body() {
    let test_name = "chromiumoxide_fails_when_the_seed_has_an_unreadable_success_body";
    let site = spawn_site();
    let engine = create_engine(Some(config())).expect("engine must build");
    assert_unreadable_body_error(test_name, scrape(&engine, &format!("{site}/broken")).await);
}

#[tokio::test]
async fn chromiumoxide_fails_when_a_late_navigation_has_an_unreadable_success_body() {
    let test_name = "chromiumoxide_fails_when_a_late_navigation_has_an_unreadable_success_body";
    let site = spawn_site();
    let engine = create_engine(Some(config())).expect("engine must build");
    assert_unreadable_body_error(test_name, scrape(&engine, &format!("{site}/")).await);
}

#[tokio::test]
async fn chromiumoxide_interact_fails_before_actions_when_the_seed_body_is_unreadable() {
    let test_name = "chromiumoxide_interact_fails_before_actions_when_the_seed_body_is_unreadable";
    let site = spawn_site();
    let engine = create_engine(Some(config())).expect("engine must build");
    assert_unreadable_interact_error(
        test_name,
        interact(&engine, &format!("{site}/broken"), Vec::new()).await,
    );
}

#[tokio::test]
async fn chromiumoxide_returns_a_malformed_encoded_terminal_redirect_with_an_empty_body() {
    let test_name = "chromiumoxide_returns_a_malformed_encoded_terminal_redirect_with_an_empty_body";
    let site = spawn_site();
    let engine = create_engine(Some(config())).expect("engine must build");
    match scrape(&engine, &format!("{site}/redirect-broken")).await {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Ok(page) => assert_eq!((page.status_code, page.html.as_str()), (302, ""), "{test_name}"),
        Err(error) => panic!("{test_name}: malformed redirect body must degrade to empty: {error:?}"),
    }
}
