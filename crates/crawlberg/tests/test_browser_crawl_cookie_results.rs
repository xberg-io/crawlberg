#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{BrowserConfig, BrowserMode, CrawlConfig, CrawlResult, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(mode: BrowserMode, enabled: bool) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            mode,
            timeout: Duration::from_secs(20),
            chrome_path: std::env::var_os("CRAWLBERG_TEST_CHROME").map(Into::into),
            ..BrowserConfig::default()
        },
        cookies_enabled: enabled,
        respect_robots_txt: false,
        max_concurrent: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

async fn site(script: &str) -> MockServer {
    cookie_site(script, "result=1; Path=/").await
}

async fn cookie_site(script: &str, set_cookie: &str) -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("set-cookie", set_cookie)
                .set_body_raw(
                    format!("<html><body><script>{script}</script><a href='/next'>next</a></body></html>"),
                    "text/html",
                ),
        )
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/next"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>end</body></html>", "text/html"))
        .mount(&site)
        .await;
    site
}

async fn run(site: &MockServer, config: CrawlConfig) -> CrawlResult {
    let engine = create_engine(Some(config)).expect("engine");
    let result = crawl(&engine, &format!("{}/", site.uri())).await.expect("crawl");
    assert_eq!(result.error, None);
    assert_eq!(result.pages.len(), 2);
    result
}

fn pairs(result: &CrawlResult) -> Vec<(String, String)> {
    let mut pairs: Vec<_> = result
        .cookies
        .iter()
        .map(|c| (c.name.clone(), c.value.clone()))
        .collect();
    pairs.sort();
    pairs
}

#[tokio::test]
async fn should_report_response_cookies_in_http_and_browser_crawls() {
    let site = site("").await;
    for mode in [BrowserMode::Never, BrowserMode::Always] {
        assert_eq!(
            pairs(&run(&site, config(mode, true)).await),
            vec![("result".into(), "1".into())]
        );
    }
}

#[tokio::test]
async fn should_report_javascript_cookies_and_leave_disabled_results_empty() {
    let site = site("document.cookie='script=2; Path=/'").await;
    assert_eq!(
        pairs(&run(&site, config(BrowserMode::Always, true)).await),
        vec![("result".into(), "1".into()), ("script".into(), "2".into())]
    );
    assert_eq!(pairs(&run(&site, config(BrowserMode::Always, false)).await), Vec::new());
}

#[tokio::test]
async fn should_exclude_external_baseline_and_report_changed_and_javascript_cookies() {
    use chromiumoxide::cdp::browser_protocol::network::CookieParam;
    use chromiumoxide::cdp::browser_protocol::storage::{GetCookiesParams, SetCookiesParams};
    use futures::StreamExt as _;

    let site = site("document.cookie='changed=after; Path=/'; document.cookie='script=2; Path=/'").await;
    let seed = format!("{}/", site.uri());
    let profile = tempfile::tempdir().expect("fresh profile");
    let mut launch = chromiumoxide::browser::BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(profile.path());
    if let Some(chrome) = std::env::var_os("CRAWLBERG_TEST_CHROME") {
        launch = launch.chrome_executable(chrome);
    }
    let (mut browser, mut handler) = chromiumoxide::Browser::launch(launch.build().expect("config"))
        .await
        .expect("real Chrome");
    let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
    browser
        .execute(SetCookiesParams {
            cookies: ["owner", "changed"]
                .into_iter()
                .map(|name| CookieParam {
                    url: Some(seed.clone()),
                    path: Some("/".into()),
                    ..CookieParam::new(name, "before")
                })
                .collect(),
            browser_context_id: None,
        })
        .await
        .expect("baseline cookies");
    let mut options = config(BrowserMode::Always, true);
    options.browser.endpoint = Some(browser.websocket_address().clone());
    options.browser.chrome_path = None;
    let engine = create_engine(Some(options)).expect("engine");
    let result = crawl(&engine, &seed).await;
    let requests = site.received_requests().await.expect("request recording");
    let sent = requests
        .iter()
        .find(|request| request.url.path() == "/")
        .and_then(|request| request.headers.get("cookie"))
        .and_then(|header| header.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let owned = browser
        .execute(GetCookiesParams::default())
        .await
        .expect("external jar");
    shutdown(&mut browser, handler).await;
    let result = result.expect("crawl");
    assert_eq!(result.error, None);
    assert_eq!(result.pages.len(), 2);
    assert!(
        sent.contains("owner=before") && sent.contains("changed=before"),
        "baseline cookies must reach the navigation"
    );
    assert_eq!(
        pairs(&result),
        vec![
            ("changed".into(), "after".into()),
            ("result".into(), "1".into()),
            ("script".into(), "2".into())
        ]
    );
    let mut owned: Vec<_> = owned.result.cookies.into_iter().map(|c| (c.name, c.value)).collect();
    owned.sort();
    assert_eq!(
        owned,
        vec![("changed".into(), "before".into()), ("owner".into(), "before".into())]
    );
}

async fn shutdown(browser: &mut chromiumoxide::Browser, handler: tokio::task::JoinHandle<()>) {
    let deadline = Duration::from_secs(2);
    let closed = matches!(tokio::time::timeout(deadline, browser.close()).await, Ok(Ok(_)));
    let exited = closed && matches!(tokio::time::timeout(deadline, browser.wait()).await, Ok(Ok(_)));
    let killed = exited || matches!(tokio::time::timeout(deadline, browser.kill()).await, Ok(Some(Ok(()))));
    handler.abort();
    assert!(killed, "owned Chrome must terminate");
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn should_report_native_browser_cookies() {
    let site = site("document.cookie='script=2; Path=/'").await;
    let mut options = config(BrowserMode::Always, true);
    options.browser.backend = crawlberg::BrowserBackend::Native;
    options.browser.chrome_path = None;
    assert_eq!(
        pairs(&run(&site, options.clone()).await),
        vec![("result".into(), "1".into()), ("script".into(), "2".into())]
    );
    options.cookies_enabled = false;
    assert_eq!(pairs(&run(&site, options).await), Vec::new());
}

#[tokio::test]
async fn should_report_cookie_without_explicit_path_once_in_each_backend() {
    let site = cookie_site("", "result=1").await;
    let mut configurations = vec![config(BrowserMode::Never, true)];
    configurations.push(config(BrowserMode::Always, true));
    #[cfg(feature = "browser-native")]
    {
        let mut native = config(BrowserMode::Always, true);
        native.browser.backend = crawlberg::BrowserBackend::Native;
        native.browser.chrome_path = None;
        configurations.push(native);
    }
    for options in configurations {
        assert_eq!(pairs(&run(&site, options).await), vec![("result".into(), "1".into())]);
    }
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn should_keep_first_header_cookie_observation_when_native_javascript_rewrites_it() {
    let site = cookie_site("document.cookie='result=2'", "result=1").await;
    let mut options = config(BrowserMode::Always, true);
    options.browser.backend = crawlberg::BrowserBackend::Native;
    options.browser.chrome_path = None;
    assert_eq!(pairs(&run(&site, options).await), vec![("result".into(), "1".into())]);
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn should_report_native_cookie_with_case_sensitive_path_once() {
    let site = cookie_site("", "result=1; Path=/Folder").await;
    let mut options = config(BrowserMode::Always, true);
    options.browser.backend = crawlberg::BrowserBackend::Native;
    options.browser.chrome_path = None;
    let result = run(&site, options).await;
    assert_eq!(pairs(&result), vec![("result".into(), "1".into())]);
    assert_eq!(result.cookies[0].path.as_deref(), Some("/Folder"));
}
