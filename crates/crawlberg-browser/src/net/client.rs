use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::{Client, Method};
use tokio::sync::RwLock;
use url::Url;

use crate::net::cookies::{CookieJar, CookieRequestContext};
use crate::net::credential::{OriginHeaders, refuse_userinfo, without_userinfo};
use crate::net::error_with_causes;
use crate::net::interceptor::{InterceptAction, RequestInterceptor};
use crate::net::proxy::UpstreamProxy;
use crate::net::resolver::{
    EnvironmentSystemProxySelector, SystemProxyIdentity, SystemProxySelector, with_policy_resolver,
};
use crate::net::ssrf::{DefaultSsrfValidator, SsrfValidator};
use crate::redact::{RedactedHeaders, RedactedValues};

#[derive(Clone)]
pub struct Response {
    pub url: Url,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub redirected_from: Vec<Url>,
}

impl std::fmt::Debug for Response {
    /// Redacted: `headers` can carry `Set-Cookie`. Header names stay visible; sensitive
    /// values print as `***`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            url,
            status,
            headers,
            body,
            redirected_from,
        } = self;
        f.debug_struct("Response")
            .field("url", url)
            .field("status", status)
            .field("headers", &RedactedHeaders(headers))
            .field("body", body)
            .field("redirected_from", redirected_from)
            .finish()
    }
}

impl Response {
    pub fn text(&self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.body.clone())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(|s| s.as_str())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    pub fn is_html(&self) -> bool {
        self.content_type().map(|ct| ct.contains("text/html")).unwrap_or(false)
    }
}

#[derive(Clone)]
pub struct RequestInfo {
    pub url: Url,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub resource_type: ResourceType,
}

impl std::fmt::Debug for RequestInfo {
    /// Redacted: `headers` is a *request* map populated from caller configuration, so every
    /// value is hidden and only the names print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            url,
            method,
            headers,
            resource_type,
        } = self;
        f.debug_struct("RequestInfo")
            .field("url", url)
            .field("method", method)
            .field("headers", &RedactedValues(headers))
            .field("resource_type", resource_type)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceType {
    Document,
    Script,
    Stylesheet,
    Image,
    Font,
    Xhr,
    Fetch,
    Other,
}

pub type RequestCallback = Arc<dyn Fn(&RequestInfo) + Send + Sync>;
pub type ResponseCallback = Arc<dyn Fn(&RequestInfo, &Response) + Send + Sync>;

/// Redirect hops attempted before giving up with [`NetError::TooManyRedirects`].
const MAX_REDIRECTS: usize = 20;
const MAX_ENVIRONMENT_PROXY_CLIENTS: usize = 64;

const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";

/// The static half of the Chrome request fingerprint.
///
/// ~keep These values are a browser-emulation surface, not cosmetics: sites fingerprint the
/// exact set, order and spelling of the `sec-ch-ua*` / `sec-fetch-*` headers, and a mismatch
/// between them and the User-Agent is itself a bot signal. Change them only together, and only
/// to track a real Chrome release.
fn browser_fingerprint_headers(user_agent: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(user_agent).unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_USER_AGENT)),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
        ),
    );
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9"),
    );
    headers.insert(
        HeaderName::from_static("sec-ch-ua"),
        HeaderValue::from_static("\"Chromium\";v=\"145\", \"Not;A=Brand\";v=\"24\", \"Google Chrome\";v=\"145\""),
    );
    headers.insert(
        HeaderName::from_static("sec-ch-ua-mobile"),
        HeaderValue::from_static("?0"),
    );
    headers.insert(
        HeaderName::from_static("sec-ch-ua-platform"),
        HeaderValue::from_static("\"Linux\""),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-dest"),
        HeaderValue::from_static("document"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-mode"),
        HeaderValue::from_static("navigate"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-site"),
        HeaderValue::from_static("none"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-user"),
        HeaderValue::from_static("?1"),
    );
    headers.insert(
        HeaderName::from_static("upgrade-insecure-requests"),
        HeaderValue::from_static("1"),
    );
    headers
}

/// Flatten the response headers to lowercase name/value pairs, dropping non-UTF-8 values.
fn collect_response_headers(response: &reqwest::Response) -> HashMap<String, String> {
    response
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
        .collect()
}

fn resolve_redirect(current_url: &Url, location: &HeaderValue) -> Result<Url, NetError> {
    let location_str = location
        .to_str()
        .map_err(|_| NetError::Network("Invalid redirect Location header".into()))?;
    current_url
        .join(location_str)
        .map(|next_url| without_userinfo(&next_url))
        .map_err(|e| NetError::Network(format!("Invalid redirect URL: {}", e)))
}

/// Whether a redirect status rewrites the request to a bodyless GET (301/302/303 do; 307/308 do not).
fn downgrades_to_get(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::MOVED_PERMANENTLY
        || status == reqwest::StatusCode::FOUND
        || status == reqwest::StatusCode::SEE_OTHER
}

async fn fetch_file_url(url: &Url) -> Result<Response, NetError> {
    let path = url
        .to_file_path()
        .map_err(|_| NetError::Network("Invalid file URL".to_string()))?;
    let body = tokio::fs::read(&path)
        .await
        .map_err(|e| NetError::Network(format!("Failed to read file: {}", e)))?;

    let mut headers = HashMap::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ct = match ext.to_lowercase().as_str() {
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "js" | "mjs" => "application/javascript",
            "json" => "application/json",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            _ => "application/octet-stream",
        };
        headers.insert("content-type".to_string(), ct.to_string());
    }

    Ok(Response {
        url: url.clone(),
        status: 200,
        headers,
        body,
        redirected_from: Vec::new(),
    })
}

pub struct HttpClient {
    client: tokio::sync::OnceCell<Client>,
    environment_clients: tokio::sync::Mutex<HashMap<SystemProxyIdentity, Client>>,
    system_proxy_selector: Arc<dyn SystemProxySelector>,
    upstream: Option<UpstreamProxy>,
    proxy: Option<reqwest::Proxy>,
    /// SSRF policy applied to the initial URL and every redirect hop.
    pub ssrf: Arc<dyn SsrfValidator>,
    /// Whether `file://` URLs may be fetched. Off unless the embedder opts in.
    pub allow_file_access: bool,
    pub cookie_jar: Arc<CookieJar>,
    pub user_agent: RwLock<String>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    /// The credential header the embedder scoped to one host; sent only to that host.
    pub origin_headers: RwLock<Option<OriginHeaders>>,
    pub interceptor: RwLock<Option<Box<dyn RequestInterceptor + Send + Sync>>>,
    pub on_request: RwLock<Vec<RequestCallback>>,
    pub on_response: RwLock<Vec<ResponseCallback>>,
    pub timeout: Duration,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
}

impl HttpClient {
    pub fn new() -> Self {
        Self::with_cookie_jar(Arc::new(CookieJar::new()))
    }

    pub fn with_cookie_jar(cookie_jar: Arc<CookieJar>) -> Self {
        Self::build(
            cookie_jar,
            None,
            Arc::new(DefaultSsrfValidator::from_env()),
            false,
            Arc::new(EnvironmentSystemProxySelector),
        )
    }

    /// Build a client that sends every request through `proxy`, if given.
    ///
    /// Fails with [`NetError::InvalidProxy`] when the proxy cannot be used, rather than
    /// building a client that silently connects directly.
    pub fn with_options(cookie_jar: Arc<CookieJar>, proxy: Option<&UpstreamProxy>) -> Result<Self, NetError> {
        Self::with_ssrf(cookie_jar, proxy, Arc::new(DefaultSsrfValidator::from_env()), false)
    }

    /// Build a client with an explicit SSRF policy.
    ///
    /// `crawlberg` uses this to inject the crawl's configured policy — including its
    /// allowlist, in place of the deny-list-only default. Fails with
    /// [`NetError::InvalidProxy`] when the proxy cannot be used.
    pub fn with_ssrf(
        cookie_jar: Arc<CookieJar>,
        proxy: Option<&UpstreamProxy>,
        ssrf: Arc<dyn SsrfValidator>,
        allow_file_access: bool,
    ) -> Result<Self, NetError> {
        let proxy = match proxy {
            Some(upstream) => Some((upstream.clone(), upstream.reqwest_proxy()?)),
            None => None,
        };
        Ok(Self::build(
            cookie_jar,
            proxy,
            ssrf,
            allow_file_access,
            Arc::new(EnvironmentSystemProxySelector),
        ))
    }

    #[cfg(test)]
    fn with_ssrf_and_proxy_selector(
        cookie_jar: Arc<CookieJar>,
        ssrf: Arc<dyn SsrfValidator>,
        selector: Arc<dyn SystemProxySelector>,
    ) -> Self {
        Self::build(cookie_jar, None, ssrf, false, selector)
    }

    fn build(
        cookie_jar: Arc<CookieJar>,
        proxy: Option<(UpstreamProxy, reqwest::Proxy)>,
        ssrf: Arc<dyn SsrfValidator>,
        allow_file_access: bool,
        system_proxy_selector: Arc<dyn SystemProxySelector>,
    ) -> Self {
        let (upstream, proxy) = proxy.unzip();
        HttpClient {
            client: tokio::sync::OnceCell::new(),
            environment_clients: tokio::sync::Mutex::new(HashMap::new()),
            system_proxy_selector,
            upstream,
            proxy,
            ssrf,
            allow_file_access,
            cookie_jar,
            user_agent: RwLock::new(DEFAULT_USER_AGENT.to_string()),
            extra_headers: RwLock::new(HashMap::new()),
            origin_headers: RwLock::new(None),
            interceptor: RwLock::new(None),
            on_request: RwLock::new(Vec::new()),
            on_response: RwLock::new(Vec::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            timeout: Duration::from_secs(30),
        }
    }

    async fn get_base_client(&self) -> &Client {
        self.client
            .get_or_init(|| async {
                let mut builder = Client::builder()
                    .redirect(Policy::none())
                    .timeout(Duration::from_secs(30))
                    .danger_accept_invalid_certs(false);

                let proxied = self.proxy.is_some();
                if let Some(ref proxy) = self.proxy {
                    builder = builder.proxy(proxy.clone());
                } else {
                    builder = builder.no_proxy();
                }
                builder = with_policy_resolver(builder, proxied, &self.ssrf);

                builder.build().expect("failed to build HTTP client")
            })
            .await
    }

    async fn get_client(&self, url: &Url) -> Result<Client, NetError> {
        if self.proxy.is_some() {
            self.ssrf
                .validate_remote_resolution(url)
                .map_err(NetError::SsrfDenied)?;
            return Ok(self.get_base_client().await.clone());
        }
        let Some(proxy) = self.system_proxy_selector.proxy_for(url)? else {
            return Ok(self.get_base_client().await.clone());
        };
        self.ssrf
            .validate_remote_resolution(url)
            .map_err(NetError::SsrfDenied)?;
        let identity = proxy.identity();
        let mut clients = self.environment_clients.lock().await;
        if let Some(client) = clients.get(&identity) {
            return Ok(client.clone());
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .danger_accept_invalid_certs(false)
            .proxy(proxy.reqwest_proxy()?)
            .build()
            .expect("failed to build HTTP client");
        if clients.len() >= MAX_ENVIRONMENT_PROXY_CLIENTS {
            clients.clear();
        }
        clients.insert(identity, client.clone());
        Ok(client)
    }

    /// Read-only accessor for the proxy the client was configured with
    /// (if any). Exposed so the JS fetch bridge can route its own reqwest
    /// requests through the same upstream proxy.
    pub fn proxy(&self) -> Option<&UpstreamProxy> {
        self.upstream.as_ref()
    }

    /// Apply the SSRF policy to `url`.
    ///
    /// `file://` is decided here rather than inside the validator: it is a local-access
    /// question, not a network-egress one, and the injected crawlberg validator rejects
    /// the scheme outright.
    async fn validate_url(&self, url: &Url) -> Result<(), NetError> {
        if url.scheme() == "file" {
            return if self.allow_file_access {
                Ok(())
            } else {
                Err(NetError::SsrfDenied(
                    "file:// access is disabled; enable allow_file_access to permit it".to_string(),
                ))
            };
        }

        self.ssrf.validate(url).await.map_err(NetError::SsrfDenied)
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, NetError> {
        self.fetch_with_method(Method::GET, url, None).await
    }

    pub async fn post_form(&self, url: &Url, body: &str) -> Result<Response, NetError> {
        self.fetch_with_method(Method::POST, url, Some(body.as_bytes().to_vec()))
            .await
    }

    pub async fn fetch_with_method(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
    ) -> Result<Response, NetError> {
        self.fetch_following(initial_method, url, initial_body, None).await
    }

    /// Fetch `url`, following at most `max_redirects` redirects. The redirect response at the
    /// limit is returned as the response, as the crawl's HTTP fetch returns it. `None` follows
    /// up to the client's own cap and fails past it with [`NetError::TooManyRedirects`].
    pub async fn fetch_following(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
        max_redirects: Option<usize>,
    ) -> Result<Response, NetError> {
        self.fetch_following_from(initial_method, url, initial_body, max_redirects, None)
            .await
    }

    pub(crate) async fn fetch_following_from(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
        max_redirects: Option<usize>,
        initiator: Option<&Url>,
    ) -> Result<Response, NetError> {
        self.fetch_following_with_context(initial_method, url, initial_body, max_redirects, initiator, true)
            .await
    }

    pub(crate) async fn fetch_subresource(
        &self,
        url: &Url,
        site_for_cookies: Option<&Url>,
    ) -> Result<Response, NetError> {
        self.fetch_following_with_context(Method::GET, url, None, None, site_for_cookies, false)
            .await
    }

    async fn fetch_following_with_context(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
        max_redirects: Option<usize>,
        site_for_cookies: Option<&Url>,
        top_level: bool,
    ) -> Result<Response, NetError> {
        refuse_userinfo(url)?;
        self.validate_url(url).await?;

        if url.scheme() == "file" {
            return fetch_file_url(url).await;
        }

        let mut method = initial_method;
        let mut body = initial_body;

        let mut current_url = url.clone();
        let mut redirects = Vec::new();

        let requests = max_redirects.map_or(MAX_REDIRECTS, |limit| limit.saturating_add(1));
        for _request in 0..requests {
            let request_info = self.request_info(&current_url, &method).await;

            if let Some(response) = self.apply_interceptor(&request_info).await? {
                return Ok(response);
            }

            for cb in self.on_request.read().await.iter() {
                cb(&request_info);
            }

            let context = if top_level {
                CookieRequestContext::top_level(site_for_cookies, is_safe_method(&method))
            } else {
                CookieRequestContext::subresource(site_for_cookies)
            };
            let headers = self.request_headers(&current_url, context).await;
            let resp = self.send_request(&current_url, &method, body.as_ref(), headers).await?;

            let status = resp.status();
            self.store_response_cookies(&resp, &current_url, context);
            let response_headers = collect_response_headers(&resp);

            if status.is_redirection()
                && max_redirects.is_none_or(|limit| redirects.len() < limit)
                && let Some(location) = resp.headers().get(reqwest::header::LOCATION)
            {
                let next_url = resolve_redirect(&current_url, location)?;
                self.validate_url(&next_url).await?;
                redirects.push(current_url.clone());
                current_url = next_url;
                if downgrades_to_get(status) {
                    method = Method::GET;
                    body = None;
                }
                continue;
            }

            let body_bytes = resp
                .bytes()
                .await
                .map_err(|e| NetError::Network(format!("Failed to read body: {}", e)))?
                .to_vec();

            let response = Response {
                url: current_url,
                status: status.as_u16(),
                headers: response_headers,
                body: body_bytes,
                redirected_from: redirects,
            };

            for cb in self.on_response.read().await.iter() {
                cb(&request_info, &response);
            }

            return Ok(response);
        }

        Err(NetError::TooManyRedirects(current_url.to_string()))
    }

    async fn request_info(&self, url: &Url, method: &Method) -> RequestInfo {
        RequestInfo {
            url: url.clone(),
            method: method.to_string(),
            headers: self.extra_headers.read().await.clone(),
            resource_type: ResourceType::Document,
        }
    }

    fn store_response_cookies(&self, response: &reqwest::Response, url: &Url, context: CookieRequestContext<'_>) {
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(set_cookie) = value.to_str() {
                self.cookie_jar.set_cookie_for_request(set_cookie, url, context);
            }
        }
    }

    /// Run the registered interceptor, if any.
    ///
    /// `Ok(Some(response))` means the interceptor fulfilled the request itself and no
    /// network request must be made; `Ok(None)` means carry on.
    async fn apply_interceptor(&self, request_info: &RequestInfo) -> Result<Option<Response>, NetError> {
        let action = match self.interceptor.read().await.as_ref() {
            Some(interceptor) => interceptor.intercept(request_info).await,
            None => return Ok(None),
        };

        match action {
            InterceptAction::Continue => Ok(None),
            InterceptAction::Block => Err(NetError::Blocked(request_info.url.to_string())),
            InterceptAction::Fulfill(response) => Ok(Some(response)),
            InterceptAction::ModifyHeaders(headers) => {
                self.extra_headers.write().await.extend(headers);
                Ok(None)
            }
        }
    }

    /// Build the outgoing header set: browser fingerprint, then jar cookies, then the
    /// caller's extra headers, which are applied last and therefore win.
    async fn request_headers(&self, url: &Url, context: CookieRequestContext<'_>) -> HeaderMap {
        let mut headers = browser_fingerprint_headers(&self.user_agent.read().await.clone());

        let cookie_header = self.cookie_jar.get_cookie_header_for_request(url, context);
        if !cookie_header.is_empty()
            && let Ok(value) = HeaderValue::from_str(&cookie_header)
        {
            headers.insert(reqwest::header::COOKIE, value);
        }

        for (key, value) in self.extra_headers.read().await.iter() {
            if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(key.as_bytes()), HeaderValue::from_str(value)) {
                headers.insert(name, value);
            }
        }

        if let Some(origin_headers) = self.origin_headers.read().await.as_ref() {
            for (name, value) in origin_headers.headers_for(url) {
                if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
                    headers.insert(name, value);
                }
            }
        }

        headers
    }

    async fn send_request(
        &self,
        url: &Url,
        method: &Method,
        body: Option<&Vec<u8>>,
        headers: HeaderMap,
    ) -> Result<reqwest::Response, NetError> {
        let mut req_builder = self
            .get_client(url)
            .await?
            .request(method.clone(), url.as_str())
            .headers(headers);

        if let Some(body) = body {
            if *method == Method::POST {
                req_builder = req_builder.header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            }
            req_builder = req_builder.body(body.clone());
        }

        self.in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let response = req_builder.send().await.map_err(|e| {
            self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            NetError::Network(format!("{}: {}", url, error_with_causes(&e)))
        })?;
        self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        Ok(response)
    }

    pub async fn set_user_agent(&self, ua: &str) {
        *self.user_agent.write().await = ua.to_string();
    }

    pub async fn set_extra_headers(&self, headers: HashMap<String, String>) {
        *self.extra_headers.write().await = headers;
    }

    /// Scope headers, such as a credential, to one host. See [`OriginHeaders`].
    pub async fn set_origin_headers(&self, origin_headers: Option<OriginHeaders>) {
        *self.origin_headers.write().await = origin_headers;
    }

    pub fn active_requests(&self) -> u32 {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_network_idle(&self) -> bool {
        self.active_requests() == 0
    }
}

fn is_safe_method(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD || method == Method::OPTIONS || method == Method::TRACE
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("Too many redirects: {0}")]
    TooManyRedirects(String),

    #[error("Request blocked: {0}")]
    Blocked(String),

    /// Refused by the SSRF policy, as opposed to failing in transport.
    #[error("SSRF policy denied the request: {0}")]
    SsrfDenied(String),

    /// The configured proxy cannot be used, so no client was built.
    #[error("invalid proxy: {0}")]
    InvalidProxy(#[from] crate::net::proxy::ProxyError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Records every URL it is asked about, so a test can prove the policy was
    /// actually consulted rather than bypassed.
    #[derive(Debug, Default)]
    struct RecordingValidator {
        seen: Mutex<Vec<String>>,
        deny: Option<String>,
    }

    #[async_trait::async_trait]
    impl SsrfValidator for RecordingValidator {
        async fn validate(&self, url: &Url) -> Result<(), String> {
            self.seen.lock().expect("lock").push(url.to_string());
            match &self.deny {
                Some(needle) if url.as_str().contains(needle.as_str()) => Err("denied by test policy".to_string()),
                _ => Ok(()),
            }
        }

        fn validate_remote_resolution(&self, _url: &Url) -> Result<(), String> {
            // ~keep This test policy decides from the URL string alone, so remote DNS cannot
            // change its decision; proxy tests intentionally permit that separate lookup.
            Ok(())
        }
    }

    /// Serves one canned response per accepted connection, recording each raw request head.
    /// Each response says `Connection: close`, so the client never reuses a connection this server has dropped.
    ///
    /// Returns the base URL and the shared log, so a test can assert on exactly what went
    /// over the wire rather than on what the builder code appears to do.
    async fn spawn_recording_server(responses: Vec<&'static str>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        tokio::spawn(async move {
            let mut index = 0usize;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                log.lock()
                    .expect("lock")
                    .push(String::from_utf8_lossy(&buf[..read]).to_string());
                let response = responses.get(index).copied().unwrap_or("HTTP/1.1 200 OK\r\n\r\n");
                index += 1;
                let response = response.replacen("\r\n", "\r\nConnection: close\r\n", 1);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        (format!("http://{addr}"), requests)
    }

    fn header_line(request: &str, name: &str) -> Option<String> {
        request
            .lines()
            .find(|line| line.to_lowercase().starts_with(&format!("{}:", name.to_lowercase())))
            .map(|line| line.split_once(':').expect("header line").1.trim().to_string())
    }

    /// Serves one canned response per accepted connection.
    async fn spawn_server(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        format!("http://{addr}")
    }

    fn client_with(validator: Arc<RecordingValidator>) -> HttpClient {
        HttpClient::with_ssrf(Arc::new(CookieJar::new()), None, validator, false)
            .expect("no proxy, so the client must build")
    }

    #[tokio::test]
    async fn system_proxy_refuses_a_hostname_the_policy_cannot_verify_at_connection_time() {
        use crate::net::resolver::tests::{TestProxySelector, denied_server};

        #[derive(Debug, Default)]
        struct RefuseRemoteNames {
            checked: Mutex<Vec<String>>,
        }

        #[async_trait::async_trait]
        impl SsrfValidator for RefuseRemoteNames {
            async fn validate(&self, _url: &Url) -> Result<(), String> {
                Ok(())
            }

            fn validate_remote_resolution(&self, url: &Url) -> Result<(), String> {
                self.checked.lock().expect("lock").push(url.to_string());
                match url.host() {
                    Some(url::Host::Domain(_)) => Err("configured network cannot be checked remotely".to_owned()),
                    _ => Ok(()),
                }
            }
        }

        let (proxy_port, proxy_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
        let selector = Arc::new(TestProxySelector::default());
        selector.set_proxy(&format!("http://localhost:{proxy_port}"));
        let policy = Arc::new(RefuseRemoteNames::default());
        let client = HttpClient::with_ssrf_and_proxy_selector(Arc::new(CookieJar::new()), policy.clone(), selector);

        let error = client
            .fetch(&"http://split.example/".parse::<Url>().expect("valid hostname URL"))
            .await
            .expect_err("the proxy's later hostname lookup cannot enforce the address policy");
        assert!(matches!(error, NetError::SsrfDenied(_)));
        assert!(
            proxy_requests.lock().expect("lock").is_empty(),
            "the refusal must happen before the request reaches the proxy"
        );

        client
            .fetch(&"http://198.51.100.1/".parse::<Url>().expect("valid literal URL"))
            .await
            .expect("a literal IP is checkable before the proxy request");
        assert_eq!(proxy_requests.lock().expect("lock").len(), 1);
        assert_eq!(policy.checked.lock().expect("lock").len(), 2);
    }

    #[tokio::test]
    async fn default_validator_refuses_a_proxy_hostname_unless_private_networks_are_allowed() {
        use crate::net::resolver::tests::{TestProxySelector, denied_server};

        let (proxy_port, proxy_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
        let selector = Arc::new(TestProxySelector::default());
        selector.set_proxy(&format!("http://localhost:{proxy_port}"));
        let target = "http://public.example/".parse::<Url>().expect("valid hostname URL");

        let denying = HttpClient::with_ssrf_and_proxy_selector(
            Arc::new(CookieJar::new()),
            Arc::new(DefaultSsrfValidator::with_deny_private(true)),
            selector.clone(),
        );
        let error = denying
            .fetch(&target)
            .await
            .expect_err("the default policy cannot verify the proxy's DNS answer");
        assert!(matches!(error, NetError::SsrfDenied(_)));
        assert!(
            proxy_requests.lock().expect("lock").is_empty(),
            "the default refusal must happen before the request reaches the proxy"
        );

        let permitting = HttpClient::with_ssrf_and_proxy_selector(
            Arc::new(CookieJar::new()),
            Arc::new(DefaultSsrfValidator::with_deny_private(false)),
            selector,
        );
        permitting
            .fetch(&target)
            .await
            .expect("the explicit private-network override permits remote DNS");
        assert_eq!(proxy_requests.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn environment_proxy_changes_are_applied_without_resolving_the_private_proxy_host() {
        use crate::net::resolver::tests::{RebindingPolicy, TestProxySelector, denied_server};

        let (first_port, first_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfirst").await;
        let (second_port, second_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond").await;
        let selector = Arc::new(TestProxySelector::default());
        selector.set_proxy(&format!("http://localhost:{first_port}"));
        let policy = Arc::new(RebindingPolicy::default());
        let client =
            HttpClient::with_ssrf_and_proxy_selector(Arc::new(CookieJar::new()), policy.clone(), selector.clone());
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
    async fn no_proxy_keeps_the_policy_resolver_on_a_direct_request() {
        use crate::net::resolver::tests::{RebindingPolicy, TestProxySelector, denied_server};

        let (target_port, target_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ntarget").await;
        let (proxy_port, proxy_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nproxy").await;
        let selector = Arc::new(TestProxySelector::default());
        selector.set_proxy(&format!("http://localhost:{proxy_port}"));
        selector.direct_host("localhost");
        let policy = Arc::new(RebindingPolicy::default());
        let client = HttpClient::with_ssrf_and_proxy_selector(Arc::new(CookieJar::new()), policy.clone(), selector);

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

    #[tokio::test]
    async fn environment_proxy_credentials_select_distinct_clients() {
        use crate::net::proxy::credentialed_proxy;
        use crate::net::resolver::tests::{RebindingPolicy, TestProxySelector};

        let (proxy, requests) = credentialed_proxy::start().await;
        let mut proxy_url = proxy.address().clone();
        proxy_url.set_username("operator").expect("proxy URL takes a username");
        proxy_url
            .set_password(Some(credentialed_proxy::PASSWORD))
            .expect("proxy URL takes a password");
        let selector = Arc::new(TestProxySelector::default());
        selector.set_proxy(proxy_url.as_str());
        let client = HttpClient::with_ssrf_and_proxy_selector(
            Arc::new(CookieJar::new()),
            Arc::new(RebindingPolicy::default()),
            selector.clone(),
        );
        let target = "http://example.invalid/page".parse::<Url>().expect("valid URL");

        let accepted = client
            .fetch(&target)
            .await
            .expect("correct credentials must be accepted");
        proxy_url
            .set_password(Some("wrong-password"))
            .expect("proxy URL takes a password");
        selector.set_proxy(proxy_url.as_str());
        let refused = client.fetch(&target).await.expect("a 407 is an HTTP response");

        assert_eq!((accepted.status, refused.status), (200, 407));
        let requests = requests.lock().expect("lock");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            credentialed_proxy::proxy_authorization(&requests[0]),
            Some(credentialed_proxy::expected_authorization())
        );
        assert_ne!(
            credentialed_proxy::proxy_authorization(&requests[0]),
            credentialed_proxy::proxy_authorization(&requests[1])
        );
    }

    #[tokio::test]
    async fn injected_validator_permits_loopback_without_any_env_var() {
        // ~keep The whole point of injection: reaching a private address is a policy
        // decision, not a process-wide environment toggle.
        let base = spawn_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi").await;
        let validator = Arc::new(RecordingValidator::default());
        let client = client_with(validator.clone());

        let response = client
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect("an allow-all injected policy must permit loopback");

        assert_eq!(response.status, 200, "expected the canned 200 response");
        assert_eq!(
            validator.seen.lock().expect("lock").len(),
            1,
            "the injected validator must be consulted exactly once for a non-redirected fetch"
        );
    }

    #[tokio::test]
    async fn injected_validator_denial_blocks_the_fetch() {
        let base = spawn_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi").await;
        let validator = Arc::new(RecordingValidator {
            seen: Mutex::new(Vec::new()),
            deny: Some("127.0.0.1".to_string()),
        });

        let err = client_with(validator)
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect_err("a denying policy must block the fetch");

        assert!(
            matches!(err, NetError::SsrfDenied(_)),
            "denial must be distinguishable from a transport failure, got {err:?}"
        );
    }

    #[tokio::test]
    async fn redirect_targets_are_revalidated() {
        // ~keep Hop 1 being permitted says nothing about where the chain ends up.
        let base =
            spawn_server("HTTP/1.1 302 Found\r\nLocation: http://10.0.0.1/admin\r\nContent-Length: 0\r\n\r\n").await;
        let validator = Arc::new(RecordingValidator {
            seen: Mutex::new(Vec::new()),
            deny: Some("10.0.0.1".to_string()),
        });
        let client = client_with(validator.clone());

        let err = client
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect_err("the redirect target must be refused");

        assert!(
            matches!(err, NetError::SsrfDenied(_)),
            "expected SsrfDenied for the redirect target, got {err:?}"
        );
        let seen = validator.seen.lock().expect("lock");
        assert_eq!(
            seen.len(),
            2,
            "both the initial URL and the redirect target must be checked"
        );
        assert!(
            seen[1].contains("10.0.0.1"),
            "the second check must be the redirect target, got {:?}",
            seen[1]
        );
    }

    #[tokio::test]
    async fn file_urls_are_refused_unless_explicitly_allowed() {
        let validator = Arc::new(RecordingValidator::default());
        let denied = client_with(validator.clone());
        let err = denied
            .fetch(&"file:///etc/passwd".parse::<Url>().expect("valid URL"))
            .await
            .expect_err("file access must be off by default");

        assert!(
            matches!(err, NetError::SsrfDenied(_)),
            "expected SsrfDenied for file://, got {err:?}"
        );
        assert!(
            validator.seen.lock().expect("lock").is_empty(),
            "file:// is decided before the network policy is consulted"
        );
    }

    /// Pins the full Chrome fingerprint header set as it appears on the wire.
    ///
    /// These values are a browser-emulation surface: a dropped or altered header changes how
    /// sites classify the crawler, and nothing else in the suite would notice.
    #[tokio::test]
    async fn every_default_request_header_is_sent_with_its_exact_value() {
        let (base, requests) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        client
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect("fetch must succeed");

        let sent = requests.lock().expect("lock")[0].clone();
        let expected = [
            (
                "user-agent",
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
            ),
            (
                "accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
            ),
            ("accept-language", "en-US,en;q=0.9"),
            (
                "sec-ch-ua",
                "\"Chromium\";v=\"145\", \"Not;A=Brand\";v=\"24\", \"Google Chrome\";v=\"145\"",
            ),
            ("sec-ch-ua-mobile", "?0"),
            ("sec-ch-ua-platform", "\"Linux\""),
            ("sec-fetch-dest", "document"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-site", "none"),
            ("sec-fetch-user", "?1"),
            ("upgrade-insecure-requests", "1"),
        ];
        for (name, value) in expected {
            assert_eq!(header_line(&sent, name).as_deref(), Some(value), "header {name}");
        }
        assert_eq!(header_line(&sent, "cookie"), None, "no cookies in an empty jar");
    }

    #[tokio::test]
    async fn a_custom_user_agent_replaces_the_default_one() {
        let (base, requests) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        client.set_user_agent("MyCrawler/1.0").await;
        client
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect("fetch must succeed");

        let sent = requests.lock().expect("lock")[0].clone();
        assert_eq!(header_line(&sent, "user-agent").as_deref(), Some("MyCrawler/1.0"));
    }

    #[tokio::test]
    async fn jar_cookies_are_attached_and_extra_headers_override_the_defaults() {
        let (base, requests) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let jar = Arc::new(CookieJar::new());
        let url: Url = base.parse().expect("valid URL");
        jar.set_cookie("session=abc", &url);
        let client = HttpClient::with_ssrf(jar, None, Arc::new(RecordingValidator::default()), false)
            .expect("no proxy, so the client must build");
        client
            .set_extra_headers(HashMap::from([
                ("x-custom".to_string(), "yes".to_string()),
                ("accept-language".to_string(), "de-DE".to_string()),
            ]))
            .await;
        client.fetch(&url).await.expect("fetch must succeed");

        let sent = requests.lock().expect("lock")[0].clone();
        assert_eq!(header_line(&sent, "cookie").as_deref(), Some("session=abc"));
        assert_eq!(header_line(&sent, "x-custom").as_deref(), Some("yes"));
        assert_eq!(
            header_line(&sent, "accept-language").as_deref(),
            Some("de-DE"),
            "extra headers are applied after the defaults and win"
        );
    }

    #[tokio::test]
    async fn a_set_cookie_response_header_is_stored_in_the_jar() {
        let (base, _requests) = spawn_recording_server(vec![
            "HTTP/1.1 200 OK\r\nSet-Cookie: got=1\r\nContent-Length: 0\r\n\r\n",
        ])
        .await;
        let jar = Arc::new(CookieJar::new());
        let url: Url = base.parse().expect("valid URL");
        let client = HttpClient::with_ssrf(jar.clone(), None, Arc::new(RecordingValidator::default()), false)
            .expect("no proxy, so the client must build");
        client.fetch(&url).await.expect("fetch must succeed");

        assert_eq!(jar.get_cookie_header(&url), "got=1");
    }

    struct FixedInterceptor(Mutex<Option<InterceptAction>>);

    #[async_trait::async_trait]
    impl RequestInterceptor for FixedInterceptor {
        async fn intercept(&self, _request: &RequestInfo) -> InterceptAction {
            self.0.lock().expect("lock").take().unwrap_or(InterceptAction::Continue)
        }
    }

    #[tokio::test]
    async fn an_intercepting_block_action_fails_the_fetch_without_a_request() {
        let (base, requests) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        *client.interceptor.write().await = Some(Box::new(FixedInterceptor(Mutex::new(Some(InterceptAction::Block)))));

        let err = client
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect_err("Block must fail the fetch");

        assert!(matches!(err, NetError::Blocked(_)), "expected Blocked, got {err:?}");
        assert!(requests.lock().expect("lock").is_empty(), "no request may be sent");
    }

    #[tokio::test]
    async fn an_intercepting_fulfill_action_short_circuits_with_the_canned_response() {
        let (base, requests) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let url: Url = base.parse().expect("valid URL");
        let canned = Response {
            url: url.clone(),
            status: 418,
            headers: HashMap::new(),
            body: b"teapot".to_vec(),
            redirected_from: Vec::new(),
        };
        let client = client_with(Arc::new(RecordingValidator::default()));
        *client.interceptor.write().await = Some(Box::new(FixedInterceptor(Mutex::new(Some(
            InterceptAction::Fulfill(canned),
        )))));

        let response = client.fetch(&url).await.expect("Fulfill must succeed");
        assert_eq!(response.status, 418);
        assert_eq!(response.body, b"teapot".to_vec());
        assert!(requests.lock().expect("lock").is_empty(), "no request may be sent");
    }

    #[tokio::test]
    async fn an_intercepting_modify_headers_action_merges_into_the_extra_headers() {
        let (base, requests) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        *client.interceptor.write().await = Some(Box::new(FixedInterceptor(Mutex::new(Some(
            InterceptAction::ModifyHeaders(HashMap::from([("x-injected".to_string(), "1".to_string())])),
        )))));

        client
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect("Continue-like action must proceed");

        let sent = requests.lock().expect("lock")[0].clone();
        assert_eq!(header_line(&sent, "x-injected").as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn a_302_redirect_downgrades_post_to_get_and_drops_the_body() {
        let (base, requests) = spawn_recording_server(vec![
            "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ])
        .await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        let response = client
            .post_form(&base.parse::<Url>().expect("valid URL"), "a=1")
            .await
            .expect("redirect must be followed");

        assert_eq!(response.status, 200);
        assert_eq!(response.redirected_from.len(), 1);
        let sent = requests.lock().expect("lock").clone();
        assert!(
            sent[0].starts_with("POST /"),
            "first hop is the POST, got {:?}",
            &sent[0][..16]
        );
        assert!(
            sent[1].starts_with("GET /next"),
            "302 downgrades to GET, got {:?}",
            &sent[1][..16]
        );
        assert!(!sent[1].contains("a=1"), "the POST body must not be replayed");
    }

    #[tokio::test]
    async fn a_307_redirect_preserves_the_post_method_and_body() {
        let (base, requests) = spawn_recording_server(vec![
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ])
        .await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        client
            .post_form(&base.parse::<Url>().expect("valid URL"), "a=1")
            .await
            .expect("redirect must be followed");

        let sent = requests.lock().expect("lock").clone();
        assert!(
            sent[1].starts_with("POST /next"),
            "307 preserves POST, got {:?}",
            &sent[1][..16]
        );
        assert!(sent[1].contains("a=1"), "307 replays the body");
        assert_eq!(
            header_line(&sent[1], "content-type").as_deref(),
            Some("application/x-www-form-urlencoded")
        );
    }

    #[tokio::test]
    async fn a_redirect_loop_stops_at_the_hop_limit() {
        let looping = "HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n";
        let (loop_base, loop_requests) = spawn_recording_server(vec![looping; MAX_REDIRECTS + 2]).await;

        let err = client_with(Arc::new(RecordingValidator::default()))
            .fetch(&loop_base.parse::<Url>().expect("valid URL"))
            .await
            .expect_err("an endless redirect chain must terminate");

        assert!(
            matches!(err, NetError::TooManyRedirects(_)),
            "expected TooManyRedirects, got {err:?}"
        );
        assert_eq!(
            loop_requests.lock().expect("lock").len(),
            MAX_REDIRECTS,
            "exactly {MAX_REDIRECTS} hops are attempted before giving up"
        );
    }

    #[tokio::test]
    async fn a_limit_above_the_hop_cap_follows_the_whole_chain() {
        let hop = "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n";
        let (base, requests) = spawn_recording_server(vec![hop; MAX_REDIRECTS + 5]).await;

        let resp = client_with(Arc::new(RecordingValidator::default()))
            .fetch_following(
                reqwest::Method::GET,
                &base.parse::<Url>().expect("valid URL"),
                None,
                Some(30),
            )
            .await
            .expect("a limit of 30 must follow a chain of 25 redirects");

        assert_eq!(
            (resp.status, resp.redirected_from.len()),
            (200, MAX_REDIRECTS + 5),
            "the limit, not the cap of {MAX_REDIRECTS} hops, bounds the chain"
        );
        assert_eq!(requests.lock().expect("lock").len(), MAX_REDIRECTS + 6);
    }

    #[tokio::test]
    async fn request_and_response_callbacks_fire_for_each_hop() {
        let (base, _requests) = spawn_recording_server(vec![
            "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ])
        .await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        let request_hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let response_hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = request_hits.clone();
        client.on_request.write().await.push(Arc::new(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));
        let seen = response_hits.clone();
        client.on_response.write().await.push(Arc::new(move |_, _| {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));

        client
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect("fetch must succeed");

        assert_eq!(
            request_hits.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "on_request fires once per hop, redirect included"
        );
        assert_eq!(
            response_hits.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "on_response fires only for the final, non-redirect response"
        );
    }

    #[tokio::test]
    async fn in_flight_returns_to_zero_after_a_fetch() {
        let (base, _requests) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        assert!(client.is_network_idle());
        client
            .fetch(&base.parse::<Url>().expect("valid URL"))
            .await
            .expect("fetch must succeed");
        assert_eq!(client.active_requests(), 0);
    }

    fn proxied_client(proxy: &str) -> Result<HttpClient, NetError> {
        let proxy = crate::net::proxy::test_proxy(proxy)?;
        HttpClient::with_ssrf(
            Arc::new(CookieJar::new()),
            Some(&proxy),
            Arc::new(RecordingValidator::default()),
            false,
        )
    }

    #[tokio::test]
    async fn a_credentialed_proxy_carries_the_request_with_its_credentials() {
        use crate::net::proxy::credentialed_proxy;
        let (proxy, requests) = credentialed_proxy::start().await;
        let client = HttpClient::with_ssrf(
            Arc::new(CookieJar::new()),
            Some(&proxy),
            Arc::new(RecordingValidator::default()),
            false,
        )
        .expect("an http proxy must build");

        let response = client
            .fetch(&"http://origin.test/page".parse::<Url>().expect("valid URL"))
            .await
            .expect("the proxy accepts the credentials, so the fetch must succeed");

        assert_eq!(response.body, b"via-proxy");
        credentialed_proxy::assert_one_authenticated_request(&requests, "http://origin.test/page");
    }

    #[tokio::test]
    async fn a_proxy_that_refuses_the_credentials_fails_the_fetch_instead_of_connecting_directly() {
        use crate::net::proxy::credentialed_proxy;
        let (proxy, requests) = credentialed_proxy::start().await;
        let (target, direct) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\ndirect"]).await;
        let client = HttpClient::with_ssrf(
            Arc::new(CookieJar::new()),
            Some(&credentialed_proxy::with_wrong_password(&proxy)),
            Arc::new(RecordingValidator::default()),
            false,
        )
        .expect("an http proxy must build");

        let result = client
            .fetch(&format!("{target}/page").parse::<Url>().expect("valid URL"))
            .await;

        assert!(
            !matches!(result, Ok(ref response) if response.status == 200),
            "a refused proxy must not serve the page: {result:?}"
        );
        assert!(direct.lock().expect("lock").is_empty(), "the fetch connected directly");
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
        let (proxy, requests) =
            spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nvia-proxy"]).await;
        let client = proxied_client(&proxy).expect("an http proxy must build");

        let response = client
            .fetch(&"http://origin.test/page".parse::<Url>().expect("valid URL"))
            .await
            .expect("the proxy answers, so the fetch must succeed");

        assert_eq!(response.body, b"via-proxy");
        let requests = requests.lock().expect("lock");
        assert!(
            requests
                .first()
                .is_some_and(|r| r.starts_with("GET http://origin.test/page ")),
            "the proxy must receive the absolute-form request, got {requests:?}"
        );
    }

    #[tokio::test]
    async fn a_rebinding_host_never_reaches_the_address_the_policy_denies() {
        use crate::net::resolver::tests::{RebindingPolicy, denied_server};
        use crate::page::PageError;

        let (port, seen) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nDENIED").await;
        let policy = Arc::new(RebindingPolicy::default());
        let client = HttpClient::with_ssrf(Arc::new(CookieJar::new()), None, policy.clone(), false)
            .expect("no proxy, so the client must build");

        let err = client
            .fetch(&format!("http://localhost:{port}/").parse::<Url>().expect("valid URL"))
            .await
            .expect_err("the connection's lookup answers a denied address");

        let NetError::Network(message) = &err else {
            panic!("expected a refused connection, got {err:?}");
        };
        assert!(
            message.contains("denied by the test policy: 127.0.0.1"),
            "the refusal must carry the policy's reason: {message}"
        );
        assert!(
            !PageError::from(err)
                .to_string()
                .contains("Network error: Network error"),
            "a page error names the network once"
        );
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
    async fn a_redirect_to_a_rebinding_host_never_reaches_the_address_the_policy_denies() {
        use crate::net::resolver::tests::{RebindingPolicy, denied_server};

        let (port, seen) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nDENIED").await;
        let redirect: &'static str = Box::leak(
            format!("HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/away\r\nContent-Length: 0\r\n\r\n")
                .into_boxed_str(),
        );
        let (start, start_requests) = spawn_recording_server(vec![redirect]).await;
        let policy = Arc::new(RebindingPolicy::default());
        let client = HttpClient::with_ssrf(Arc::new(CookieJar::new()), None, policy.clone(), false)
            .expect("no proxy, so the client must build");

        client
            .fetch(&start.parse::<Url>().expect("valid URL"))
            .await
            .expect_err("the redirect target's lookup answers a denied address");

        assert_eq!(
            start_requests.lock().expect("lock").len(),
            1,
            "the first hop is fetched"
        );
        assert!(
            seen.lock().expect("lock").is_empty(),
            "the denied address must receive no connection: {:?}",
            seen.lock().expect("lock")
        );
        assert_eq!(*policy.resolved.lock().expect("lock"), vec!["localhost"]);
    }

    #[tokio::test]
    async fn a_proxied_client_leaves_the_target_to_the_proxy() {
        use crate::net::resolver::tests::RebindingPolicy;

        let (proxy, proxy_requests) =
            spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let policy = Arc::new(RebindingPolicy::default());
        // ~keep A proxy named by host: a client that asked the policy for it would be refused.
        let proxy = proxy.replacen("127.0.0.1", "localhost", 1);
        let proxy = crate::net::proxy::test_proxy(&proxy).expect("an http proxy");
        let client = HttpClient::with_ssrf(Arc::new(CookieJar::new()), Some(&proxy), policy.clone(), false)
            .expect("an http proxy must build");

        client
            .fetch(&"http://example.invalid/".parse::<Url>().expect("valid URL"))
            .await
            .expect("the proxy answers the request");

        let proxy_requests = proxy_requests.lock().expect("lock");
        assert!(
            proxy_requests[0].starts_with("GET http://example.invalid/ "),
            "the request goes to the proxy: {proxy_requests:?}"
        );
        assert!(
            policy.resolved.lock().expect("lock").is_empty(),
            "the proxy resolves the target, so the client must not"
        );
    }

    const URL_PASSWORD: &str = "s3cret";

    /// `base` with `user:s3cret@` userinfo.
    fn with_userinfo(base: &str) -> String {
        base.replacen("http://", &format!("http://user:{URL_PASSWORD}@"), 1)
    }

    #[tokio::test]
    async fn a_url_with_userinfo_is_refused_before_the_network() {
        let (base, requests) = spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let validator = Arc::new(RecordingValidator::default());
        let client = client_with(validator.clone());

        let err = client
            .fetch(&with_userinfo(&base).parse::<Url>().expect("valid URL"))
            .await
            .expect_err("a URL with userinfo must be refused");

        let NetError::Blocked(message) = &err else {
            panic!("expected NetError::Blocked, got {err:?}");
        };
        assert!(
            !message.contains(URL_PASSWORD),
            "the password must not be named, got '{message}'"
        );
        // ~keep Positive twin: the refusal names the URL without its userinfo, so the test
        // ~keep cannot pass on an empty message.
        assert!(
            message.contains(&base),
            "the refusal must name the clean URL, got '{message}'"
        );
        assert!(
            requests.lock().expect("lock").is_empty(),
            "nothing may reach the network"
        );
    }

    #[tokio::test]
    async fn a_redirect_location_with_userinfo_is_followed_without_it() {
        let (target, target_requests) =
            spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let redirect: &'static str = Box::leak(
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {}/next\r\nContent-Length: 0\r\n\r\n",
                with_userinfo(&target)
            )
            .into_boxed_str(),
        );
        let (start, _) = spawn_recording_server(vec![redirect]).await;

        let response = client_with(Arc::new(RecordingValidator::default()))
            .fetch(&start.parse::<Url>().expect("valid URL"))
            .await
            .expect("the redirect must be followed");

        assert_eq!(response.url.as_str(), format!("{target}/next"));
        let requests = target_requests.lock().expect("lock");
        assert_eq!(requests.len(), 1, "the redirect target must be requested once");
        assert_eq!(
            header_line(&requests[0], "authorization"),
            None,
            "a page-supplied userinfo must never become a header"
        );
    }

    #[tokio::test]
    async fn the_origin_headers_reach_their_host_and_no_other() {
        let (other, other_requests) =
            spawn_recording_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]).await;
        let other_port = other.rsplit(':').next().expect("port").to_owned();
        let redirect: &'static str = Box::leak(
            format!("HTTP/1.1 302 Found\r\nLocation: http://localhost:{other_port}/away\r\nContent-Length: 0\r\n\r\n")
                .into_boxed_str(),
        );
        let (start, start_requests) = spawn_recording_server(vec![redirect]).await;
        let client = client_with(Arc::new(RecordingValidator::default()));
        client
            .set_origin_headers(Some(OriginHeaders {
                host: "127.0.0.1".to_owned(),
                headers: vec![("Authorization".to_owned(), "Basic dXNlcjpwdw==".to_owned())],
            }))
            .await;

        client
            .fetch(&start.parse::<Url>().expect("valid URL"))
            .await
            .expect("the redirect must be followed");

        let start_requests = start_requests.lock().expect("lock");
        assert_eq!(
            header_line(&start_requests[0], "authorization").as_deref(),
            Some("Basic dXNlcjpwdw=="),
            "the scoped host gets the header"
        );
        let other_requests = other_requests.lock().expect("lock");
        assert_eq!(other_requests.len(), 1, "the cross-host redirect must be followed");
        assert_eq!(
            header_line(&other_requests[0], "authorization"),
            None,
            "a cross-host redirect target never gets the header"
        );
    }
}
