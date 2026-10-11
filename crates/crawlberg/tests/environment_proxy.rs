#![allow(
    unsafe_code,
    reason = "all tests in this binary share one serial key and restore every mutated environment variable"
)]

use std::ffi::{OsStr, OsString};
use std::sync::{Arc, Mutex};

use crawlberg::{CrawlConfig, HostMatcher, create_engine, scrape};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PROXY_ENV: [&str; 9] = [
    "ALL_PROXY",
    "all_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "REQUEST_METHOD",
];

struct EnvironmentGuard(Vec<(&'static str, Option<OsString>)>);

impl EnvironmentGuard {
    fn isolated_http_proxy(proxy: impl AsRef<OsStr>) -> Self {
        let saved = PROXY_ENV
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect();
        for name in PROXY_ENV {
            // SAFETY: ~keep every test in this binary shares one serial key, and the guard restores every variable.
            unsafe { std::env::remove_var(name) };
        }
        let guard = Self(saved);
        guard.set_http_proxy(proxy);
        guard
    }

    fn set_http_proxy(&self, proxy: impl AsRef<OsStr>) {
        // SAFETY: ~keep every test in this binary shares one serial key, and the guard restores HTTP_PROXY.
        unsafe { std::env::set_var("HTTP_PROXY", proxy) };
    }

    fn set_no_proxy(&self, no_proxy: impl AsRef<OsStr>) {
        // SAFETY: ~keep every test in this binary shares one serial key, and the guard restores NO_PROXY.
        unsafe { std::env::set_var("NO_PROXY", no_proxy) };
    }
}

async fn recording_server(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("server must bind");
    let address = format!(
        "http://localhost:{}",
        listener.local_addr().expect("server address").port()
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0; 4096];
            let read = socket.read(&mut request).await.unwrap_or(0);
            log.lock()
                .expect("request log")
                .push(String::from_utf8_lossy(&request[..read]).to_string());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (address, requests)
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..) {
            match value {
                Some(value) => {
                    // SAFETY: ~keep every test in this binary shares one serial key; this restores the original value.
                    unsafe { std::env::set_var(name, value) };
                }
                None => {
                    // SAFETY: ~keep every test in this binary shares one serial key; this restores absence.
                    unsafe { std::env::remove_var(name) };
                }
            }
        }
    }
}

#[tokio::test]
#[serial_test::serial(environment_proxy)]
async fn environment_proxy_on_a_private_host_is_admitted_like_an_explicit_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy must bind");
    let proxy_url = format!(
        "http://localhost:{}",
        listener.local_addr().expect("proxy address").port()
    );
    let _environment = EnvironmentGuard::isolated_http_proxy(proxy_url);
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("client must reach proxy");
        let mut request = vec![0; 4096];
        let read = socket.read(&mut request).await.expect("proxy must read request");
        let first_line = String::from_utf8_lossy(&request[..read])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let body = "<html><body><p>served through environment proxy</p></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.expect("proxy must answer");
        first_line
    });

    let config = CrawlConfig {
        ssrf_deny_private_explicit: Some(true),
        ..CrawlConfig::default()
    };
    let engine = create_engine(Some(config)).expect("engine must build");
    let result = scrape(&engine, "http://1.1.1.1/proxy-check")
        .await
        .expect("environment proxy must be usable under the default SSRF policy");
    let first_line = proxy.await.expect("proxy task must finish");

    assert_eq!(result.status_code, 200);
    assert_eq!(first_line, "GET http://1.1.1.1/proxy-check HTTP/1.1");
}

#[tokio::test]
#[serial_test::serial(environment_proxy)]
async fn one_engine_tracks_environment_proxy_and_credential_changes() {
    let (first_proxy, first_requests) = recording_server("<html>first</html>").await;
    let (second_proxy, second_requests) = recording_server("<html>second</html>").await;
    let environment = EnvironmentGuard::isolated_http_proxy(&first_proxy);
    let config = CrawlConfig {
        ssrf_deny_private_explicit: Some(true),
        ..CrawlConfig::default()
    };
    let engine = create_engine(Some(config)).expect("engine must build");

    scrape(&engine, "http://1.1.1.1/first")
        .await
        .expect("the first proxy must answer");
    environment.set_http_proxy(&second_proxy);
    scrape(&engine, "http://1.1.1.1/second")
        .await
        .expect("the changed proxy must answer");

    let mut credentialed = url::Url::parse(&second_proxy).expect("proxy URL");
    credentialed.set_username("operator").expect("proxy URL takes username");
    credentialed
        .set_password(Some("first-password"))
        .expect("proxy URL takes password");
    environment.set_http_proxy(credentialed.as_str());
    scrape(&engine, "http://1.1.1.1/credential-one")
        .await
        .expect("the credentialed proxy must answer");
    credentialed
        .set_password(Some("second-password"))
        .expect("proxy URL takes password");
    environment.set_http_proxy(credentialed.as_str());
    scrape(&engine, "http://1.1.1.1/credential-two")
        .await
        .expect("the changed credentials must be used");

    assert_eq!(first_requests.lock().expect("request log").len(), 1);
    let second_requests = second_requests.lock().expect("request log");
    assert_eq!(second_requests.len(), 3);
    let authorization = |request: &str| {
        request
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("proxy-authorization:"))
            .map(str::to_owned)
    };
    assert!(authorization(&second_requests[0]).is_none());
    let first = authorization(&second_requests[1]).expect("first credentials must be sent");
    let second = authorization(&second_requests[2]).expect("second credentials must be sent");
    assert_ne!(
        first, second,
        "changed credentials must select a different cached client"
    );
}

#[tokio::test]
#[serial_test::serial(environment_proxy)]
async fn no_proxy_routes_an_allowlisted_private_target_directly() {
    let (target, target_requests) = recording_server("<html>direct</html>").await;
    let (proxy, proxy_requests) = recording_server("<html>proxy</html>").await;
    let environment = EnvironmentGuard::isolated_http_proxy(proxy);
    environment.set_no_proxy("localhost");
    let mut config = CrawlConfig::default();
    config.ssrf.allowlist.push(HostMatcher::exact("localhost"));
    config.ssrf_deny_private_explicit = Some(true);
    let engine = create_engine(Some(config)).expect("engine must build");

    let result = scrape(&engine, &format!("{target}/direct"))
        .await
        .expect("the allowlisted direct target must answer");

    assert_eq!(result.status_code, 200);
    assert_eq!(target_requests.lock().expect("request log").len(), 1);
    assert!(proxy_requests.lock().expect("request log").is_empty());
}

/// A proxy that answers `/start` with a redirect to `/landed` and the cookie `hop=1`, and every
/// other request with a page. It records each request.
async fn redirecting_proxy() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy must bind");
    let address = format!(
        "http://localhost:{}",
        listener.local_addr().expect("proxy address").port()
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0; 4096];
            let read = socket.read(&mut request).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]).to_string();
            let response = if request.starts_with("GET http://1.1.1.1/start ") {
                "HTTP/1.1 302 Found\r\nLocation: http://1.1.1.1/landed\r\nSet-Cookie: hop=1; Path=/\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned()
            } else {
                let body = "<html><body><p>landed</p></body></html>";
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            log.lock().expect("request log").push(request);
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (address, requests)
}

/// ~keep The client for an environment proxy must not follow a redirect itself: the engine sends
/// ~keep each hop, so the cookie that the first hop sets is in the request of the second hop.
#[tokio::test]
#[serial_test::serial(environment_proxy)]
async fn a_redirect_through_an_environment_proxy_sends_the_cookie_of_the_first_hop() {
    let (proxy, requests) = redirecting_proxy().await;
    let _environment = EnvironmentGuard::isolated_http_proxy(proxy);
    let config = CrawlConfig {
        cookies_enabled: true,
        ssrf_deny_private_explicit: Some(true),
        ..CrawlConfig::default()
    };
    let engine = create_engine(Some(config)).expect("engine must build");

    let result = scrape(&engine, "http://1.1.1.1/start")
        .await
        .expect("the redirect must be followed through the environment proxy");

    assert_eq!(result.status_code, 200);
    let requests = requests.lock().expect("request log");
    let landed = requests
        .iter()
        .find(|request| request.starts_with("GET http://1.1.1.1/landed "))
        .expect("the second hop must go through the environment proxy");
    let cookie = landed
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("cookie:"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| value.trim().to_owned());
    assert_eq!(cookie.as_deref(), Some("hop=1"));
}
