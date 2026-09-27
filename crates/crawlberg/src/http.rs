//! HTTP fetching with redirect handling, retry logic, and cookie extraction.

mod body;
mod challenge;
mod client;
mod headers;
mod retry;
mod status;
mod waf;

use std::collections::HashMap;

use reqwest::header::{CONTENT_TYPE, HeaderMap, USER_AGENT};

use crate::error::{CrawlError, classify_reqwest_error, error_chain_string};
use crate::net::origin::is_authorized_host;
use crate::net::ssrf::validate_url;
use crate::types::{AuthConfig, CrawlConfig};

use headers::build_headers_map;

pub(crate) use body::{
    effective_max_body_size, read_body_bounded, read_text_bounded, redecode_with_charset,
    truncate_body_at_char_boundary,
};
pub(crate) use challenge::{challenge_status_error, is_challenge_status};
pub(crate) use client::build_client;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use headers::extract_cookies_from_hashmap;
pub(crate) use headers::extract_response_meta_from_hashmap;
pub(crate) use retry::{fetch_with_retry, should_retry_error};
pub(crate) use status::status_error;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use waf::{detect_waf_vendor, is_waf_blocked};

/// Browser-specific extras attached to an `HttpResponse` produced by the native
/// browser backend. Populated when `browser_used` is true.
///
/// Exposed as `pub` because it is a field of the public [`HttpResponse`] struct.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct BrowserExtras {
    /// Result of an in-page JavaScript evaluation, if any.
    pub eval_result: Option<serde_json::Value>,
    /// Network-level metadata for sub-resource requests recorded by the browser.
    pub network_events: Vec<crate::types::ResponseMeta>,
    /// Cookies present in the browser session after page load.
    pub cookies: Vec<crate::types::CookieInfo>,
}

/// An HTTP response with status, headers, and body content.
///
/// Exposed as `pub` so that [`crate::types::WafClassifier`] implementations —
/// which are defined outside `crate::http` — can inspect responses. All
/// fields that are only used by internal paths carry `#[allow(dead_code)]`.
pub struct HttpResponse {
    /// HTTP status code (e.g. 200, 403).
    pub status: u16,
    /// Value of the `Content-Type` response header, or empty string if absent.
    pub content_type: String,
    /// Decoded response body as UTF-8 text.
    pub body: String,
    /// Raw response body bytes (before UTF-8 decoding).
    pub body_bytes: Vec<u8>,
    /// All response headers, keyed by lowercase header name.
    #[allow(dead_code)]
    pub headers: std::collections::HashMap<String, Vec<String>>,
    /// Optional browser-specific extras (eval result, network events, cookies).
    #[allow(dead_code)]
    pub browser_extras: Option<BrowserExtras>,
    /// The URL of the final response after any transparent redirect following.
    ///
    /// On native targets reqwest uses `Policy::none()` so this always equals
    /// the request URL (redirects are handled manually by `follow_redirects`).
    /// On wasm targets the browser's `fetch` follows redirects transparently
    /// and `reqwest::Response::url()` returns the post-redirect URL — which is
    /// what the wasm scrape path needs to populate `ScrapeResult::final_url`.
    #[allow(dead_code)]
    pub final_url: String,
    /// PNG screenshot bytes captured for this fetch, when the caller requested one
    /// (`CrawlConfig.capture_screenshot`) and the fetch went through a browser backend
    /// that supports capturing it. `None` for every plain-HTTP fetch and for browser
    /// fetches that did not request a screenshot.
    #[allow(dead_code)]
    pub screenshot: Option<Vec<u8>>,
}

/// Everything a fetch needs that does not change from one redirect hop to the next.
struct FetchContext<'a> {
    url: &'a str,
    config: &'a CrawlConfig,
    extra_headers: &'a HashMap<String, String>,
    client: &'a reqwest::Client,
    initial_url: &'a url::Url,
    /// The host configured credentials are authorized for. `None` falls back to
    /// `initial_url`'s own host -- see [`apply_auth`].
    origin_host: Option<&'a str>,
}

/// What one hop produced: a redirect target still to follow, or a finished response.
///
/// ~keep Not boxed despite the variant size gap: `Complete` carries the value `http_fetch`
/// ~keep returns, which was already moved by value out of this code before the hop loop was
/// ~keep split out. Boxing it to satisfy the size-ratio lint would add a heap allocation to
/// ~keep every successful fetch and buy nothing back.
#[allow(clippy::large_enum_variant)]
enum HopOutcome {
    Redirect(url::Url),
    Complete(HttpResponse),
}

/// Where a 3xx response points.
enum RedirectTarget {
    /// The `Location` header, resolved against the URL that served the redirect.
    Follow(url::Url),
    /// A `Location` that does not resolve to a URL; the 3xx is returned as the response.
    Unresolvable,
}

/// Response metadata captured before the body is consumed.
struct ResponseHead {
    status: u16,
    content_type: String,
    final_url: String,
    headers: HeaderMap,
}

impl ResponseHead {
    fn from_response(resp: &reqwest::Response) -> Self {
        Self {
            status: resp.status().as_u16(),
            content_type: resp
                .headers()
                .get_all(CONTENT_TYPE)
                .iter()
                .next_back()
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned(),
            final_url: resp.url().to_string(),
            headers: resp.headers().clone(),
        }
    }

    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn content_length(&self) -> Option<usize> {
        self.headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
    }

    fn into_response(
        self,
        body: String,
        body_bytes: Vec<u8>,
        headers_map: HashMap<String, Vec<String>>,
    ) -> HttpResponse {
        HttpResponse {
            status: self.status,
            content_type: self.content_type,
            body,
            body_bytes,
            headers: headers_map,
            browser_extras: None,
            final_url: self.final_url,
            screenshot: None,
        }
    }
}

/// Perform a single HTTP GET request with the given configuration.
///
/// Handles user-agent, authentication, custom headers, error status codes,
/// content-length validation, and SSRF policy enforcement.
///
/// SSRF validation is applied to the initial URL and to every redirect target
/// (manual redirect loop to ensure policy applies to all hops).
pub(crate) async fn http_fetch(
    url: &str,
    config: &CrawlConfig,
    extra_headers: &std::collections::HashMap<String, String>,
    client: &reqwest::Client,
    origin_host: Option<&str>,
) -> Result<HttpResponse, CrawlError> {
    let initial_url = url::Url::parse(url).map_err(|e| CrawlError::ssrf_violation(url, format!("invalid URL: {e}")))?;

    validate_url(&initial_url, &config.ssrf)
        .await
        .map_err(|e| CrawlError::ssrf_violation(url, e.to_string()))?;

    let context = FetchContext {
        url,
        config,
        extra_headers,
        client,
        initial_url: &initial_url,
        origin_host,
    };
    let mut current_url = initial_url.clone();
    let mut redirects_followed: usize = 0;

    loop {
        let next_url = match fetch_one_hop(&context, &current_url).await? {
            HopOutcome::Complete(response) => return Ok(response),
            HopOutcome::Redirect(next_url) => next_url,
        };

        if let Err(e) = validate_url(&next_url, &config.ssrf).await {
            return Err(CrawlError::ssrf_violation(&next_url, e.to_string()));
        }

        redirects_followed += 1;
        // ~keep `CrawlConfig.max_redirects` is the single effective redirect-hop bound for
        // every GET, matching `follow_redirects` in `engine/crawl_loop.rs`. It is the only
        // one of the two redirect-count fields exposed by the builder (see
        // `types/builder.rs::max_redirects`); `SsrfPolicy.max_redirects` is kept only for
        // backward-compatible (de)serialization of the SSRF policy shape (it is part of the
        // fixtures/schema.json contract) and is not read at runtime -- SSRF safety itself is
        // unaffected because every redirect target is still validated by `validate_url` above.
        if redirects_followed > config.max_redirects {
            return Err(CrawlError::ssrf_violation(&next_url, "too many redirects"));
        }

        current_url = next_url;
    }
}

/// Fetch `current_url` once, without following any redirect it returns.
async fn fetch_one_hop(context: &FetchContext<'_>, current_url: &url::Url) -> Result<HopOutcome, CrawlError> {
    let resp = send_hop_request(context, current_url).await?;
    let head = ResponseHead::from_response(&resp);

    if (300..400).contains(&head.status) {
        match redirect_target(current_url, &head.headers) {
            Some(RedirectTarget::Follow(next_url)) => return Ok(HopOutcome::Redirect(next_url)),
            Some(RedirectTarget::Unresolvable) => {
                return Ok(HopOutcome::Complete(
                    unresolvable_redirect_response(context.config, resp, head).await,
                ));
            }
            None => {}
        }
    }

    // ~keep Computed lazily and cached below rather than unconditionally up front: most
    // non-2xx statuses (404, 500, 502, ...) return before ever needing a header map, so
    // building one here would add an allocation to paths that previously had none. The
    // challenge statuses below are the exception and always need it.
    let mut headers_map_cache: Option<HashMap<String, Vec<String>>> = None;

    // ~keep Fingerprinting has to precede `status_error`: once a 429/503 has become a
    // `RateLimited`/`ServerError` the retry policy answers it with `Retry`, and a challenge
    // is then re-requested by the client that provoked it instead of escalating to the
    // browser tier (crawlberg#169).
    if is_challenge_status(head.status) {
        let headers_map = headers_map_cache.get_or_insert_with(|| build_headers_map(&head.headers));
        return Err(challenge_status_error(
            head.status,
            context.url,
            headers_map,
            resp,
            effective_max_body_size(context.config),
        )
        .await);
    }
    if let Some(error) = status_error(head.status, context.url) {
        return Err(error);
    }

    // ~keep Header-only WAF fingerprints must fire before reading a 2xx body as real content.
    // ~keep The TOML corpus is the single WAF source of truth; do not hardcode header lists here.
    if let Some(header_vendor) = header_only_waf_vendor(&head, &mut headers_map_cache) {
        let config = context.config;
        return Err(body_confirmed_waf_error(config, resp, &head, header_vendor, &mut headers_map_cache).await);
    }

    let expected_len = head.content_length();
    let body_bytes = read_validated_body(context.config, resp, expected_len).await?;
    let body = String::from_utf8_lossy(&body_bytes).into_owned();

    // ~keep Small 2xx bodies with high-confidence vendor JS fingerprints are treated as WAF interstitials.
    if let Some(vendor) = body_waf_vendor(&head, &body, &body_bytes, &mut headers_map_cache) {
        return Err(CrawlError::WafBlocked {
            message: format!("waf/blocked detected on 2xx (body): {vendor}"),
            vendor,
        });
    }

    // ~keep Reuses the cached header map (built at most once above) instead of walking
    // `headers` a third time; falls back to a fresh build only for the statuses that
    // never populated the cache (anything outside 200..300 and not explicitly matched
    // above, e.g. 206 or an unlisted 4xx/5xx that falls through to no terminal error).
    let headers_map = headers_map_cache.unwrap_or_else(|| build_headers_map(&head.headers));
    Ok(HopOutcome::Complete(head.into_response(body, body_bytes, headers_map)))
}

/// Build and send the GET for one hop.
async fn send_hop_request(context: &FetchContext<'_>, current_url: &url::Url) -> Result<reqwest::Response, CrawlError> {
    // ~keep WASM has no client-level timeout; apply the budget to every redirect hop.
    let mut req = context
        .client
        .get(current_url.to_string())
        .timeout(context.config.request_timeout);

    if let Some(ref ua) = context.config.user_agent {
        req = req.header(USER_AGENT, ua.as_str());
    } else {
        req = req.header(USER_AGENT, concat!("crawlberg/", env!("CARGO_PKG_VERSION")));
    }

    req = apply_auth(req, context, current_url);

    for (k, v) in &context.config.custom_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    for (k, v) in context.extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    req.send().await.map_err(classify_reqwest_error)
}

/// Attach the configured credentials, but only while the hop is still on the host they
/// were configured for: `context.origin_host` when the caller named one (this fetch is
/// part of a wider crawl -- e.g. a robots.txt request, whose own URL is not the crawl's
/// seed -- see crawlberg#387), else `context.initial_url`'s own host.
///
/// ~keep Redirects are followed manually under `Policy::none()`, so reqwest's own
/// ~keep strip-credentials-on-cross-host behaviour never runs and we must do it here:
/// ~keep an open redirect off an authenticated origin would otherwise hand the
/// ~keep configured Authorization header straight to the redirect target.
fn apply_auth(
    req: reqwest::RequestBuilder,
    context: &FetchContext<'_>,
    current_url: &url::Url,
) -> reqwest::RequestBuilder {
    let authorized = match context.origin_host {
        Some(origin_host) => is_authorized_host(origin_host, current_url),
        None => is_authorized_host(context.initial_url.host_str().unwrap_or(""), current_url),
    };
    if !authorized {
        if context.config.auth.is_some() {
            tracing::debug!(
                origin = context
                    .origin_host
                    .unwrap_or_else(|| context.initial_url.host_str().unwrap_or("")),
                target = current_url.host_str().unwrap_or(""),
                "withholding configured credentials from a cross-host request"
            );
        }
        return req;
    }

    match context.config.auth {
        Some(AuthConfig::Basic {
            ref username,
            ref password,
        }) => req.basic_auth(username, Some(password)),
        Some(AuthConfig::Bearer { ref token }) => req.bearer_auth(token),
        Some(AuthConfig::Header { ref name, ref value }) => req.header(name.as_str(), value.as_str()),
        None => req,
    }
}

/// Resolve a 3xx response's `Location` header against the URL that served it.
fn redirect_target(current_url: &url::Url, headers: &HeaderMap) -> Option<RedirectTarget> {
    let location = headers
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)?;

    Some(match current_url.join(&location) {
        Ok(next_url) => RedirectTarget::Follow(next_url),
        Err(_) => RedirectTarget::Unresolvable,
    })
}

/// Return a 3xx whose `Location` could not be resolved as the response itself.
async fn unresolvable_redirect_response(
    config: &CrawlConfig,
    resp: reqwest::Response,
    head: ResponseHead,
) -> HttpResponse {
    let (body_bytes, _) = read_body_bounded(resp, effective_max_body_size(config))
        .await
        .unwrap_or_default();
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    let headers_map = build_headers_map(&head.headers);
    head.into_response(body, body_bytes, headers_map)
}

/// The WAF vendor a 2xx's headers alone fingerprint, before its body is read.
fn header_only_waf_vendor(
    head: &ResponseHead,
    headers_map_cache: &mut Option<HashMap<String, Vec<String>>>,
) -> Option<String> {
    if !head.is_success() {
        return None;
    }
    let headers_map = headers_map_cache.get_or_insert_with(|| build_headers_map(&head.headers));
    challenge::header_waf_vendor(head.status, headers_map)
}

/// Re-run classification over the body of a 2xx its headers already flagged, preferring
/// the vendor the body names and falling back to the header-derived one.
async fn body_confirmed_waf_error(
    config: &CrawlConfig,
    resp: reqwest::Response,
    head: &ResponseHead,
    header_vendor: String,
    headers_map_cache: &mut Option<HashMap<String, Vec<String>>>,
) -> CrawlError {
    let body = read_text_bounded(resp, effective_max_body_size(config)).await;
    let headers_map = headers_map_cache.get_or_insert_with(|| build_headers_map(&head.headers));
    let vendor = waf::waf_vendor_from_body(head.status, &body, headers_map).unwrap_or(header_vendor);
    CrawlError::WafBlocked {
        message: format!("waf/blocked detected on 2xx (header): {vendor}"),
        vendor,
    }
}

/// The WAF vendor a 2xx's already-read body fingerprints.
fn body_waf_vendor(
    head: &ResponseHead,
    body: &str,
    body_bytes: &[u8],
    headers_map_cache: &mut Option<HashMap<String, Vec<String>>>,
) -> Option<String> {
    if !head.is_success() {
        return None;
    }
    let headers_map = headers_map_cache.get_or_insert_with(|| build_headers_map(&head.headers));
    waf::waf_vendor_from_bytes(head.status, body_bytes, body, headers_map)
}

/// Shortfall below the declared `content-length` that is read as a truncated transfer
/// rather than as ordinary framing slack.
const BODY_SHORTFALL_TOLERANCE_BYTES: usize = 100;

/// Read the response body under the configured cap and reject a short transfer.
async fn read_validated_body(
    config: &CrawlConfig,
    resp: reqwest::Response,
    expected_len: Option<usize>,
) -> Result<Vec<u8>, CrawlError> {
    let (body_bytes, hit_cap) = read_body_bounded(resp, effective_max_body_size(config))
        .await
        .map_err(classify_body_read_error)?;

    // ~keep A capped read stopping short of `content-length` is expected (that is the
    // point of `max_body_size`), not evidence of a truncated/failed transfer.
    if !hit_cap
        && let Some(expected) = expected_len
        && body_bytes.len() < expected
        && expected - body_bytes.len() > BODY_SHORTFALL_TOLERANCE_BYTES
    {
        return Err(CrawlError::data_loss(format!(
            "data_loss: expected {expected} bytes, got {}",
            body_bytes.len()
        )));
    }

    Ok(body_bytes)
}

/// Tell a truncated or failed body transfer apart from any other reqwest failure.
fn classify_body_read_error(e: reqwest::Error) -> CrawlError {
    let chain = error_chain_string(&e);
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
        let message = format!("data_loss: {e}");
        CrawlError::data_loss_with_source(message, e)
    } else {
        classify_reqwest_error(e)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "http_tests.rs"]
mod tests;
