#[cfg(feature = "stealth")]
use std::collections::HashMap;
#[cfg(feature = "stealth")]
use std::error::Error;
#[cfg(feature = "stealth")]
use std::sync::Arc;
#[cfg(feature = "stealth")]
use std::time::Duration;

#[cfg(feature = "stealth")]
use tokio::sync::RwLock;
#[cfg(feature = "stealth")]
use url::Url;

#[cfg(feature = "stealth")]
use super::client::{NetError, Response};
#[cfg(feature = "stealth")]
use crate::net::cookies::CookieJar;
#[cfg(feature = "stealth")]
use crate::net::ssrf::{DefaultSsrfValidator, SsrfValidator};

#[cfg(feature = "stealth")]
pub const STEALTH_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";

#[cfg(feature = "stealth")]
pub struct StealthHttpClient {
    client: wreq::Client,
    /// SSRF policy applied to the initial URL and every redirect hop.
    pub ssrf: Arc<dyn SsrfValidator>,
    pub cookie_jar: Arc<CookieJar>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
}

#[cfg(feature = "stealth")]
impl StealthHttpClient {
    pub fn new(cookie_jar: Arc<CookieJar>) -> Self {
        Self::build(cookie_jar, None, Arc::new(DefaultSsrfValidator::from_env()))
    }

    /// Build a stealth client that sends every request through `proxy_url`, if given.
    ///
    /// Fails with [`NetError::InvalidProxy`] when the proxy cannot be used, rather than
    /// building a client that silently connects directly.
    pub fn with_proxy(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Result<Self, NetError> {
        Self::with_ssrf(cookie_jar, proxy_url, Arc::new(DefaultSsrfValidator::from_env()))
    }

    /// Build a stealth client with an explicit SSRF policy.
    ///
    /// Fails with [`NetError::InvalidProxy`] when the proxy cannot be used.
    pub fn with_ssrf(
        cookie_jar: Arc<CookieJar>,
        proxy_url: Option<&str>,
        ssrf: Arc<dyn SsrfValidator>,
    ) -> Result<Self, NetError> {
        let proxy = proxy_url.map(crate::net::proxy::wreq_proxy).transpose()?;
        Ok(Self::build(cookie_jar, proxy, ssrf))
    }

    fn build(cookie_jar: Arc<CookieJar>, proxy: Option<wreq::Proxy>, ssrf: Arc<dyn SsrfValidator>) -> Self {
        let cert_store = wreq::tls::trust::CertStore::builder()
            .set_default_paths()
            .build()
            .expect("Failed to load system CA certificates");

        let emulation_opts = wreq_util::Emulation::builder()
            .profile(wreq_util::Profile::Chrome145)
            .platform(wreq_util::Platform::Linux)
            .build();

        let mut builder = wreq::Client::builder()
            .emulation(emulation_opts)
            .tls_cert_store(cert_store)
            .timeout(Duration::from_secs(30))
            .redirect(wreq::redirect::Policy::none());

        if let Some(proxy) = proxy {
            builder = builder.proxy(proxy);
        }

        let client = builder.build().expect("failed to build wreq stealth client");

        StealthHttpClient {
            client,
            ssrf,
            cookie_jar,
            extra_headers: RwLock::new(HashMap::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, NetError> {
        self.ssrf.validate(url).await.map_err(NetError::SsrfDenied)?;

        let mut current_url = url.clone();
        let mut redirects = Vec::new();

        for _ in 0..20 {
            let mut req = self.client.get(current_url.as_str());

            let cookie_header = self.cookie_jar.get_cookie_header(&current_url);
            if !cookie_header.is_empty() {
                req = req.header("Cookie", &cookie_header);
            }

            for (k, v) in self.extra_headers.read().await.iter() {
                req = req.header(k.as_str(), v.as_str());
            }

            self.in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let resp = req.send().await.map_err(|e| {
                self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                NetError::Network(format!("{}: {} (source: {:?})", current_url, e, e.source()))
            })?;
            self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            let status = resp.status();

            for val in resp.headers().get_all("set-cookie") {
                if let Ok(s) = val.to_str() {
                    self.cookie_jar.set_cookie(s, &current_url);
                }
            }

            let response_headers: HashMap<String, String> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
                .collect();

            if status.is_redirection()
                && let Some(location) = resp.headers().get("location")
            {
                let location_str = location
                    .to_str()
                    .map_err(|_| NetError::Network("Invalid redirect Location".into()))?;
                let next_url = current_url
                    .join(location_str)
                    .map_err(|e| NetError::Network(format!("Invalid redirect URL: {}", e)))?;
                // ~keep Re-validate every hop: the first URL being permitted says
                // nothing about where a redirect chain ends up.
                self.ssrf.validate(&next_url).await.map_err(NetError::SsrfDenied)?;
                redirects.push(current_url.clone());
                current_url = next_url;
                continue;
            }

            let body = resp
                .bytes()
                .await
                .map_err(|e| NetError::Network(format!("Failed to read body: {}", e)))?
                .to_vec();

            return Ok(Response {
                url: current_url,
                status: status.as_u16(),
                headers: response_headers,
                body,
                redirected_from: redirects,
            });
        }

        Err(NetError::TooManyRedirects(url.to_string()))
    }

    pub async fn set_extra_headers(&self, headers: HashMap<String, String>) {
        *self.extra_headers.write().await = headers;
    }

    pub fn active_requests(&self) -> u32 {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_network_idle(&self) -> bool {
        self.active_requests() == 0
    }
}

#[cfg(all(test, feature = "stealth"))]
mod tests {
    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Debug)]
    struct AllowAll;

    #[async_trait::async_trait]
    impl SsrfValidator for AllowAll {
        async fn validate(&self, _url: &Url) -> Result<(), String> {
            Ok(())
        }
    }

    fn proxied_client(proxy: &str) -> Result<StealthHttpClient, NetError> {
        StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), Some(proxy), Arc::new(AllowAll))
    }

    #[test]
    fn an_unparseable_or_scheme_less_proxy_url_refuses_the_client_without_echoing_it() {
        for proxy in crate::net::proxy::credential_urls::URLS {
            let Err(err) = proxied_client(proxy) else {
                panic!("{proxy} must refuse the client, not build one that connects directly");
            };
            assert!(matches!(err, NetError::InvalidProxy(_)), "{proxy}: got {err:?}");
            crate::net::proxy::credential_urls::assert_not_shown(proxy, &err.to_string());
        }
    }

    #[test]
    fn a_socks5_or_other_unsupported_proxy_scheme_refuses_the_client() {
        for (proxy, scheme) in [("socks5://proxy.test:1080", "socks5"), ("ftp://proxy.test:21", "ftp")] {
            let Err(err) = proxied_client(proxy) else {
                panic!("{proxy} must refuse the client");
            };
            assert!(
                matches!(err, NetError::InvalidProxy(crate::net::proxy::ProxyError::UnsupportedScheme(ref s)) if s == scheme),
                "{proxy}: got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_http_proxy_carries_the_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let proxy = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let read = socket.read(&mut buf).await.unwrap_or(0);
            log.lock()
                .expect("lock")
                .push(String::from_utf8_lossy(&buf[..read]).to_string());
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nvia-proxy")
                .await;
        });
        let client = proxied_client(&proxy).expect("an http proxy must build");

        let response = tokio::time::timeout(
            Duration::from_secs(10),
            client.fetch(&"http://origin.test/page".parse::<Url>().expect("valid URL")),
        )
        .await
        .expect("the fetch must finish")
        .expect("the proxy answers, so the fetch must succeed");

        assert_eq!(response.body, b"via-proxy");
        let seen = seen.lock().expect("lock");
        assert!(
            seen.first()
                .is_some_and(|r| r.starts_with("GET http://origin.test/page ")),
            "the proxy must receive the absolute-form request, got {seen:?}"
        );
    }
}
