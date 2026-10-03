use std::ffi::{OsStr, OsString};

use crawlberg::{CrawlConfig, create_engine, scrape};
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
            // SAFETY: this test is serial, and restores every variable before returning.
            unsafe { std::env::remove_var(name) };
        }
        // SAFETY: this test is serial, and restores HTTP_PROXY before returning.
        unsafe { std::env::set_var("HTTP_PROXY", proxy) };
        Self(saved)
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..) {
            match value {
                Some(value) => {
                    // SAFETY: this test is serial, and this restores the original value.
                    unsafe { std::env::set_var(name, value) };
                }
                None => {
                    // SAFETY: this test is serial, and this restores the variable's absence.
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
