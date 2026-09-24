use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::{Client, Method};
use tokio::sync::RwLock;
use url::Url;

use crate::net::cookies::CookieJar;
use crate::net::interceptor::{InterceptAction, RequestInterceptor};
use crate::net::ssrf::{DefaultSsrfValidator, SsrfValidator};

#[derive(Debug, Clone)]
pub struct Response {
    pub url: Url,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub redirected_from: Vec<Url>,
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

#[derive(Debug, Clone)]
pub struct RequestInfo {
    pub url: Url,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub resource_type: ResourceType,
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
    proxy_url: Option<String>,
    /// SSRF policy applied to the initial URL and every redirect hop.
    pub ssrf: Arc<dyn SsrfValidator>,
    /// Whether `file://` URLs may be fetched. Off unless the embedder opts in.
    pub allow_file_access: bool,
    pub cookie_jar: Arc<CookieJar>,
    pub user_agent: RwLock<String>,
    pub extra_headers: RwLock<HashMap<String, String>>,
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
        Self::with_options(cookie_jar, None)
    }

    pub fn with_options(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        Self::with_ssrf(cookie_jar, proxy_url, Arc::new(DefaultSsrfValidator::from_env()), false)
    }

    /// Build a client with an explicit SSRF policy.
    ///
    /// `crawlberg` uses this to inject the crawl's configured policy — including its
    /// allowlist — in place of the deny-list-only default.
    pub fn with_ssrf(
        cookie_jar: Arc<CookieJar>,
        proxy_url: Option<&str>,
        ssrf: Arc<dyn SsrfValidator>,
        allow_file_access: bool,
    ) -> Self {
        HttpClient {
            client: tokio::sync::OnceCell::new(),
            proxy_url: proxy_url.map(|s| s.to_string()),
            ssrf,
            allow_file_access,
            cookie_jar,
            user_agent: RwLock::new(DEFAULT_USER_AGENT.to_string()),
            extra_headers: RwLock::new(HashMap::new()),
            interceptor: RwLock::new(None),
            on_request: RwLock::new(Vec::new()),
            on_response: RwLock::new(Vec::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            timeout: Duration::from_secs(30),
        }
    }

    async fn get_client(&self) -> &Client {
        self.client
            .get_or_init(|| async {
                let mut builder = Client::builder()
                    .redirect(Policy::none())
                    .timeout(Duration::from_secs(30))
                    .danger_accept_invalid_certs(false);

                if let Some(ref proxy) = self.proxy_url
                    && let Ok(p) = reqwest::Proxy::all(proxy.as_str())
                {
                    builder = builder.proxy(p);
                }

                builder.build().expect("failed to build HTTP client")
            })
            .await
    }

    /// Read-only accessor for the proxy URL the client was configured with
    /// (if any). Exposed so the JS fetch bridge can route its own reqwest
    /// requests through the same upstream proxy.
    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
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
        self.validate_url(url).await?;

        if url.scheme() == "file" {
            return fetch_file_url(url).await;
        }

        let mut method = initial_method;
        let mut body = initial_body;

        let mut current_url = url.clone();
        let mut redirects = Vec::new();

        for _redirect_count in 0..MAX_REDIRECTS {
            let request_info = self.request_info(&current_url, &method).await;

            if let Some(response) = self.apply_interceptor(&request_info).await? {
                return Ok(response);
            }

            for cb in self.on_request.read().await.iter() {
                cb(&request_info);
            }

            let headers = self.request_headers(&current_url).await;
            let resp = self.send_request(&current_url, &method, body.as_ref(), headers).await?;

            let status = resp.status();
            self.store_response_cookies(&resp, &current_url);
            let response_headers = collect_response_headers(&resp);

            if status.is_redirection()
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

    fn store_response_cookies(&self, response: &reqwest::Response, url: &Url) {
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(set_cookie) = value.to_str() {
                self.cookie_jar.set_cookie(set_cookie, url);
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
    async fn request_headers(&self, url: &Url) -> HeaderMap {
        let mut headers = browser_fingerprint_headers(&self.user_agent.read().await.clone());

        let cookie_header = self.cookie_jar.get_cookie_header(url);
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
            .get_client()
            .await
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
            NetError::Network(format!("{}: {}", url, e))
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

    pub fn active_requests(&self) -> u32 {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_network_idle(&self) -> bool {
        self.active_requests() == 0
    }
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
    }

    /// Serves one canned response per accepted connection, recording each raw request head.
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
        let client = HttpClient::with_ssrf(jar, None, Arc::new(RecordingValidator::default()), false);
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
        let client = HttpClient::with_ssrf(jar.clone(), None, Arc::new(RecordingValidator::default()), false);
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
}
