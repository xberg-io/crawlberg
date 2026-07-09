//! Base HTTP fetch service (innermost in the Tower stack).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

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

/// Check whether a `CrawlError` is retryable (server errors, rate limits, bad gateways).
fn is_retryable(e: &CrawlError) -> bool {
    matches!(
        e,
        CrawlError::ServerError(_) | CrawlError::RateLimited(_) | CrawlError::BadGateway(_)
    )
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

    if let Some(ref auth) = config.auth {
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
    }

    for (k, v) in &config.custom_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    for (k, v) in &crawl_req.headers {
        req = req.header(k.as_str(), v.as_str());
    }

    req
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
    let url = url::Url::parse(&req.url).map_err(|e| CrawlError::SsrfPolicyViolation {
        url: req.url.clone(),
        reason: format!("invalid URL: {e}"),
    })?;

    validate_url(&url, &config.ssrf)
        .await
        .map_err(|e| CrawlError::SsrfPolicyViolation {
            url: req.url.clone(),
            reason: e.to_string(),
        })?;

    let http_req = apply_headers(client.get(url.to_string()), config, req);

    // ~keep reqwest uses Policy::none(); redirect following is explicit and policy-checked by callers.
    let resp = http_req.send().await.map_err(|e| classify_reqwest_error(&e))?;

    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get_all(reqwest::header::CONTENT_TYPE)
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    let mut headers: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in resp.headers().iter() {
        if let Ok(v) = value.to_str() {
            headers
                .entry(name.as_str().to_lowercase())
                .or_default()
                .push(v.to_string());
        }
    }

    // ~keep Return 3xx responses as-is so redirect handling stays caller-owned.
    if (300..400).contains(&status) {
        let body_bytes = resp.bytes().await.unwrap_or_default().to_vec();
        let body = String::from_utf8_lossy(&body_bytes).into_owned();
        return Ok(CrawlResponse {
            status,
            content_type,
            body,
            body_bytes,
            headers,
        });
    }

    match status {
        401 => return Err(CrawlError::Unauthorized("unauthorized".into())),
        403 => {
            let server = headers
                .get("server")
                .and_then(|v| v.first())
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            let body = resp.text().await.unwrap_or_default();
            if crate::http::is_waf_blocked(&server, &body, &headers) {
                let vendor = crate::http::detect_waf_vendor(&server, &body.to_lowercase());
                return Err(CrawlError::WafBlocked {
                    message: format!("waf/blocked detected: {vendor}"),
                    vendor,
                });
            }
            return Err(CrawlError::Forbidden("forbidden".into()));
        }
        404 => return Err(CrawlError::NotFound(format!("not_found: {}", req.url))),
        408 => return Err(CrawlError::Timeout("timeout".into())),
        410 => return Err(CrawlError::Gone("gone".into())),
        429 => return Err(CrawlError::RateLimited("rate_limited".into())),
        500 => return Err(CrawlError::ServerError("server_error".into())),
        502 => return Err(CrawlError::BadGateway("bad_gateway".into())),
        503 => {
            return Err(CrawlError::ServerError("service unavailable".into()));
        }
        _ => {}
    }

    let body_bytes = resp.bytes().await.map_err(|e| {
        let chain = crate::error::error_chain_string(&e);
        let is_body_error = chain.contains("content-length")
            || chain.contains("truncate")
            || chain.contains("incomplete")
            || chain.contains("end of file")
            || chain.contains("body error")
            || chain.contains("body from connection")
            || chain.contains("decoding response body")
            || chain.contains("error decoding");
        #[cfg(not(target_arch = "wasm32"))]
        let is_body_error = is_body_error || e.is_body();
        if is_body_error {
            CrawlError::DataLoss(format!("data_loss: {e}"))
        } else {
            classify_reqwest_error(&e)
        }
    })?;

    let body_vec = body_bytes.to_vec();

    if let Some(expected) = headers
        .get("content-length")
        .and_then(|v| v.first())
        .and_then(|s| s.parse::<usize>().ok())
        && body_vec.len() < expected
        && expected - body_vec.len() > 100
    {
        return Err(CrawlError::DataLoss(format!(
            "data_loss: expected {} bytes, got {}",
            expected,
            body_vec.len()
        )));
    }

    let body = String::from_utf8_lossy(&body_vec).into_owned();

    // ~keep Some WAFs return 200 challenge pages, so short 2xx bodies still need WAF classification.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let server = headers
            .get("server")
            .and_then(|v| v.first())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        if status == 200 && body.len() < 5000 && crate::http::is_waf_blocked(&server, &body, &headers) {
            let vendor = crate::http::detect_waf_vendor(&server, &body.to_lowercase());
            return Err(CrawlError::WafBlocked {
                message: format!("waf/blocked detected on 2xx (body): {vendor}"),
                vendor,
            });
        }
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

        Box::pin(async move {
            let retry_count = config.retry_count;
            let retry_codes = &config.retry_codes;

            for attempt in 0..=retry_count {
                match do_fetch(&client, &config, &req).await {
                    Ok(resp) => {
                        if retry_codes.contains(&resp.status) && attempt < retry_count {
                            tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                            continue;
                        }
                        return Ok(resp);
                    }
                    Err(e) if is_retryable(&e) && attempt < retry_count => {
                        tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            Err(CrawlError::Other("retry exhausted".into()))
        })
    }
}
