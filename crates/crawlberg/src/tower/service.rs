//! Base HTTP fetch service (innermost in the Tower stack).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tower::Service;

use super::types::{CrawlRequest, CrawlResponse, ResponseBody};
use crate::error::{CrawlError, classify_reqwest_error};
use crate::net::credentials::seed_host_headers;
use crate::net::ssrf::validate_url;
use crate::types::CrawlConfig;

/// Innermost Tower service that performs the actual HTTP fetch.
#[derive(Clone)]
pub struct HttpFetchService {
    client: reqwest::Client,
    config: Arc<CrawlConfig>,
    /// The cookies of the crawl this service fetches for, when that crawl keeps cookies.
    #[cfg(feature = "browser")]
    crawl_cookies: Option<Arc<crate::browser::CrawlCookies>>,
    #[cfg(feature = "browser-native")]
    native_cookies: Option<crawlberg_browser::adapter::NativeRenderState>,
}

impl HttpFetchService {
    pub fn new(client: reqwest::Client, config: CrawlConfig) -> Self {
        Self {
            client,
            config: Arc::new(config),
            #[cfg(feature = "browser")]
            crawl_cookies: None,
            #[cfg(feature = "browser-native")]
            native_cookies: None,
        }
    }

    #[cfg(feature = "browser-native")]
    pub(crate) fn for_native_crawl(mut self, cookies: Option<crawlberg_browser::adapter::NativeRenderState>) -> Self {
        self.native_cookies = cookies;
        self
    }

    /// Fetch for the crawl that has `crawl_cookies`: each response reports the cookies it set
    /// there as soon as its headers arrive.
    #[cfg(feature = "browser")]
    pub(crate) fn for_crawl(mut self, crawl_cookies: Option<Arc<crate::browser::CrawlCookies>>) -> Self {
        self.crawl_cookies = crawl_cookies;
        self
    }
}

/// The `User-Agent` value this request will actually send.
///
/// ~keep `crawl_req.headers` wins when it already names one -- set explicitly by a caller
/// (`RedirectPolicy::admits`, pinning the agent it picked for this hop) or by the UA rotation
/// layer upstream of this service. Callers that need to judge robots rules against the agent a
/// request actually sent (crawlberg#423) must read this same value back off the response
/// (`CrawlResponse::sent_user_agent`) rather than recomputing the engine's configured default.
fn resolved_user_agent(config: &CrawlConfig, crawl_req: &CrawlRequest, url: &url::Url) -> String {
    crate::helpers::request_user_agent(config, url, crawl_req.headers.get("user-agent").map(String::as_str)).to_owned()
}

fn apply_headers(
    mut req: reqwest::RequestBuilder,
    config: &CrawlConfig,
    crawl_req: &CrawlRequest,
    url: &url::Url,
    sent_user_agent: &str,
) -> reqwest::RequestBuilder {
    req = req.header(reqwest::header::USER_AGENT, sent_user_agent);

    // ~keep A `user-agent` custom header is already reflected in `sent_user_agent`
    // (`request_user_agent` scopes it, crawlberg#423), which the line above already
    // set on the request; re-adding it here would append a second, redundant `User-Agent`
    // header line rather than replacing the first one.
    for (name, value) in seed_host_headers(config, url) {
        if name.eq_ignore_ascii_case("user-agent") {
            continue;
        }
        req = req.header(name.as_str(), value.as_str());
    }

    for (k, v) in &crawl_req.headers {
        if !k.eq_ignore_ascii_case("user-agent") {
            req = req.header(k.as_str(), v.as_str());
        }
    }

    req
}

/// HTTP status codes in `[REDIRECT_STATUS_MIN, REDIRECT_STATUS_MAX)` are returned to the caller
/// unclassified so that redirect handling stays caller-owned.
const REDIRECT_STATUS_MIN: u16 = 300;
const REDIRECT_STATUS_MAX: u16 = 400;

/// Bytes by which a body may fall short of `content-length` before it counts as data loss.
///
/// ~keep A small shortfall is routinely produced by servers that miscount a compressed or
/// chunked body, so only a clearly truncated transfer is reported.
const CONTENT_LENGTH_SHORTFALL_TOLERANCE: usize = 100;

/// Whether a status is a redirect that `do_fetch` returns to the caller unclassified.
fn is_redirect_status(status: u16) -> bool {
    (REDIRECT_STATUS_MIN..REDIRECT_STATUS_MAX).contains(&status)
}

/// Read the last `content-type` header, or an empty string when absent or non-UTF-8.
fn content_type_of(resp: &reqwest::Response) -> String {
    resp.headers()
        .get_all(reqwest::header::CONTENT_TYPE)
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

/// Build the `CrawlResponse` for a 3xx without classifying it; a failed body read yields an
/// empty body rather than an error, because the caller only needs the status and headers.
async fn read_redirect_response(
    resp: reqwest::Response,
    config: &CrawlConfig,
    status: u16,
    content_type: String,
    headers: HashMap<String, Vec<String>>,
    sent_user_agent: String,
) -> CrawlResponse {
    let (body_bytes, _) = crate::http::read_body_bounded(resp, crate::http::effective_max_body_size(config))
        .await
        .unwrap_or_default();
    CrawlResponse::new(status, content_type, headers, ResponseBody::Bytes(body_bytes))
        .with_sent_user_agent(Some(sent_user_agent))
}

/// Whether an error chain names a truncated or failed body transfer rather than a transport fault.
fn is_body_error_chain(chain: &str) -> bool {
    chain.contains("content-length")
        || chain.contains("truncate")
        || chain.contains("incomplete")
        || chain.contains("end of file")
        || chain.contains("body error")
        || chain.contains("body from connection")
        || chain.contains("decoding response body")
        || chain.contains("error decoding")
}

/// Classify a failed body read as data loss where the chain says so, else as a transport error.
fn classify_body_read_error(e: reqwest::Error) -> CrawlError {
    if e.is_timeout() {
        return classify_reqwest_error(e);
    }
    let chain = crate::error::error_chain_string(&e);
    let is_body_error = is_body_error_chain(&chain);
    #[cfg(not(target_arch = "wasm32"))]
    let is_body_error = is_body_error || e.is_body();
    if is_body_error {
        CrawlError::data_loss_with_source(e.to_string(), e)
    } else {
        classify_reqwest_error(e)
    }
}

/// Report data loss when a body stops materially short of its declared `content-length`.
fn content_length_shortfall_error(
    headers: &HashMap<String, Vec<String>>,
    body_len: usize,
    hit_cap: bool,
) -> Option<CrawlError> {
    // ~keep A capped read stopping short of `content-length` is expected (that is the
    // point of `max_body_size`), not evidence of a truncated/failed transfer.
    if hit_cap {
        return None;
    }
    let expected = headers
        .get("content-length")
        .and_then(|v| v.first())
        .and_then(|s| s.parse::<usize>().ok())?;
    if body_len < expected && expected - body_len > CONTENT_LENGTH_SHORTFALL_TOLERANCE {
        return Some(CrawlError::data_loss(format!(
            "expected {expected} bytes, got {body_len}"
        )));
    }
    None
}

#[cfg(feature = "browser-native")]
fn apply_native_cookies(
    http_req: reqwest::RequestBuilder,
    service: &HttpFetchService,
    req: &CrawlRequest,
    url: &url::Url,
) -> reqwest::RequestBuilder {
    let config = &*service.config;
    if let Some(cookies) = &service.native_cookies {
        let explicit = req.headers.keys().any(|name| name.eq_ignore_ascii_case("cookie"))
            || seed_host_headers(config, url)
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cookie"));
        if explicit {
            http_req
        } else {
            // ~keep An empty header suppresses the HTTP jar too: a deleted native cookie
            // ~keep must not be revived by an older HTTP-only copy.
            http_req.header(reqwest::header::COOKIE, cookies.request_cookie_header(url))
        }
    } else {
        http_req
    }
}

#[cfg(any(feature = "browser", feature = "browser-native"))]
fn absorb_response_cookies(service: &HttpFetchService, url: &url::Url, headers: &HashMap<String, Vec<String>>) {
    #[cfg(feature = "browser-native")]
    if let Some(cookies) = &service.native_cookies {
        for raw in headers.get("set-cookie").into_iter().flatten() {
            cookies.store_response_cookie(raw, url);
        }
    }

    // ~keep The crawl's cookies are the one source a browser page reads, and this is the one place
    // ~keep a response fetched without the browser arrives. It reports the cookies it set here,
    // ~keep before its status can turn it into an error, a redirect, a retry or a browser fetch
    // ~keep of the same address: none of those can read the crawl's cookies first.
    #[cfg(feature = "browser")]
    if let Some(crawl_cookies) = service.crawl_cookies.as_deref() {
        crawl_cookies.absorb_http_response(url.as_str(), headers);
    }
}

/// Perform a single HTTP fetch (no retry, no redirect following) with SSRF validation.
///
/// Returns the raw response — including any 3xx — without following redirects.
/// Redirect resolution is the responsibility of the caller (`follow_redirects` in
/// `crawl_loop.rs`), which drives the hop loop and updates `current_url` so that
/// `final_url` in the `RedirectOutcome` is correct.
///
/// SSRF validation is applied to the requested URL before the fetch. Per-hop
/// validation of redirect targets is performed by `follow_redirects` before it
/// calls `fetch_response` with the next hop URL (which re-enters `do_fetch` and
/// re-validates the new URL here).
async fn do_fetch(service: &HttpFetchService, req: &CrawlRequest) -> Result<CrawlResponse, CrawlError> {
    let (client, config) = (&service.client, &*service.config);
    let url =
        url::Url::parse(&req.url).map_err(|e| CrawlError::ssrf_violation(&req.url, format!("invalid URL: {e}")))?;

    crate::net::userinfo::refuse(&url)?;
    validate_url(&url, &config.ssrf)
        .await
        .map_err(|e| CrawlError::ssrf_violation(req.url.clone(), e.to_string()))?;

    let sent_user_agent = resolved_user_agent(config, req, &url);
    let client = crate::http::request_client(client, config, &url)?;
    let http_req = apply_headers(client.get(url.to_string()), config, req, &url, &sent_user_agent);
    #[cfg(feature = "browser-native")]
    let http_req = apply_native_cookies(http_req, service, req, &url);

    // ~keep reqwest uses Policy::none(); redirect following is explicit and policy-checked by callers.
    let resp = crate::http::send_with_cookies(http_req, config)
        .await
        .map_err(classify_reqwest_error)?;

    crate::http::validate_content_encoding(resp.headers())?;
    let status = resp.status().as_u16();
    let content_type = content_type_of(&resp);
    let headers = crate::http::build_headers_map(resp.headers());
    #[cfg(any(feature = "browser", feature = "browser-native"))]
    absorb_response_cookies(service, &url, &headers);

    // ~keep Return 3xx responses as-is so redirect handling stays caller-owned.
    if is_redirect_status(status) {
        return Ok(read_redirect_response(resp, config, status, content_type, headers, sent_user_agent).await);
    }

    // ~keep Read error bodies before classification so soft results retain origin content,
    // ~keep while the original error chain still drives WAF escalation and retry policy.
    if config.soft_http_errors
        && (crate::http::is_challenge_status(status) || crate::http::status_error(status, &req.url).is_some())
    {
        let (body, truncated) = match crate::http::read_body_bounded(resp, crate::http::effective_max_body_size(config))
            .await
        {
            Ok(read) => read,
            Err(error) => {
                tracing::warn!(url = %req.url, status, %error, "failed to read the body of an error page; using an empty one");
                Default::default()
            }
        };
        if truncated {
            tracing::warn!(url = %req.url, status, "error page body truncated at the body size limit");
        }
        return Err(crate::http::status_error_keeping_response(
            status,
            &req.url,
            content_type,
            headers,
            body,
        ));
    }
    // ~keep Shares `http::challenge_status_error` with `http::fetch_one_hop` rather than keeping
    // a second copy: the two copies had already drifted. The one that stood here classified
    // every response as if it were a 403 and showed the classifier only the `server` header, so
    // a 403 identified by any other header came back as vendor "unknown".
    if crate::http::is_challenge_status(status) {
        return Err(crate::http::challenge_status_error(
            status,
            &req.url,
            &headers,
            resp,
            crate::http::effective_max_body_size(config),
        )
        .await);
    }
    if let Some(error) = crate::http::status_error(status, &req.url) {
        return Err(error);
    }

    let response_url = resp.url().clone();
    let (body_vec, hit_cap) = crate::http::read_body_bounded(resp, crate::http::effective_max_body_size(config))
        .await
        .map_err(|error| classify_body_read_error(error.with_url(response_url)))?;

    if let Some(error) = content_length_shortfall_error(&headers, body_vec.len(), hit_cap) {
        return Err(error);
    }

    let response = CrawlResponse::new(status, content_type, headers, ResponseBody::Bytes(body_vec))
        .with_sent_user_agent(Some(sent_user_agent));

    // ~keep The same 2xx decision `http::fetch_one_hop` makes, so the engine's crawl and the plain
    // fetch refuse exactly the same responses (crawlberg#231).
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(error) = crate::http::waf_2xx_error(status, &response.body_bytes, &response.body, &response.headers) {
        return Err(error);
    }

    Ok(response)
}

impl Service<CrawlRequest> for HttpFetchService {
    type Response = CrawlResponse;
    type Error = CrawlError;
    type Future = Pin<Box<dyn Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: CrawlRequest) -> Self::Future {
        let service = self.clone();

        // ~keep A single fetch attempt only: retries are owned by the dispatch loop
        // ~keep (`engine::fetch::run_dispatch_loop`, via `SimpleRetryPolicy::from_config`), which
        // ~keep re-invokes `run_tier` — and therefore this service — once per attempt, honouring
        // ~keep `retry_count`/`retry_codes` exactly once. Previously this service ran its own
        // ~keep `0..=retry_count` loop *inside* the dispatch loop's own retry loop, multiplying
        // ~keep attempts (retry_count=4 produced 20 requests instead of 5). A direct
        // ~keep `tower::Service` consumer of `HttpFetchService` that bypasses the dispatch loop
        // ~keep now gets no retries at all; wrap it in its own retry middleware (e.g. a
        // ~keep `tower::retry::Retry` layer) if it needs them.
        Box::pin(async move { do_fetch(&service, &req).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn truncated_body_reqwest_error() -> reqwest::Error {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener must bind");
        let address = listener.local_addr().expect("listener must have an address");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

            if let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0_u8; 1024];
                let _ = socket.read(&mut request).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 500000\r\n\r\ntruncated")
                    .await;
                let _ = socket.flush().await;
            }
        });

        reqwest::Client::new()
            .get(format!("http://{address}/"))
            .send()
            .await
            .expect("response headers must arrive")
            .bytes()
            .await
            .expect_err("the truncated body must fail")
    }

    #[tokio::test]
    async fn a_tower_body_error_renders_one_prefix_and_keeps_its_source() {
        let raw_error = truncated_body_reqwest_error().await;
        let raw_message = raw_error.to_string();
        let error = classify_body_read_error(raw_error);

        assert_eq!(error.to_string(), format!("data_loss: {raw_message}"));

        use std::error::Error as _;
        let source = error.source().expect("data loss must expose its source");
        let original = source.source().expect("the source wrapper must expose reqwest's error");
        assert!(original.downcast_ref::<reqwest::Error>().is_some());
    }

    #[tokio::test]
    async fn a_request_url_with_userinfo_is_refused_before_the_network() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&mock)
            .await;
        let config = CrawlConfig::builder().allow_private_networks(true).build();
        let client = crate::http::build_client(&config).expect("client must build");
        let service = HttpFetchService::new(client, config);

        let credentialed = CrawlRequest {
            url: mock.uri().replacen("http://", "http://user:TOWER-PW-8b2c@", 1) + "/in",
            headers: std::collections::HashMap::new(),
            tier: None,
        };
        let error = do_fetch(&service, &credentialed)
            .await
            .map(|_| ())
            .expect_err("a URL with userinfo must be refused");
        let text = error.to_string();
        assert!(
            !text.contains("TOWER-PW-8b2c"),
            "the error must not print the password: {text}"
        );

        do_fetch(&service, &CrawlRequest::new(format!("{}/out", mock.uri())))
            .await
            .expect("the same URL without userinfo must be fetched");
        let paths: Vec<String> = mock
            .received_requests()
            .await
            .expect("request recording is on")
            .into_iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert_eq!(paths, ["/out"], "only the URL without userinfo may reach the network");
    }

    #[test]
    fn only_3xx_is_returned_to_the_caller_as_a_redirect() {
        for status in [200u16, 204, 299, 400, 403, 404, 500] {
            assert!(
                !is_redirect_status(status),
                "{status} must not be treated as a redirect"
            );
        }
        for status in [300u16, 301, 302, 303, 307, 308, 399] {
            assert!(is_redirect_status(status), "{status} must be treated as a redirect");
        }
    }

    #[test]
    fn body_error_chains_are_recognised_by_substring() {
        for chain in [
            "connection closed before message completed: content-length mismatch",
            "body truncated",
            "incomplete message",
            "unexpected end of file",
            "body error",
            "error reading body from connection",
            "error decoding response body",
            "error decoding gzip",
        ] {
            assert!(is_body_error_chain(chain), "{chain:?} should be a body error");
        }
        for chain in ["dns error", "connection refused", "tls handshake failure"] {
            assert!(!is_body_error_chain(chain), "{chain:?} should not be a body error");
        }
    }

    fn headers_with_content_length(value: &str) -> HashMap<String, Vec<String>> {
        let mut headers = HashMap::new();
        headers.insert("content-length".to_owned(), vec![value.to_owned()]);
        headers
    }

    #[test]
    fn a_capped_read_is_never_reported_as_data_loss() {
        let headers = headers_with_content_length("100000");
        assert!(content_length_shortfall_error(&headers, 10, true).is_none());
    }

    #[test]
    fn a_shortfall_within_tolerance_is_not_data_loss() {
        let headers = headers_with_content_length("1000");
        assert!(
            content_length_shortfall_error(&headers, 1000 - CONTENT_LENGTH_SHORTFALL_TOLERANCE, false).is_none(),
            "a shortfall of exactly the tolerance is accepted"
        );
        assert!(content_length_shortfall_error(&headers, 1000, false).is_none());
        assert!(
            content_length_shortfall_error(&headers, 1200, false).is_none(),
            "a body longer than content-length is not data loss"
        );
    }

    #[test]
    fn a_shortfall_past_the_tolerance_is_data_loss() {
        let headers = headers_with_content_length("1000");
        let body_len = 1000 - CONTENT_LENGTH_SHORTFALL_TOLERANCE - 1;
        let error = content_length_shortfall_error(&headers, body_len, false).expect("data loss expected");
        assert!(
            matches!(&error, CrawlError::DataLoss { message, .. }
                if message == &format!("expected 1000 bytes, got {body_len}")),
            "got {error:?}"
        );
        assert_eq!(
            error.to_string(),
            format!("data_loss: expected 1000 bytes, got {body_len}")
        );

        use std::error::Error as _;
        assert!(error.source().is_none(), "a measured shortfall has no underlying error");
    }

    #[test]
    fn a_missing_or_unparseable_content_length_is_not_data_loss() {
        assert!(content_length_shortfall_error(&HashMap::new(), 0, false).is_none());
        let headers = headers_with_content_length("not-a-number");
        assert!(content_length_shortfall_error(&headers, 0, false).is_none());
    }
}
