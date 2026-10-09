#![cfg(all(unix, feature = "browser-chromiumoxide"))]
#![allow(unsafe_code)]

use chromiumoxide::Browser;
use chromiumoxide::browser::BrowserConfig;
use futures::{SinkExt, StreamExt};

mod common;

fn test_chrome() -> Option<std::path::PathBuf> {
    std::env::var_os("CHROME").map(Into::into).or_else(|| {
        chromiumoxide::detection::default_executable(Default::default())
            .map_err(|error| {
                common::announce_chrome_skip("private browser pipe", &error.to_string());
            })
            .ok()
    })
}

#[tokio::test]
async fn launched_browser_uses_a_private_pipe_without_a_debugging_port() {
    let Some(chrome) = test_chrome() else {
        return;
    };
    let profile = tempfile::tempdir().expect("temporary profile");
    let config = BrowserConfig::builder()
        .chrome_executable(chrome)
        .user_data_dir(profile.path())
        .build()
        .expect("browser config");
    let (mut browser, mut handler) = Browser::launch(config).await.expect("browser launch");
    let driver = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let mut independent = browser.raw_connection().await.expect("independent private client");
    independent
        .send(async_tungstenite::tungstenite::Message::Text(
            r#"{"id":0,"method":"Browser.getVersion","params":{}}"#.into(),
        ))
        .await
        .expect("independent command");
    let page = browser.new_page("about:blank").await.expect("page over CDP");
    let value: u64 = page
        .evaluate("21 * 2")
        .await
        .expect("CDP evaluation")
        .into_value()
        .expect("value");
    assert_eq!(value, 42);
    let response = independent
        .next()
        .await
        .expect("independent response")
        .expect("valid response");
    let response: serde_json::Value =
        serde_json::from_str(response.to_text().expect("text response")).expect("CDP JSON");
    assert_eq!(response["id"], 0);
    assert!(
        response["result"]["product"]
            .as_str()
            .is_some_and(|product| product.contains("Chrome"))
    );
    assert_eq!(browser.websocket_address(), "pipe");
    assert!(!profile.path().join("DevToolsActivePort").exists());
    let control = std::net::TcpListener::bind("127.0.0.1:0").expect("positive socket control");
    let control_port = control.local_addr().expect("control address").port();
    let positive = std::process::Command::new("lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &std::process::id().to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
        ])
        .output()
        .expect("lsof control");
    assert!(String::from_utf8_lossy(&positive.stdout).contains(&format!(":{control_port}")));
    drop(control);
    {
        let child = browser.get_mut_child().expect("launched browser child");
        let pid = child.as_mut_inner().id().expect("browser pid");
        let sockets = std::process::Command::new("lsof")
            .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
            .output()
            .expect("lsof must be installed to verify the listening sockets");
        assert!(
            sockets.stdout.is_empty(),
            "Chrome must have no listening TCP socket: {}",
            String::from_utf8_lossy(&sockets.stdout)
        );
    }
    let _ = browser.close().await;
    let _ = browser.wait().await;
    driver.abort();
}

#[tokio::test]
async fn browser_exits_when_its_caller_is_killed() {
    let Some(chrome) = test_chrome() else {
        return;
    };
    for signal in [libc::SIGKILL, libc::SIGTERM] {
        assert_browser_exits_after_signal(&chrome, signal).await;
    }
}

async fn assert_browser_exits_after_signal(chrome: &std::path::Path, signal: i32) {
    let marker_dir = tempfile::tempdir().expect("marker directory");
    let marker = marker_dir.path().join("browser.pid");
    let mut caller = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--ignored", "--exact", "pipe_owner_helper"])
        .env("CRAWLBERG_PIPE_OWNER_PID", &marker)
        .env("CHROME", chrome)
        .spawn()
        .expect("caller process");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !marker.exists() {
        if caller.try_wait().expect("caller status").is_some() || std::time::Instant::now() >= deadline {
            let _ = caller.kill();
            let _ = caller.wait();
            panic!("caller must launch Chrome before the death test");
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let browser_pid: i32 = std::fs::read_to_string(marker)
        .expect("browser pid")
        .parse()
        .expect("numeric pid");
    // SAFETY: the pid belongs to the live subprocess created and retained by this test. ~keep
    assert_eq!(
        unsafe { libc::kill(i32::try_from(caller.id()).expect("pid fits"), signal) },
        0
    );
    caller.wait().expect("reap caller");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        // SAFETY: signal zero observes the specific child pid without delivering a signal.
        if unsafe { libc::kill(browser_pid, 0) } == -1 {
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
            break;
        }
        if std::time::Instant::now() >= deadline {
            // SAFETY: clean up only the browser launched by this test's child process.
            unsafe { libc::kill(browser_pid, libc::SIGKILL) };
            panic!("Chrome {browser_pid} outlived its killed caller");
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[tokio::test]
#[ignore = "subprocess helper invoked by browser_exits_when_its_caller_is_killed"]
async fn pipe_owner_helper() {
    let marker = std::env::var_os("CRAWLBERG_PIPE_OWNER_PID").expect("parent marker");
    let chrome = std::env::var("CHROME").expect("CHROME");
    let profile = std::path::Path::new(&marker)
        .parent()
        .expect("marker parent")
        .join("profile");
    let config = BrowserConfig::builder()
        .chrome_executable(chrome)
        .user_data_dir(&profile)
        .build()
        .expect("config");
    let (mut browser, mut handler) = Browser::launch(config).await.expect("launch");
    let driver = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let page = browser.new_page("about:blank").await.expect("ready browser");
    let pid = browser
        .get_mut_child()
        .expect("launched child")
        .as_mut_inner()
        .id()
        .expect("pid");
    std::fs::write(marker, pid.to_string()).expect("publish browser pid");
    let _keep_alive = (browser, driver, page, profile);
    std::future::pending::<()>().await;
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn browser_firewall_keeps_child_requests_guarded_over_the_private_pipe() {
    use crawlberg::{BrowserMode, CrawlConfig, HostMatcher, create_engine, scrape};
    let Some(chrome) = test_chrome() else {
        return;
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let allowed = server.uri().replace("127.0.0.1", "localhost");
    let denied = server.uri();
    let body = format!(
        "<html><body><h1>Private transport</h1><iframe src='{allowed}/frame'></iframe>\
         <iframe src='{denied}/denied'></iframe></body></html>",
    );
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/frame"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>Child frame</body></html>", "text/html"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/denied"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("DENIED", "text/html"))
        .expect(0)
        .mount(&server)
        .await;
    let mut config = CrawlConfig::default();
    config.ssrf.allowlist = vec![HostMatcher::exact("localhost")];
    config.respect_robots_txt = false;
    config.browser.mode = BrowserMode::Always;
    config.browser.chrome_path = Some(chrome);
    let engine = create_engine(Some(config)).expect("engine");
    let page = scrape(&engine, &format!("{}/", server.uri().replace("127.0.0.1", "localhost")))
        .await
        .expect("protected scrape");
    assert!(page.html.contains("Private transport"));
    assert!(page.ssrf_refused_urls.contains(&format!("{denied}/denied")));
    drop(engine);
}
