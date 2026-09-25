//! Base HTTP fetch service (innermost in the Tower stack).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tower::Service;

use super::types::{CrawlRequest, CrawlResponse};
use crate::error::{CrawlError, classify_reqwest_error};
use crate::net::ssrf::validate_url;
use crate::types::CrawlConfig;

/// Innermost Tower service that performs the actual HTTP fetch.
#[derive(Clone)]
pub struct HttpFetchService {
    client: reqwest::Client,
    config: Arc<CrawlConfig>,
}

impl HttpFetchService {
    pub fn new(client: reqwest::Client, config: CrawlConfig) -> Self {
        Self {
            client,
            config: Arc::new(config),
        }
    }
}

/// Helper to apply auth and custom headers to a request builder.
fn apply_headers(
    mut req: reqwest::RequestBuilder,
    config: &CrawlConfig,
    crawl_req: &CrawlRequest,
) -> reqwest::RequestBuilder {
    if !crawl_req.headers.contains_key("user-agent") {
        if let Some(ref ua) = config.user_agent {
            req = req.header(reqwest::header::USER_AGENT, ua.as_str());
        } else {
            req = req.header(
                reqwest::header::USER_AGENT,
                concat!("crawlberg/", env!("CARGO_PKG_VERSION")),
            );
        }
    }

    // ~keep Withhold configured credentials once a redirect chain has left its origin host;
    // ~keep reqwest's own cross-host stripping never runs because we follow redirects manually.
    if let Some(ref auth) = config.auth {
        if crawl_req.is_on_origin_host() {
            match auth {
                crate::types::AuthConfig::Basic { username, password } => {
                    req = req.basic_auth(username, Some(password));
                }
                crate::types::AuthConfig::Bearer { token } => {
                    req = req.bearer_auth(token);
                }
                crate::types::AuthConfig::Header { name, value } => {
                    req = req.header(name.as_str(), value.as_str());
                }
            }
        } else {
            tracing::debug!(
                origin = crawl_req.origin_host.as_deref().unwrap_or(""),
                target = crawl_req.domain().unwrap_or_default(),
                "withholding configured credentials from a cross-host redirect hop"
            );
        }
    }

    for (k, v) in &config.custom_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    for (k, v) in &crawl_req.headers {
        req = req.header(k.as_str(), v.as_str());
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

/// Largest 2xx body still treated as a possible WAF challenge page rather than real content.
const WAF_CHALLENGE_MAX_BODY_LEN: usize = 5000;

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

/// Collect response headers into a lowercase-keyed multi-map, dropping non-UTF-8 values.
fn collect_headers(resp: &reqwest::Response) -> HashMap<String, Vec<String>> {
    let mut headers: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in resp.headers().iter() {
        if let Ok(v) = value.to_str() {
            headers
                .entry(name.as_str().to_lowercase())
                .or_default()
                .push(v.to_string());
        }
    }
    headers
}

/// Read the lowercase `server` header, or an empty string when absent.
fn server_header(headers: &HashMap<String, Vec<String>>) -> String {
    headers
        .get("server")
        .and_then(|v| v.first())
        .map(|s| s.to_lowercase())
        .unwrap_or_default()
}

/// Build the `CrawlResponse` for a 3xx without classifying it; a failed body read yields an
/// empty body rather than an error, because the caller only needs the status and headers.
async fn read_redirect_response(
    resp: reqwest::Response,
    config: &CrawlConfig,
    status: u16,
    content_type: String,
    headers: HashMap<String, Vec<String>>,
) -> CrawlResponse {
    let (body_bytes, _) = crate::http::read_body_bounded(resp, crate::http::effective_max_body_size(config))
        .await
        .unwrap_or_default();
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    CrawlResponse {
        status,
        content_type,
        body,
        body_bytes,
        headers,
    }
}

/// Classify a 403 as a WAF block when the body or headers carry a vendor fingerprint.
async fn forbidden_error(
    resp: reqwest::Response,
    headers: &HashMap<String, Vec<String>>,
    config: &CrawlConfig,
) -> CrawlError {
    let server = server_header(headers);
    let body = crate::http::read_text_bounded(resp, crate::http::effective_max_body_size(config)).await;
    if crate::http::is_waf_blocked(&server, &body, headers) {
        let vendor = crate::http::detect_waf_vendor(&server, &body.to_lowercase());
        return CrawlError::WafBlocked {
            message: format!("waf/blocked detected: {vendor}"),
            vendor,
        };
    }
    CrawlError::forbidden("forbidden")
}

/// Map a status code that needs no body inspection onto its error, if it is an error at all.
fn status_error(status: u16, url: &str) -> Option<CrawlError> {
    match status {
        401 => Some(CrawlError::unauthorized("unauthorized")),
        404 => Some(CrawlError::not_found(format!("not_found: {url}"))),
        408 => Some(CrawlError::timeout("timeout")),
        410 => Some(CrawlError::gone("gone")),
        429 => Some(CrawlError::rate_limited("rate_limited")),
        500 => Some(CrawlError::server_error("server_error")),
        502 => Some(CrawlError::bad_gateway("bad_gateway")),
        503 => Some(CrawlError::server_error("service unavailable")),
        _ => None,
    }
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
    let chain = crate::error::error_chain_string(&e);
    let is_body_error = is_body_error_chain(&chain);
    #[cfg(not(target_arch = "wasm32"))]
    let is_body_error = is_body_error || e.is_body();
    if is_body_error {
        let message = format!("data_loss: {e}");
        CrawlError::data_loss_with_source(message, e)
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
            "data_loss: expected {expected} bytes, got {body_len}"
        )));
    }
    None
}

/// Classify a short 2xx body as a WAF challenge page when it carries a vendor fingerprint.
///
/// ~keep Some WAFs return 200 challenge pages, so short 2xx bodies still need WAF classification.
#[cfg(not(target_arch = "wasm32"))]
fn waf_error_for_success(status: u16, body: &str, headers: &HashMap<String, Vec<String>>) -> Option<CrawlError> {
    if status != 200 || body.len() >= WAF_CHALLENGE_MAX_BODY_LEN {
        return None;
    }
    let server = server_header(headers);
    if !crate::http::is_waf_blocked(&server, body, headers) {
        return None;
    }
    let vendor = crate::http::detect_waf_vendor(&server, &body.to_lowercase());
    Some(CrawlError::WafBlocked {
        message: format!("waf/blocked detected on 2xx (body): {vendor}"),
        vendor,
    })
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
async fn do_fetch(
    client: &reqwest::Client,
    config: &CrawlConfig,
    req: &CrawlRequest,
) -> Result<CrawlResponse, CrawlError> {
    let url =
        url::Url::parse(&req.url).map_err(|e| CrawlError::ssrf_violation(&req.url, format!("invalid URL: {e}")))?;

    validate_url(&url, &config.ssrf)
        .await
        .map_err(|e| CrawlError::ssrf_violation(req.url.clone(), e.to_string()))?;

    let http_req = apply_headers(client.get(url.to_string()), config, req);

    // ~keep reqwest uses Policy::none(); redirect following is explicit and policy-checked by callers.
    let resp = http_req.send().await.map_err(classify_reqwest_error)?;

    let status = resp.status().as_u16();
    let content_type = content_type_of(&resp);
    let headers = collect_headers(&resp);

    // ~keep Return 3xx responses as-is so redirect handling stays caller-owned.
    if is_redirect_status(status) {
        return Ok(read_redirect_response(resp, config, status, content_type, headers).await);
    }

    if status == 403 {
        return Err(forbidden_error(resp, &headers, config).await);
    }
    if let Some(error) = status_error(status, &req.url) {
        return Err(error);
    }

    let (body_vec, hit_cap) = crate::http::read_body_bounded(resp, crate::http::effective_max_body_size(config))
        .await
        .map_err(classify_body_read_error)?;

    if let Some(error) = content_length_shortfall_error(&headers, body_vec.len(), hit_cap) {
        return Err(error);
    }

    let body = String::from_utf8_lossy(&body_vec).into_owned();

    #[cfg(not(target_arch = "wasm32"))]
    if let Some(error) = waf_error_for_success(status, &body, &headers) {
        return Err(error);
    }

    Ok(CrawlResponse {
        status,
        content_type,
        body,
        body_bytes: body_vec,
        headers,
    })
}

impl Service<CrawlRequest> for HttpFetchService {
    type Response = CrawlResponse;
    type Error = CrawlError;
    type Future = Pin<Box<dyn Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: CrawlRequest) -> Self::Future {
        let client = self.client.clone();
        let config = self.config.clone();

        // ~keep A single fetch attempt only: retries are owned by the dispatch loop
        // ~keep (`engine::fetch::run_dispatch_loop`, via `SimpleRetryPolicy::from_config`), which
        // ~keep re-invokes `run_tier` — and therefore this service — once per attempt, honouring
        // ~keep `retry_count`/`retry_codes` exactly once. Previously this service ran its own
        // ~keep `0..=retry_count` loop *inside* the dispatch loop's own retry loop, multiplying
        // ~keep attempts (retry_count=4 produced 20 requests instead of 5). A direct
        // ~keep `tower::Service` consumer of `HttpFetchService` that bypasses the dispatch loop
        // ~keep now gets no retries at all; wrap it in its own retry middleware (e.g. a
        // ~keep `tower::retry::Retry` layer) if it needs them.
        Box::pin(async move { do_fetch(&client, &config, &req).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Status → error mapping captured from the pre-extraction `do_fetch` match, so the
    /// extraction into [`status_error`] can be shown to be behavior-preserving.
    fn expected_status_mapping() -> Vec<(u16, Option<&'static str>)> {
        vec![
            (200, None),
            (201, None),
            (204, None),
            (400, None),
            (401, Some("unauthorized")),
            (402, None),
            (403, None),
            (404, Some("not_found")),
            (405, None),
            (408, Some("timeout")),
            (409, None),
            (410, Some("gone")),
            (418, None),
            (429, Some("rate_limited")),
            (451, None),
            (500, Some("server_error")),
            (501, None),
            (502, Some("bad_gateway")),
            (503, Some("service_unavailable")),
            (504, None),
        ]
    }

    fn error_tag(error: &CrawlError) -> &'static str {
        match error {
            CrawlError::Unauthorized { .. } => "unauthorized",
            CrawlError::NotFound { .. } => "not_found",
            CrawlError::Timeout { .. } => "timeout",
            CrawlError::Gone { .. } => "gone",
            CrawlError::RateLimited { .. } => "rate_limited",
            CrawlError::ServerError { message, .. } if message == "service unavailable" => "service_unavailable",
            CrawlError::ServerError { .. } => "server_error",
            CrawlError::BadGateway { .. } => "bad_gateway",
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn status_error_maps_exactly_the_codes_the_fetch_path_classified() {
        for (status, expected) in expected_status_mapping() {
            let actual = status_error(status, "https://example.com/x");
            assert_eq!(
                actual.as_ref().map(error_tag),
                expected,
                "status {status} mapped to {actual:?}"
            );
        }
    }

    #[test]
    fn status_error_embeds_the_requested_url_in_the_not_found_message() {
        let error = status_error(404, "https://example.com/missing").expect("404 is an error");
        assert!(
            matches!(&error, CrawlError::NotFound { message, .. } if message == "not_found: https://example.com/missing"),
            "got {error:?}"
        );
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
                if message == &format!("data_loss: expected 1000 bytes, got {body_len}")),
            "got {error:?}"
        );
    }

    #[test]
    fn a_missing_or_unparseable_content_length_is_not_data_loss() {
        assert!(content_length_shortfall_error(&HashMap::new(), 0, false).is_none());
        let headers = headers_with_content_length("not-a-number");
        assert!(content_length_shortfall_error(&headers, 0, false).is_none());
    }
}
