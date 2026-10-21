#![allow(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

use std::sync::Arc;

use crawlberg::{BrowserMode, CrawlConfig, CrawlEvent, crawl_stream, create_engine};
use futures::StreamExt;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsAcceptor;

async fn tls_origin() -> (String, tokio::task::JoinHandle<()>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(
                include_bytes!("fixtures/self_signed/cert.der").to_vec(),
            )],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                include_bytes!("fixtures/self_signed/key.der").to_vec(),
            )),
        )
        .expect("TLS server config");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local listener");
    let url = format!("https://{}", listener.local_addr().expect("local address"));
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };
                loop {
                    let mut request = [0; 8192];
                    let Ok(count) = stream.read(&mut request).await else {
                        break;
                    };
                    if count == 0 {
                        break;
                    }
                    let robots = String::from_utf8_lossy(&request[..count]).starts_with("GET /robots.txt ");
                    let (kind, body) = if robots {
                        ("text/plain", "User-agent: *\nAllow: /\n")
                    } else if String::from_utf8_lossy(&request[..count]).starts_with("GET /module.js ") {
                        ("application/javascript", "export const message = 'Module loaded';")
                    } else if String::from_utf8_lossy(&request[..count]).starts_with("GET /data ") {
                        ("text/plain", "Fetch loaded")
                    } else {
                        ("text/html", "<html><body><h1>Secure page</h1></body></html>")
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
                        body.len()
                    );
                    if stream.write_all(response.as_bytes()).await.is_err() || stream.flush().await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (url, server)
}

#[tokio::test]
async fn tls_policy_covers_page_and_robots_and_isolates_cached_clients() {
    let (url, server) = tls_origin().await;
    for accept in [false, true, false] {
        for robots in [false, true] {
            let mut config: CrawlConfig = serde_json::from_value(serde_json::json!({
                "danger_accept_invalid_certs": accept,
                "respect_robots_txt": robots,
                "max_depth": 0,
                "max_pages": 1,
                "ssrf": {"deny_private": false}
            }))
            .expect("TLS option must deserialize");
            config.browser.mode = BrowserMode::Never;
            let handle = create_engine(Some(config)).expect("engine builds");
            let mut stream = crawl_stream(&handle, &url).await.expect("crawl starts");
            let mut pages = 0;
            let mut errors = Vec::new();
            while let Some(event) = stream.next().await {
                match event.expect("event") {
                    CrawlEvent::Page { result } => {
                        pages += 1;
                        assert!(result.markdown.expect("Markdown").content.contains("Secure page"));
                    }
                    CrawlEvent::Error { error, .. } => errors.push(error),
                    CrawlEvent::Complete { .. } => {}
                }
            }
            assert_eq!(
                pages,
                usize::from(accept),
                "accept={accept}, robots={robots}: {errors:?}"
            );
            assert_eq!(errors.len(), usize::from(!accept), "accept={accept}, robots={robots}");
        }
    }
    server.abort();
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn browser_tls_policy_covers_robots_and_resets_between_pool_clients() {
    use crawlberg::{BrowserBackend, BrowserConfig, BrowserPool, BrowserPoolConfig};
    let (url, server) = tls_origin().await;
    let pool = BrowserPool::new(BrowserPoolConfig {
        max_pages: 1,
        ..Default::default()
    });
    for pooled in [false, true] {
        for accept in [false, true, false] {
            for robots in [false, true] {
                let mut config = CrawlConfig::builder()
                    .allow_private_networks(true)
                    .danger_accept_invalid_certs(accept)
                    .respect_robots_txt(robots)
                    .max_depth(0)
                    .max_pages(1)
                    .build();
                config.browser = BrowserConfig {
                    backend: BrowserBackend::Chromiumoxide,
                    mode: BrowserMode::Always,
                    ..Default::default()
                };
                config.browser_pool = pooled.then(|| Arc::clone(&pool));
                let handle = create_engine(Some(config)).expect("engine builds");
                let mut stream = crawl_stream(&handle, &url).await.expect("crawl starts");
                let mut pages = 0;
                let mut errors = Vec::new();
                while let Some(event) = stream.next().await {
                    match event.expect("event") {
                        CrawlEvent::Page { result } => {
                            pages += 1;
                            assert!(result.markdown.expect("Markdown").content.contains("Secure page"));
                        }
                        CrawlEvent::Error { error, .. } => errors.push(error),
                        CrawlEvent::Complete { .. } => {}
                    }
                }
                assert_eq!(
                    pages,
                    usize::from(accept),
                    "pooled={pooled}, accept={accept}, robots={robots}: {errors:?}"
                );
                assert_eq!(
                    errors.len(),
                    usize::from(!accept),
                    "pooled={pooled}, accept={accept}, robots={robots}"
                );
            }
        }
    }
    pool.shutdown().await;
    server.abort();
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_tls_policy_covers_page_and_robots() {
    use crawlberg::{BrowserBackend, BrowserConfig};
    let (url, server) = tls_origin().await;
    for accept in [false, true, false] {
        for robots in [false, true] {
            let mut config = CrawlConfig::builder()
                .allow_private_networks(true)
                .danger_accept_invalid_certs(accept)
                .respect_robots_txt(robots)
                .max_depth(0)
                .max_pages(1)
                .build();
            config.browser = BrowserConfig {
                backend: BrowserBackend::Native,
                mode: BrowserMode::Always,
                ..Default::default()
            };
            let handle = create_engine(Some(config)).expect("engine builds");
            let mut stream = crawl_stream(&handle, &url).await.expect("crawl starts");
            let mut pages = 0;
            let mut errors = Vec::new();
            while let Some(event) = stream.next().await {
                match event.expect("event") {
                    CrawlEvent::Page { result } => {
                        pages += 1;
                        assert!(result.markdown.expect("Markdown").content.contains("Secure page"));
                    }
                    CrawlEvent::Error { error, .. } => errors.push(error),
                    CrawlEvent::Complete { .. } => {}
                }
            }
            assert_eq!(
                pages,
                usize::from(accept),
                "native accept={accept}, robots={robots}: {errors:?}"
            );
            assert_eq!(
                errors.len(),
                usize::from(!accept),
                "native accept={accept}, robots={robots}"
            );
        }
    }
    server.abort();
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_tls_policy_covers_script_fetch_and_module_import() {
    use crawlberg::{BrowserBackend, BrowserConfig, scrape};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let (tls_url, server) = tls_origin().await;
    let site = MockServer::start().await;
    let page = format!(
        r#"<html><body><p id="module-result">pending module</p><p id="fetch-result">pending fetch</p>
        <script>fetch('{tls_url}/data').then(r => r.text()).then(t => document.getElementById('fetch-result').textContent = t);</script>
        <script type="module">import {{ message }} from '{tls_url}/module.js'; document.getElementById('module-result').textContent = message;</script>
        </body></html>"#
    );
    Mock::given(wiremock::matchers::method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(page, "text/html"))
        .mount(&site)
        .await;
    for accept in [false, true] {
        let mut config = CrawlConfig::builder()
            .allow_private_networks(true)
            .danger_accept_invalid_certs(accept)
            .build();
        config.browser = BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            ..Default::default()
        };
        let handle = create_engine(Some(config)).expect("engine builds");
        let result = scrape(&handle, &site.uri()).await.expect("HTTP main page renders");
        assert_eq!(result.html.contains(">Fetch loaded<"), accept, "fetch: {}", result.html);
        assert_eq!(
            result.html.contains(">Module loaded<"),
            accept,
            "module: {}",
            result.html
        );
    }
    server.abort();
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn browser_sessions_isolate_mixed_tls_policies_sequentially_and_concurrently() {
    use crawlberg::{BrowserBackend, BrowserConfig, BrowserPool, BrowserPoolConfig, BrowserSessionPool, scrape};
    let (url, server) = tls_origin().await;
    let pool = BrowserPool::new(BrowserPoolConfig {
        max_pages: 2,
        ..Default::default()
    });
    let sessions = Arc::new(BrowserSessionPool::new());
    let engine = |accept| {
        let mut config = CrawlConfig::builder()
            .allow_private_networks(true)
            .danger_accept_invalid_certs(accept)
            .build();
        config.browser = BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            session_affinity: true,
            ..Default::default()
        };
        config.browser_pool = Some(Arc::clone(&pool));
        config.browser_session_pool = Some(Arc::clone(&sessions));
        create_engine(Some(config)).expect("engine builds")
    };
    let permissive = engine(true);
    let strict = engine(false);
    for accept in [false, true, false, true] {
        let result = scrape(if accept { &permissive } else { &strict }, &url).await;
        if accept {
            assert!(result.expect("permissive page").html.contains("Secure page"));
            assert!(sessions.size().await > 0, "the browser page must be parked for reuse");
        } else {
            let error = result.expect_err("strict policy must reject the self-signed certificate");
            let message = error.to_string().to_ascii_lowercase();
            assert!(
                message.contains("cert")
                    || message.contains("ssl")
                    || error.to_string()
                        == format!("browser: Chrome could not load {url}/ and showed its own error page"),
                "{error}"
            );
        }
    }
    let (accepted, rejected) = tokio::join!(scrape(&permissive, &url), scrape(&strict, &url));
    assert!(accepted.expect("permissive page").html.contains("Secure page"));
    let error = rejected.expect_err("strict concurrent page rejects the certificate");
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("cert")
            || message.contains("ssl")
            || error.to_string() == format!("browser: Chrome could not load {url}/ and showed its own error page"),
        "{error}"
    );
    pool.shutdown().await;
    server.abort();
}
