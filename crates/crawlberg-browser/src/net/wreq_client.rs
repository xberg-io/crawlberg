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
use crate::net::credential::{OriginHeaders, refuse_userinfo, without_userinfo};
#[cfg(feature = "stealth")]
use crate::net::resolver::ValidatorResolver;
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
    /// The credential header the embedder scoped to one host; sent only to that host.
    pub origin_headers: RwLock<Option<OriginHeaders>>,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
}

#[cfg(feature = "stealth")]
impl StealthHttpClient {
    pub fn new(cookie_jar: Arc<CookieJar>) -> Self {
        Self::with_proxy(cookie_jar, None)
    }

    pub fn with_proxy(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        Self::with_ssrf(cookie_jar, proxy_url, Arc::new(DefaultSsrfValidator::from_env()))
    }

    /// Build a stealth client with an explicit SSRF policy.
    pub fn with_ssrf(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>, ssrf: Arc<dyn SsrfValidator>) -> Self {
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

        match proxy_url.and_then(|proxy| wreq::Proxy::all(proxy).ok()) {
            Some(p) => builder = builder.proxy(p),
            // ~keep Connect only to the addresses the policy resolved; see `ValidatorResolver`.
            // ~keep With a proxy, the proxy resolves the target.
            None => builder = builder.dns_resolver(ValidatorResolver::new(ssrf.clone())),
        }

        let client = builder.build().expect("failed to build wreq stealth client");

        StealthHttpClient {
            client,
            ssrf,
            cookie_jar,
            extra_headers: RwLock::new(HashMap::new()),
            origin_headers: RwLock::new(None),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, NetError> {
        refuse_userinfo(url)?;
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

            if let Some(origin_headers) = self.origin_headers.read().await.as_ref() {
                for (name, value) in origin_headers.headers_for(&current_url) {
                    req = req.header(name.as_str(), value.as_str());
                }
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
                    .map(|next_url| without_userinfo(&next_url))
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
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn a_url_with_userinfo_is_refused_before_the_network() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accepted = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 16];
            let _ = socket.read(&mut buf).await;
        });
        let client = StealthHttpClient::new(Arc::new(CookieJar::new()));

        let err = client
            .fetch(&format!("http://user:s3cret@{addr}/").parse::<Url>().expect("valid URL"))
            .await
            .expect_err("a URL with userinfo must be refused");

        let NetError::Blocked(message) = &err else {
            panic!("expected NetError::Blocked, got {err:?}");
        };
        assert!(
            !message.contains("s3cret"),
            "the password must not be named, got '{message}'"
        );
        assert!(
            message.contains(&addr.to_string()),
            "the refusal names the clean URL, got '{message}'"
        );
        assert!(!accepted.is_finished(), "nothing may reach the network");
        accepted.abort();
    }

    #[tokio::test]
    async fn a_rebinding_host_never_reaches_the_address_the_policy_denies() {
        use crate::net::resolver::tests::{RebindingPolicy, denied_server};

        let (port, seen) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nDENIED").await;
        let policy = Arc::new(RebindingPolicy::default());
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), None, policy.clone());

        client
            .fetch(&format!("http://localhost:{port}/").parse::<Url>().expect("valid URL"))
            .await
            .expect_err("the connection's lookup answers a denied address");

        assert!(
            seen.lock().expect("lock").is_empty(),
            "the denied address must receive no connection: {:?}",
            seen.lock().expect("lock")
        );
        assert_eq!(
            *policy.resolved.lock().expect("lock"),
            vec!["localhost"],
            "the connection must use the policy's lookup"
        );
    }

    #[tokio::test]
    async fn a_proxied_stealth_client_leaves_the_target_to_the_proxy() {
        use crate::net::resolver::tests::RebindingPolicy;

        let (proxy, proxy_requests) =
            recording_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()).await;
        let policy = Arc::new(RebindingPolicy::default());
        // ~keep A proxy named by host: a client that asked the policy for it would be refused.
        let proxy = format!("http://localhost:{}", proxy.port());
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), Some(&proxy), policy.clone());

        client
            .fetch(&"http://example.invalid/".parse::<Url>().expect("valid URL"))
            .await
            .expect("the proxy answers the request");

        assert_eq!(
            proxy_requests.lock().expect("lock").len(),
            1,
            "the request goes to the proxy"
        );
        assert!(
            policy.resolved.lock().expect("lock").is_empty(),
            "the proxy resolves the target, so the client must not"
        );
    }

    #[derive(Debug)]
    struct AllowAll;

    #[async_trait::async_trait]
    impl SsrfValidator for AllowAll {
        async fn validate(&self, _url: &Url) -> Result<(), String> {
            Ok(())
        }
    }

    /// Serves `response` to every connection on a fresh loopback port, recording each request head.
    async fn recording_server(response: String) -> (std::net::SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                log.lock()
                    .expect("lock")
                    .push(String::from_utf8_lossy(&buf[..read]).to_lowercase());
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        (addr, requests)
    }

    #[tokio::test]
    async fn the_origin_headers_reach_their_host_and_a_redirect_loses_its_userinfo() {
        let (other, other_requests) =
            recording_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()).await;
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://user:s3cret@localhost:{}/away\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            other.port()
        );
        let (start, start_requests) = recording_server(redirect).await;
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), None, Arc::new(AllowAll));
        *client.origin_headers.write().await = Some(OriginHeaders {
            host: "127.0.0.1".to_owned(),
            headers: vec![("Authorization".to_owned(), "Basic b3JpZ2luOmNyZWQ=".to_owned())],
        });

        client
            .fetch(&format!("http://{start}/").parse::<Url>().expect("valid URL"))
            .await
            .expect("the redirect must be followed");

        let start_requests = start_requests.lock().expect("lock");
        assert!(
            start_requests[0].contains("authorization: basic b3jpz2luomnyzwq="),
            "the scoped host gets the header: {start_requests:?}"
        );
        let other_requests = other_requests.lock().expect("lock");
        assert_eq!(other_requests.len(), 1, "the cross-host redirect must be followed");
        assert!(
            !other_requests[0].contains("authorization:"),
            "neither the credential nor the Location's userinfo reaches the other host: {other_requests:?}"
        );
    }
}
