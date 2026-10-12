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
        .private_pipe(true)
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
        .private_pipe(true)
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
    config.browser.private_pipe = true;
    let engine = create_engine(Some(config)).expect("engine");
    let page = scrape(&engine, &format!("{}/", server.uri().replace("127.0.0.1", "localhost")))
        .await
        .expect("protected scrape");
    assert!(page.html.contains("Private transport"));
    assert!(page.ssrf_refused_urls.contains(&format!("{denied}/denied")));
    drop(engine);
}

/// The Chrome that an engine starts for a crawl, read from the process table by the directory of
/// its profile. A process of its own runs the crawl, so that a test can end that process.
#[cfg(feature = "browser")]
mod engine_crawl {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::test_chrome;

    const HELPER: &str = "engine_crawl::crawl_owner_helper";
    const MARKER: &str = "CRAWLBERG_PIPE_CRAWL_MARKER";
    /// Set for the crawl process when its config turns `browser.private_pipe` on.
    const PRIVATE_PIPE: &str = "CRAWLBERG_PIPE_CRAWL_PRIVATE_PIPE";

    /// The processes whose command line names `dir`, each with its command line.
    fn processes_naming(dir: &Path) -> Vec<(i32, String)> {
        let table = std::process::Command::new("ps")
            .args(["-A", "-ww", "-o", "pid=", "-o", "args="])
            .output()
            .expect("ps must be installed to read the process table");
        assert!(table.status.success(), "ps must list the processes");
        let dir = dir.to_str().expect("a UTF-8 directory");
        String::from_utf8_lossy(&table.stdout)
            .lines()
            .filter(|line| line.contains(dir))
            .filter_map(|line| {
                let (pid, args) = line.trim_start().split_once(' ')?;
                Some((pid.parse().ok()?, args.trim_start().to_owned()))
            })
            .collect()
    }

    /// The TCP sockets that the process `pid` listens on, as `lsof` prints them.
    fn listening_sockets(pid: i32) -> String {
        let sockets = std::process::Command::new("lsof")
            .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
            .output()
            .expect("lsof must be installed to read the listening sockets");
        String::from_utf8_lossy(&sockets.stdout).into_owned()
    }

    /// Stop the processes that still name `dir`. Each pid comes from the process table at this call.
    fn stop_processes_naming(dir: &Path) {
        for (pid, _) in processes_naming(dir) {
            // SAFETY: the pid names a process that runs on the directory this test created. ~keep
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }

    /// Start a process that scrapes one page in browser mode and then keeps its engine. Its
    /// config has `browser.private_pipe` as given and every other key at its default. The
    /// profile of its Chrome is under `scratch`. Return when the scrape is done.
    async fn start_crawl_owner(chrome: &Path, scratch: &Path, private_pipe: bool) -> std::process::Child {
        let marker = scratch.join("crawled");
        let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--ignored", "--exact", HELPER])
            .env(MARKER, &marker)
            .env("CHROME", chrome)
            .env("TMPDIR", scratch)
            .env_remove(PRIVATE_PIPE);
        if private_pipe {
            command.env(PRIVATE_PIPE, "1");
        }
        let mut owner = command.spawn().expect("crawl process");
        let deadline = Instant::now() + Duration::from_secs(120);
        while !marker.exists() {
            if owner.try_wait().expect("crawl process status").is_some() || Instant::now() >= deadline {
                let _ = owner.kill();
                let _ = owner.wait();
                stop_processes_naming(scratch);
                panic!("the crawl process must finish one scrape in browser mode");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        owner
    }

    #[tokio::test]
    #[ignore = "subprocess helper of the tests that read the Chrome of a crawl"]
    async fn crawl_owner_helper() {
        use crawlberg::{BrowserMode, CrawlConfig, HostMatcher, create_engine, scrape};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let marker = std::env::var_os(MARKER).expect("marker path");
        let chrome = std::env::var_os("CHROME").expect("CHROME");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("<html><body><h1>Owned</h1></body></html>", "text/html"),
            )
            .mount(&server)
            .await;
        let mut config = CrawlConfig::default();
        config.ssrf.allowlist = vec![HostMatcher::exact("localhost")];
        config.respect_robots_txt = false;
        config.browser.mode = BrowserMode::Always;
        config.browser.chrome_path = Some(chrome.into());
        config.browser.private_pipe = std::env::var_os(PRIVATE_PIPE).is_some();
        let engine = create_engine(Some(config)).expect("engine");
        let page = scrape(&engine, &format!("{}/", server.uri().replace("127.0.0.1", "localhost")))
            .await
            .expect("scrape in browser mode");
        assert!(page.html.contains("Owned"));
        std::fs::write(marker, "done").expect("publish the marker");
        let _keep_alive = (engine, server);
        std::future::pending::<()>().await;
    }

    /// The default: the key is off, so the Chrome of a crawl gets a debugging port, as before the
    /// key existed.
    #[tokio::test]
    async fn the_chrome_of_a_default_crawl_has_a_debugging_port_and_no_pipe() {
        let Some(chrome) = test_chrome() else {
            return;
        };
        let scratch = tempfile::tempdir().expect("scratch directory");
        let root = scratch.path().canonicalize().expect("scratch path");
        let mut owner = start_crawl_owner(&chrome, &root, false).await;
        let processes = processes_naming(&root);
        let _ = owner.kill();
        let _ = owner.wait();
        stop_processes_naming(&root);

        let profile_flag = format!("--user-data-dir={}", root.display());
        let browsers: Vec<&String> = processes
            .iter()
            .map(|(_, args)| args)
            .filter(|args| args.contains(&profile_flag) && !args.contains("--type="))
            .collect();
        assert!(
            !browsers.is_empty(),
            "the process table must show the Chrome of the crawl by its profile directory: {processes:?}"
        );
        for args in browsers {
            assert!(
                args.contains("--remote-debugging-port=0"),
                "the Chrome of a default crawl must get a debugging port: {args}"
            );
            assert!(
                !args.contains("--remote-debugging-pipe"),
                "the Chrome of a default crawl must not get the pipe: {args}"
            );
        }
    }

    #[tokio::test]
    async fn the_chrome_of_a_crawl_with_the_key_on_has_no_debugging_port_and_no_listening_socket() {
        let Some(chrome) = test_chrome() else {
            return;
        };
        let control = std::net::TcpListener::bind("127.0.0.1:0").expect("positive socket control");
        let control_port = control.local_addr().expect("control address").port();
        let own_pid = i32::try_from(std::process::id()).expect("pid fits");
        assert!(
            listening_sockets(own_pid).contains(&format!(":{control_port}")),
            "lsof must show a listening socket that exists"
        );
        drop(control);

        let scratch = tempfile::tempdir().expect("scratch directory");
        let root = scratch.path().canonicalize().expect("scratch path");
        let mut owner = start_crawl_owner(&chrome, &root, true).await;
        let processes = processes_naming(&root);
        let listening: Vec<String> = processes
            .iter()
            .map(|(pid, _)| listening_sockets(*pid))
            .filter(|sockets| !sockets.is_empty())
            .collect();
        let _ = owner.kill();
        let _ = owner.wait();
        stop_processes_naming(&root);

        let profile_flag = format!("--user-data-dir={}", root.display());
        let browsers: Vec<&String> = processes
            .iter()
            .map(|(_, args)| args)
            .filter(|args| args.contains(&profile_flag) && !args.contains("--type="))
            .collect();
        assert!(
            !browsers.is_empty(),
            "the process table must show the Chrome of the crawl by its profile directory: {processes:?}"
        );
        for args in browsers {
            assert!(
                args.contains("--remote-debugging-pipe"),
                "the Chrome of a crawl must speak the protocol over a pipe: {args}"
            );
        }
        for (_, args) in &processes {
            assert!(
                !args.contains("--remote-debugging-port"),
                "no process of the crawl gets a debugging port: {args}"
            );
        }
        assert!(
            listening.is_empty(),
            "no process of the crawl listens on a TCP port: {listening:?}"
        );
    }

    #[tokio::test]
    async fn no_chrome_of_a_crawl_with_the_key_on_is_left_after_its_process_is_killed() {
        let Some(chrome) = test_chrome() else {
            return;
        };
        for signal in [libc::SIGKILL, libc::SIGTERM] {
            let scratch = tempfile::tempdir().expect("scratch directory");
            let root = scratch.path().canonicalize().expect("scratch path");
            let mut owner = start_crawl_owner(&chrome, &root, true).await;
            let started = processes_naming(&root);
            // SAFETY: the pid is the one of the live crawl process that this test started. ~keep
            let sent = unsafe { libc::kill(i32::try_from(owner.id()).expect("pid fits"), signal) };
            owner.wait().expect("reap the crawl process");
            assert_eq!(sent, 0, "the crawl process must get signal {signal}");
            assert!(
                !started.is_empty(),
                "the process table must show the Chrome of the crawl by its profile directory"
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let left = processes_naming(&root);
                if left.is_empty() {
                    break;
                }
                if Instant::now() >= deadline {
                    stop_processes_naming(&root);
                    panic!("signal {signal} ended the crawl process, and 10 s later its Chrome still runs: {left:?}");
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
}
