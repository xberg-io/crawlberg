//! Chrome-backed integration tests proving `BrowserConfig.chrome_path` and
//! `BrowserConfig.chrome_args` reach the Chrome process that a one-shot
//! (non-pooled) browser-mode scrape launches, which is the path every language
//! binding uses.
//!
//! Gated behind the `browser` feature and skipped loudly when this machine has no
//! usable Chrome, like the other browser integration tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// A browser-mode config that reaches the loopback test server, with the given launch options.
fn browser_config(chrome_path: Option<std::path::PathBuf>, chrome_args: Vec<String>) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            chrome_path,
            chrome_args,
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// Serves one page whose body echoes the `User-Agent` header of the request, so a test can
/// read what the launched Chrome sent.
async fn start_user_agent_echo_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = [0_u8; 8192];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let header = |wanted: &str| {
                    request
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case(wanted).then(|| value.trim().to_owned())
                        })
                        .unwrap_or_default()
                };
                let body = format!(
                    "<html><body><p>launch-options-page ua=[{}] lang=[{}]</p></body></html>",
                    header("user-agent"),
                    header("accept-language")
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://{addr}/")
}

/// A flag from `chrome_args` reaches the Chrome process that renders the page.
#[tokio::test]
async fn a_chrome_args_flag_reaches_the_chrome_that_renders_the_page() {
    let url = start_user_agent_echo_server().await;
    let config = browser_config(None, vec!["--user-agent=crawlberg-chrome-args-marker".to_owned()]);
    let engine = create_engine(Some(config)).expect("engine must build");
    let result = scrape(&engine, &url).await;
    if let Err(CrawlError::BrowserError { message, .. }) = &result
        && is_missing_chrome_message(message)
    {
        announce_chrome_skip("a_chrome_args_flag_reaches_the_chrome_that_renders_the_page", message);
        return;
    }
    let html = result.expect("scrape must succeed").html;
    assert!(
        html.contains("ua=[crawlberg-chrome-args-marker]"),
        "the page must have been requested with the --user-agent from chrome_args: {html}"
    );
}

/// `chrome_path` names the binary crawlberg launches, and a `chrome_args` flag that names a
/// crawlberg default replaces it on the real command line: a wrapper script records its own
/// arguments and then runs the real Chrome, which must render the page.
#[cfg(unix)]
#[tokio::test]
async fn chrome_path_is_launched_with_the_caller_flag_in_place_of_the_default() {
    use std::os::unix::fs::PermissionsExt;

    let test_name = "chrome_path_is_launched_with_the_caller_flag_in_place_of_the_default";
    let real_chrome = match chromiumoxide::detection::default_executable(Default::default()) {
        Ok(path) => path,
        Err(message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
    };
    let dir = std::env::temp_dir().join(format!("crawlberg-chrome-path-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir must be creatable");
    let argv_file = dir.join("wrapper-argv");
    let wrapper = dir.join("chrome-wrapper.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexec '{}' \"$@\"\n",
            argv_file.display(),
            real_chrome.display()
        ),
    )
    .expect("wrapper must be writable");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("wrapper must be chmod-able");

    let url = start_user_agent_echo_server().await;
    let config = browser_config(
        Some(wrapper.clone()),
        vec![
            "--user-agent=crawlberg-wrapper-marker".to_owned(),
            "--lang=fr".to_owned(),
        ],
    );
    let engine = create_engine(Some(config)).expect("engine must build");
    let result = scrape(&engine, &url).await;
    let argv = std::fs::read_to_string(&argv_file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);

    let html = result
        .expect("scrape through the chrome_path wrapper must succeed")
        .html;
    assert!(
        html.contains("ua=[crawlberg-wrapper-marker]"),
        "the configured chrome_path must be the Chrome that rendered the page: {html}"
    );
    let argv: Vec<&str> = argv.lines().collect();
    assert!(
        argv.contains(&"--lang=fr"),
        "the caller's --lang must reach Chrome: {argv:?}"
    );
    assert!(
        !argv.contains(&"--lang=en_US"),
        "the caller's --lang must replace crawlberg's default --lang: {argv:?}"
    );
    assert!(
        argv.contains(&"--disable-sync"),
        "defaults the caller did not name must still reach Chrome: {argv:?}"
    );
}

/// A `chrome_path` that does not exist refuses the config and names the path; crawlberg
/// never falls back to another Chrome.
#[tokio::test]
async fn a_missing_chrome_path_is_refused_and_named() {
    let config = browser_config(
        Some(std::path::PathBuf::from("/nonexistent/crawlberg-integration-chrome")),
        Vec::new(),
    );
    let error = match create_engine(Some(config)) {
        Ok(_) => panic!("a missing chrome_path must refuse the config"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("/nonexistent/crawlberg-integration-chrome"),
        "the error must name the path, got: {error}"
    );
}
