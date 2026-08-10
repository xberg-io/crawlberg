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
            user_agent: RwLock::new(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
                    .to_string(),
            ),
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
        let max_redirects = 20;

        for _redirect_count in 0..max_redirects {
            let request_info = RequestInfo {
                url: current_url.clone(),
                method: method.to_string(),
                headers: self.extra_headers.read().await.clone(),
                resource_type: ResourceType::Document,
            };

            if let Some(interceptor) = self.interceptor.read().await.as_ref() {
                match interceptor.intercept(&request_info).await {
                    InterceptAction::Continue => {}
                    InterceptAction::Block => {
                        return Err(NetError::Blocked(current_url.to_string()));
                    }
                    InterceptAction::Fulfill(response) => {
                        return Ok(response);
                    }
                    InterceptAction::ModifyHeaders(headers) => {
                        let mut extra = self.extra_headers.write().await;
                        extra.extend(headers);
                    }
                }
            }

            for cb in self.on_request.read().await.iter() {
                cb(&request_info);
            }

            let ua = self.user_agent.read().await.clone();
            let mut headers = HeaderMap::new();
            headers.insert(USER_AGENT, HeaderValue::from_str(&ua).unwrap_or_else(|_| {
                HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36")
            }));
            headers.insert(
                reqwest::header::ACCEPT,
                HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"),
            );
            headers.insert(
                reqwest::header::ACCEPT_LANGUAGE,
                HeaderValue::from_static("en-US,en;q=0.9"),
            );
            headers.insert(
                HeaderName::from_static("sec-ch-ua"),
                HeaderValue::from_static(
                    "\"Chromium\";v=\"145\", \"Not;A=Brand\";v=\"24\", \"Google Chrome\";v=\"145\"",
                ),
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

            let cookie_header = self.cookie_jar.get_cookie_header(&current_url);
            if !cookie_header.is_empty()
                && let Ok(val) = HeaderValue::from_str(&cookie_header)
            {
                headers.insert(reqwest::header::COOKIE, val);
            }

            for (k, v) in self.extra_headers.read().await.iter() {
                if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                    headers.insert(name, val);
                }
            }

            let mut req_builder = self
                .get_client()
                .await
                .request(method.clone(), current_url.as_str())
                .headers(headers);

            if let Some(ref b) = body {
                if method == Method::POST {
                    req_builder =
                        req_builder.header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded");
                }
                req_builder = req_builder.body(b.clone());
            }

            self.in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let resp = req_builder.send().await.map_err(|e| {
                self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                NetError::Network(format!("{}: {}", current_url, e))
            })?;
            self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            let status = resp.status();

            for val in resp.headers().get_all(reqwest::header::SET_COOKIE) {
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
                && let Some(location) = resp.headers().get(reqwest::header::LOCATION)
            {
                let location_str = location
                    .to_str()
                    .map_err(|_| NetError::Network("Invalid redirect Location header".into()))?;
                let next_url = current_url
                    .join(location_str)
                    .map_err(|e| NetError::Network(format!("Invalid redirect URL: {}", e)))?;
                self.validate_url(&next_url).await?;
                redirects.push(current_url.clone());
                current_url = next_url;
                if status == reqwest::StatusCode::MOVED_PERMANENTLY
                    || status == reqwest::StatusCode::FOUND
                    || status == reqwest::StatusCode::SEE_OTHER
                {
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
}
