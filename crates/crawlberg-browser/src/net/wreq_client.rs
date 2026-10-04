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
use crate::net::cookies::{CookieJar, CookieRequestContext};
#[cfg(feature = "stealth")]
use crate::net::credential::{OriginHeaders, refuse_userinfo, without_userinfo};
#[cfg(feature = "stealth")]
use crate::net::resolver::{
    EnvironmentSystemProxySelector, SystemProxyIdentity, SystemProxySelector, ValidatorResolver,
};
#[cfg(feature = "stealth")]
use crate::net::ssrf::{DefaultSsrfValidator, SsrfValidator};

#[cfg(feature = "stealth")]
pub const STEALTH_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";

#[cfg(feature = "stealth")]
const MAX_ENVIRONMENT_PROXY_CLIENTS: usize = 64;

#[cfg(feature = "stealth")]
pub struct StealthHttpClient {
    client: wreq::Client,
    explicit_proxy: bool,
    environment_clients: std::sync::Mutex<HashMap<SystemProxyIdentity, wreq::Client>>,
    system_proxy_selector: Arc<dyn SystemProxySelector>,
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
        Self::build(
            cookie_jar,
            None,
            Arc::new(DefaultSsrfValidator::from_env()),
            Arc::new(EnvironmentSystemProxySelector),
        )
    }

    /// Build a stealth client that sends every request through `proxy`, if given.
    ///
    /// Fails with [`NetError::InvalidProxy`] when the proxy cannot be used, rather than
    /// building a client that silently connects directly.
    pub fn with_proxy(
        cookie_jar: Arc<CookieJar>,
        proxy: Option<&crate::net::proxy::UpstreamProxy>,
    ) -> Result<Self, NetError> {
        Self::with_ssrf(cookie_jar, proxy, Arc::new(DefaultSsrfValidator::from_env()))
    }

    /// Build a stealth client with an explicit SSRF policy.
    ///
    /// Fails with [`NetError::InvalidProxy`] when the proxy cannot be used.
    pub fn with_ssrf(
        cookie_jar: Arc<CookieJar>,
        proxy: Option<&crate::net::proxy::UpstreamProxy>,
        ssrf: Arc<dyn SsrfValidator>,
    ) -> Result<Self, NetError> {
        let proxy = proxy.map(crate::net::proxy::UpstreamProxy::wreq_proxy).transpose()?;
        Ok(Self::build(
            cookie_jar,
            proxy,
            ssrf,
            Arc::new(EnvironmentSystemProxySelector),
        ))
    }

    #[cfg(test)]
    fn with_ssrf_and_proxy_selector(
        cookie_jar: Arc<CookieJar>,
        ssrf: Arc<dyn SsrfValidator>,
        selector: Arc<dyn SystemProxySelector>,
    ) -> Self {
        Self::build(cookie_jar, None, ssrf, selector)
    }

    fn build(
        cookie_jar: Arc<CookieJar>,
        proxy: Option<wreq::Proxy>,
        ssrf: Arc<dyn SsrfValidator>,
        system_proxy_selector: Arc<dyn SystemProxySelector>,
    ) -> Self {
        let explicit_proxy = proxy.is_some();
        let client = Self::build_client(proxy, &ssrf);

        StealthHttpClient {
            client,
            explicit_proxy,
            environment_clients: std::sync::Mutex::new(HashMap::new()),
            system_proxy_selector,
            ssrf,
            cookie_jar,
            extra_headers: RwLock::new(HashMap::new()),
            origin_headers: RwLock::new(None),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    fn build_client(proxy: Option<wreq::Proxy>, ssrf: &Arc<dyn SsrfValidator>) -> wreq::Client {
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

        match proxy {
            Some(proxy) => builder = builder.proxy(proxy),
            // ~keep Connect only to the addresses the policy resolved; see `ValidatorResolver`.
            // ~keep With a proxy, the proxy resolves the target.
            None => builder = builder.no_proxy().dns_resolver(ValidatorResolver::new(ssrf.clone())),
        }

        builder.build().expect("failed to build wreq stealth client")
    }

    fn client_for(&self, url: &Url) -> Result<wreq::Client, NetError> {
        if self.explicit_proxy {
            self.ssrf
                .validate_remote_resolution(url)
                .map_err(NetError::SsrfDenied)?;
            return Ok(self.client.clone());
        }
        let Some(proxy) = self.system_proxy_selector.proxy_for(url)? else {
            return Ok(self.client.clone());
        };
        self.ssrf
            .validate_remote_resolution(url)
            .map_err(NetError::SsrfDenied)?;
        let identity = proxy.identity();
        let mut clients = self
            .environment_clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = clients.get(&identity) {
            return Ok(client.clone());
        }
        let client = Self::build_client(Some(proxy.wreq_proxy()?), &self.ssrf);
        if clients.len() >= MAX_ENVIRONMENT_PROXY_CLIENTS {
            clients.clear();
        }
        clients.insert(identity, client.clone());
        Ok(client)
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, NetError> {
        self.fetch_following(url, None).await
    }

    /// Fetch `url`, following at most `max_redirects` redirects, as
    /// [`crate::net::HttpClient::fetch_following`] does.
    pub async fn fetch_following(&self, url: &Url, max_redirects: Option<usize>) -> Result<Response, NetError> {
        self.fetch_following_from(url, max_redirects, None).await
    }

    pub(crate) async fn fetch_following_from(
        &self,
        url: &Url,
        max_redirects: Option<usize>,
        initiator: Option<&Url>,
    ) -> Result<Response, NetError> {
        self.fetch_following_with_context(url, max_redirects, initiator, true)
            .await
    }

    pub(crate) async fn fetch_subresource(
        &self,
        url: &Url,
        site_for_cookies: Option<&Url>,
    ) -> Result<Response, NetError> {
        self.fetch_following_with_context(url, None, site_for_cookies, false)
            .await
    }

    async fn fetch_following_with_context(
        &self,
        url: &Url,
        max_redirects: Option<usize>,
        site_for_cookies: Option<&Url>,
        top_level: bool,
    ) -> Result<Response, NetError> {
        refuse_userinfo(url)?;
        self.ssrf.validate(url).await.map_err(NetError::SsrfDenied)?;

        let mut current_url = url.clone();
        let mut redirects = Vec::new();

        let requests = max_redirects.map_or(20, |limit| limit.saturating_add(1));
        for _ in 0..requests {
            let mut req = self.client_for(&current_url)?.get(current_url.as_str());

            let context = if top_level {
                CookieRequestContext::top_level(site_for_cookies, true)
            } else {
                CookieRequestContext::subresource(site_for_cookies)
            };
            let cookie_header = self.cookie_jar.get_cookie_header_for_request(&current_url, context);
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
                    self.cookie_jar.set_cookie_for_request(s, &current_url, context);
                }
            }

            let response_headers: HashMap<String, String> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
                .collect();

            if status.is_redirection()
                && max_redirects.is_none_or(|limit| redirects.len() < limit)
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
    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

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
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), None, policy.clone())
            .expect("no proxy, so the client must build");

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
        let proxy = crate::net::proxy::test_proxy(&proxy).expect("an http proxy");
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), Some(&proxy), policy.clone())
            .expect("an http proxy must build");

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

    #[tokio::test]
    async fn environment_proxy_changes_are_applied_by_the_stealth_client() {
        use crate::net::resolver::tests::{RebindingPolicy, TestProxySelector, denied_server};

        let (first_port, first_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfirst").await;
        let (second_port, second_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond").await;
        let selector = Arc::new(TestProxySelector::default());
        selector.set_proxy(&format!("http://localhost:{first_port}"));
        let policy = Arc::new(RebindingPolicy::default());
        let client = StealthHttpClient::with_ssrf_and_proxy_selector(
            Arc::new(CookieJar::new()),
            policy.clone(),
            selector.clone(),
        );
        let target = "http://example.invalid/page".parse::<Url>().expect("valid URL");

        let first = client.fetch(&target).await.expect("the first proxy must answer");
        selector.set_proxy(&format!("http://localhost:{second_port}"));
        let second = client.fetch(&target).await.expect("the changed proxy must answer");

        assert_eq!((first.body, second.body), (b"first".to_vec(), b"second".to_vec()));
        assert_eq!(first_requests.lock().expect("lock").len(), 1);
        assert_eq!(second_requests.lock().expect("lock").len(), 1);
        assert!(
            policy.resolved.lock().expect("lock").is_empty(),
            "the policy resolver must not receive either private proxy host"
        );
    }

    #[tokio::test]
    async fn no_proxy_keeps_the_stealth_policy_resolver_on_a_direct_request() {
        use crate::net::resolver::tests::{RebindingPolicy, TestProxySelector, denied_server};

        let (target_port, target_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ntarget").await;
        let (proxy_port, proxy_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nproxy").await;
        let selector = Arc::new(TestProxySelector::default());
        selector.set_proxy(&format!("http://localhost:{proxy_port}"));
        selector.direct_host("localhost");
        let policy = Arc::new(RebindingPolicy::default());
        let client =
            StealthHttpClient::with_ssrf_and_proxy_selector(Arc::new(CookieJar::new()), policy.clone(), selector);

        client
            .fetch(
                &format!("http://localhost:{target_port}/")
                    .parse::<Url>()
                    .expect("valid URL"),
            )
            .await
            .expect_err("the direct connection lookup must be refused");

        assert_eq!(*policy.resolved.lock().expect("lock"), vec!["localhost"]);
        assert!(target_requests.lock().expect("lock").is_empty());
        assert!(proxy_requests.lock().expect("lock").is_empty());
    }

    #[derive(Debug)]
    struct AllowAll;

    #[async_trait::async_trait]
    impl SsrfValidator for AllowAll {
        async fn validate(&self, _url: &Url) -> Result<(), String> {
            Ok(())
        }
    }

    fn proxied_client(proxy: &str) -> Result<StealthHttpClient, NetError> {
        let proxy = crate::net::proxy::test_proxy(proxy)?;
        StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), Some(&proxy), Arc::new(AllowAll))
    }

    #[tokio::test]
    async fn a_credentialed_proxy_carries_the_request_with_its_credentials() {
        use crate::net::proxy::credentialed_proxy;
        let (proxy, requests) = credentialed_proxy::start().await;
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), Some(&proxy), Arc::new(AllowAll))
            .expect("an http proxy must build");

        let response = tokio::time::timeout(
            Duration::from_secs(10),
            client.fetch(&"http://origin.test/page".parse::<Url>().expect("valid URL")),
        )
        .await
        .expect("the fetch must finish")
        .expect("the proxy accepts the credentials, so the fetch must succeed");

        assert_eq!(response.body, b"via-proxy");
        credentialed_proxy::assert_one_authenticated_request(&requests, "http://origin.test/page");
    }

    #[tokio::test]
    async fn a_proxy_that_refuses_the_credentials_fails_the_fetch_instead_of_connecting_directly() {
        use crate::net::proxy::credentialed_proxy;
        let (proxy, requests) = credentialed_proxy::start().await;
        let direct = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let target = format!("http://{}/page", direct.local_addr().expect("addr"));
        let client = StealthHttpClient::with_ssrf(
            Arc::new(CookieJar::new()),
            Some(&credentialed_proxy::with_wrong_password(&proxy)),
            Arc::new(AllowAll),
        )
        .expect("an http proxy must build");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client.fetch(&target.parse::<Url>().expect("valid URL")),
        )
        .await
        .expect("the fetch must finish");

        assert!(
            !matches!(result, Ok(ref response) if response.status == 200),
            "a refused proxy must not serve the page: {result:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), direct.accept())
                .await
                .is_err(),
            "the fetch connected directly"
        );
        let requests = requests.lock().expect("lock");
        assert_eq!(requests.len(), 1, "the fetch must go to the proxy: {requests:?}");
        let sent = credentialed_proxy::proxy_authorization(&requests[0]);
        assert!(
            sent.is_some() && sent != Some(credentialed_proxy::expected_authorization()),
            "the configured wrong credentials must be sent: {sent:?}"
        );
        assert!(
            !format!("{result:?}").contains(credentialed_proxy::PASSWORD),
            "{result:?}"
        );
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

    #[tokio::test]
    async fn stealth_navigation_applies_same_site_to_the_initiating_site() {
        let (addr, requests) =
            recording_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()).await;
        let jar = Arc::new(CookieJar::new());
        let target = format!("http://{addr}/").parse::<Url>().expect("valid target URL");
        jar.set_cookie("strict=1; SameSite=Strict", &target);
        jar.set_cookie("lax=1; SameSite=Lax", &target);
        jar.set_cookie("default=1", &target);
        let client =
            StealthHttpClient::with_ssrf(jar, None, Arc::new(AllowAll)).expect("no proxy, so the client must build");
        let initiator = Url::parse("http://localhost/source").expect("valid initiator URL");

        client
            .fetch_following_from(&target, None, Some(&initiator))
            .await
            .expect("the navigation must succeed");

        let requests = requests.lock().expect("lock");
        let request = requests.first().expect("the server must receive one request");
        assert!(
            request.contains("cookie: lax=1") || request.contains("cookie: default=1"),
            "{request}"
        );
        assert!(request.contains("lax=1") && request.contains("default=1"), "{request}");
        assert!(!request.contains("strict=1"), "{request}");
    }

    /// Serves `response` to every connection on a fresh loopback port, recording each request head.
    async fn recording_server(response: String) -> (std::net::SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
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
    async fn a_limit_above_the_hop_cap_follows_past_it() {
        let (addr, requests) = recording_server(
            "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        )
        .await;
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), None, Arc::new(AllowAll))
            .expect("no proxy, so the client must build");
        let url = format!("http://{addr}/").parse::<Url>().expect("valid URL");

        let stopped = client
            .fetch_following(&url, Some(30))
            .await
            .expect("a limit of 30 must follow 30 redirects");

        assert_eq!(
            (stopped.status, stopped.redirected_from.len()),
            (302, 30),
            "the limit, not the cap of 20 hops, bounds the chain"
        );
        assert_eq!(requests.lock().expect("lock").len(), 31);
    }

    #[tokio::test]
    async fn a_limited_fetch_ends_on_the_redirect_at_the_limit() {
        let (end, end_requests) =
            recording_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()).await;
        let redirect = format!(
            "HTTP/1.1 301 Moved Permanently\r\nLocation: http://{end}/end\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (start, _) = recording_server(redirect).await;
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), None, Arc::new(AllowAll))
            .expect("no proxy, so the client must build");
        let url = format!("http://{start}/").parse::<Url>().expect("valid URL");

        let stopped = client
            .fetch_following(&url, Some(0))
            .await
            .expect("the fetch must succeed");
        assert_eq!((stopped.status, stopped.url.clone()), (301, url.clone()));
        assert!(
            end_requests.lock().expect("lock").is_empty(),
            "the limit stops the next hop"
        );

        let followed = client
            .fetch_following(&url, Some(1))
            .await
            .expect("the fetch must succeed");
        assert_eq!((followed.status, followed.redirected_from.len()), (200, 1));
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
        let client = StealthHttpClient::with_ssrf(Arc::new(CookieJar::new()), None, Arc::new(AllowAll))
            .expect("no proxy, so the client must build");
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
