//! HTTP fetching with redirect handling, retry logic, and cookie extraction.

mod body;
mod client;
mod headers;
mod retry;
mod waf;

use std::collections::HashMap;

use reqwest::header::{CONTENT_TYPE, HeaderMap, USER_AGENT};

use crate::error::{CrawlError, classify_reqwest_error, error_chain_string};
use crate::net::origin::same_host;
use crate::net::ssrf::validate_url;
use crate::types::{AuthConfig, CrawlConfig};

use headers::build_headers_map;
use retry::{GATEWAY_TIMEOUT_SUFFIX, SERVICE_UNAVAILABLE_SUFFIX};

pub(crate) use body::{
    effective_max_body_size, read_body_bounded, read_text_bounded, redecode_with_charset,
    truncate_body_at_char_boundary,
};
pub(crate) use client::build_client;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use headers::extract_cookies_from_hashmap;
pub(crate) use headers::extract_response_meta_from_hashmap;
pub(crate) use retry::fetch_with_retry;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use waf::{detect_waf_vendor, is_waf_blocked};

/// Statuses whose `Location` header this crawl follows.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

/// Statuses that carry no document. A browser commits nothing for them, so a browser fetch
/// reports them with an empty body, as the HTTP fetch does.
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
pub(crate) const NO_DOCUMENT_STATUSES: [u16; 3] = [204, 205, 304];

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
    // non-2xx statuses (404, 429, 500, ...) return before ever needing a header map, so
    // building one here would add an allocation to paths that previously had none.
    let mut headers_map_cache: Option<HashMap<String, Vec<String>>> = None;

    if head.status == 403 {
        return Err(forbidden_error(context.config, resp, &head, &mut headers_map_cache).await);
    }
    if let Some(error) = terminal_status_error(head.status, context.url) {
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

/// Attach the configured credentials, but only while the hop is still on the origin they
/// were configured for.
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
    if !same_host(context.initial_url, current_url) {
        if context.config.auth.is_some() {
            tracing::debug!(
                origin = context.initial_url.host_str().unwrap_or(""),
                target = current_url.host_str().unwrap_or(""),
                "withholding configured credentials from a cross-host redirect hop"
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

/// Classify a 403: a WAF block when the body fingerprints, a plain forbidden otherwise.
async fn forbidden_error(
    config: &CrawlConfig,
    resp: reqwest::Response,
    head: &ResponseHead,
    headers_map_cache: &mut Option<HashMap<String, Vec<String>>>,
) -> CrawlError {
    let body = read_text_bounded(resp, effective_max_body_size(config)).await;
    let headers_map = headers_map_cache.get_or_insert_with(|| build_headers_map(&head.headers));
    forbidden_body_error(head.status, &body, headers_map)
}

/// The error for a 403 body: a WAF block when the body fingerprints, a plain forbidden otherwise.
fn forbidden_body_error(status: u16, body: &str, headers_map: &HashMap<String, Vec<String>>) -> CrawlError {
    match waf::waf_vendor_from_body(status, body, headers_map) {
        Some(vendor) => CrawlError::WafBlocked {
            message: format!("waf/blocked detected: {vendor}"),
            vendor,
        },
        None => CrawlError::forbidden("forbidden"),
    }
}

/// The error a status ends the fetch with, for every status that ends it without
/// needing the response body. 403 is handled separately because it reads the body.
fn terminal_status_error(status: u16, url: &str) -> Option<CrawlError> {
    Some(match status {
        401 => CrawlError::unauthorized("unauthorized"),
        404 => CrawlError::not_found(format!("not_found: {url}")),
        408 => CrawlError::timeout("timeout: request timed out"),
        410 => CrawlError::gone("gone"),
        429 => CrawlError::rate_limited("rate_limited"),
        500 => CrawlError::server_error("server_error"),
        502 => CrawlError::bad_gateway("bad_gateway"),
        503 => CrawlError::server_error(format!("server_error: {SERVICE_UNAVAILABLE_SUFFIX}")),
        504 => CrawlError::server_error(format!("server_error: {GATEWAY_TIMEOUT_SUFFIX}")),
        _ => return None,
    })
}

/// Apply HTTP mode's status handling to a page a browser rendered. A status the HTTP fetch
/// raises as an error raises the same error here. Where HTTP mode reports the status as a
/// page instead (a 404 or 403 under `soft_http_errors`, or a 404 at the end of a redirect),
/// the page keeps its status and loses its body, as the HTTP fetch reports it.
#[cfg(any(feature = "browser", feature = "browser-native"))]
pub(crate) fn rendered_status_outcome(
    mut response: HttpResponse,
    redirected: bool,
    config: &CrawlConfig,
) -> Result<HttpResponse, CrawlError> {
    let status = response.status;
    let error = if status == 403 {
        Some(forbidden_body_error(status, &response.body, &response.headers))
    } else {
        terminal_status_error(status, &response.final_url)
    };
    let Some(error) = error else {
        return Ok(response);
    };
    let reported_as_page = match status {
        404 => config.soft_http_errors || redirected,
        403 => config.soft_http_errors,
        _ => false,
    };
    if !reported_as_page {
        return Err(error);
    }
    response.content_type.clear();
    response.body.clear();
    response.body_bytes.clear();
    Ok(response)
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
    waf::waf_vendor_from_body(head.status, "", headers_map)
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
mod tests {
    use super::*;
    use crate::net::ssrf::SsrfPolicy;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    #[tokio::test]
    async fn http_fetch_enforces_config_timeout_without_client_default() {
        const REQUEST_TIMEOUT: Duration = Duration::from_millis(50);
        const RESPONSE_DELAY: Duration = Duration::from_millis(500);
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/slow"))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(RESPONSE_DELAY)
                    .set_body_string("late"),
            )
            .mount(&mock)
            .await;
        let config = CrawlConfig {
            request_timeout: REQUEST_TIMEOUT,
            ssrf: SsrfPolicy {
                deny_private: false,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };
        // ~keep WASM has no client-level timeout; a bare native client reproduces that condition.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client must build");
        for endpoint in ["/slow", "/start"] {
            let result = http_fetch(&format!("{}{endpoint}", mock.uri()), &config, &HashMap::new(), &client).await;
            assert!(
                matches!(result, Err(CrawlError::Timeout { .. })),
                "expected timeout for {endpoint}"
            );
        }
    }

    /// `http_fetch` must populate `final_url` from the reqwest response URL.
    ///
    /// On native targets (Policy::none) this equals the request URL because
    /// redirects are not followed transparently.  The test verifies the field
    /// is set to a non-empty value matching the requested URL — confirming
    /// the plumbing that the wasm path relies on to capture the post-redirect
    /// URL is in place.
    ///
    /// Note: the wasm-specific transparent-redirect behaviour (browser `fetch`
    /// following 3xx and returning the final URL via `response.url()`) cannot
    /// be exercised under `cargo test` because it requires a wasm32 target and
    /// a real browser runtime.  The build-time check (`cargo build --target
    /// wasm32-unknown-unknown`) verifies the changed code path compiles
    /// correctly for wasm.
    #[tokio::test]
    async fn http_fetch_populates_final_url() {
        let mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>Hello</body></html>")
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;

        let url = format!("{}/page", mock.uri());
        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");
        let resp = http_fetch(&url, &config, &std::collections::HashMap::new(), &client)
            .await
            .expect("http_fetch must succeed");

        assert!(
            !resp.final_url.is_empty(),
            "final_url must not be empty after a successful fetch"
        );
        assert!(
            resp.final_url.contains("/page"),
            "final_url must contain the requested path, got: {}",
            resp.final_url
        );
    }

    /// Regression test: `http_fetch`'s internal redirect loop used to enforce
    /// `config.ssrf.max_redirects` (a `u8` with no public builder setter, default 5)
    /// instead of the builder-settable `config.max_redirects`, so `.max_redirects(N)`
    /// had no effect on this loop. This proves the builder value now bounds it.
    #[tokio::test]
    async fn http_fetch_stops_once_builder_max_redirects_is_exceeded() {
        let mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/hop0"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/hop1"))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/hop1"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/hop2"))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/hop2"))
            .respond_with(ResponseTemplate::new(200).set_body_string("done"))
            .mount(&mock)
            .await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        config.max_redirects = 1;
        let client = build_client(&config).expect("client must build");
        let url = format!("{}/hop0", mock.uri());
        let result = http_fetch(&url, &config, &std::collections::HashMap::new(), &client).await;

        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("must stop once the second hop exceeds max_redirects(1), got Ok"),
        };
        assert!(
            matches!(err, CrawlError::SsrfPolicyViolation { ref reason, .. } if reason == "too many redirects"),
            "expected a too-many-redirects SsrfPolicyViolation, got {err:?}"
        );
    }

    /// ~keep Regression: `redact_url_credentials` existed and was unit-tested, but every
    /// real `SsrfPolicyViolation` site built the variant with a struct literal carrying
    /// the raw URL — so a refused `http://user:pass@host/` leaked the credential into API
    /// error bodies, MCP payloads and tracing fields. Testing the helper in isolation is
    /// exactly what hid that, so this drives a real `http_fetch` rejection instead.
    #[tokio::test]
    async fn http_fetch_ssrf_rejection_does_not_leak_url_credentials() {
        let config = CrawlConfig::default();
        let client = build_client(&config).expect("client must build");
        let url = "http://alice:hunter2@169.254.169.254/latest/meta-data/";

        let err = match http_fetch(url, &config, &std::collections::HashMap::new(), &client).await {
            Err(e) => e,
            Ok(_) => panic!("the link-local metadata address must be refused by the default policy"),
        };

        let rendered = format!("{err}\n{err:?}");
        assert!(
            !rendered.contains("hunter2"),
            "the refused URL's password must never reach the error, got {rendered}"
        );
        assert!(
            !rendered.contains("alice"),
            "the refused URL's username must never reach the error, got {rendered}"
        );
        assert!(
            rendered.contains("169.254.169.254"),
            "the host must survive redaction so the error stays actionable, got {rendered}"
        );
    }

    /// Same redirect chain as above, but with a builder value large enough to reach the
    /// end — proving `.max_redirects(N)` is actually honored (not just enforced too
    /// tightly) by the same loop.
    #[tokio::test]
    async fn http_fetch_follows_full_chain_when_builder_max_redirects_is_sufficient() {
        let mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/hop0"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/hop1"))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/hop1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("done"))
            .mount(&mock)
            .await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        config.max_redirects = 1;
        let client = build_client(&config).expect("client must build");
        let url = format!("{}/hop0", mock.uri());
        let resp = http_fetch(&url, &config, &std::collections::HashMap::new(), &client)
            .await
            .expect("one redirect hop must be followed when max_redirects == 1");

        assert_eq!(
            resp.body, "done",
            "final response body must be the hop1 body, got {:?}",
            resp.body
        );
        assert_eq!(resp.status, 200, "final status must be 200, got {}", resp.status);
    }
    /// Characterization for the status dispatch `fetch_one_hop` performs: every status
    /// that ends a fetch without reading the body maps to one specific error. ~keep
    #[tokio::test]
    async fn http_fetch_maps_each_body_free_terminal_status_to_its_own_error() {
        /// (status, the variant it must raise, a fragment its message must carry).
        type StatusCase = (u16, fn(&CrawlError) -> bool, &'static str);

        let cases: &[StatusCase] = &[
            (401, |e| matches!(e, CrawlError::Unauthorized { .. }), "unauthorized"),
            (404, |e| matches!(e, CrawlError::NotFound { .. }), "not_found"),
            (408, |e| matches!(e, CrawlError::Timeout { .. }), "timeout"),
            (410, |e| matches!(e, CrawlError::Gone { .. }), "gone"),
            (429, |e| matches!(e, CrawlError::RateLimited { .. }), "rate_limited"),
            (500, |e| matches!(e, CrawlError::ServerError { .. }), "server_error"),
            (502, |e| matches!(e, CrawlError::BadGateway { .. }), "bad_gateway"),
            (
                503,
                |e| matches!(e, CrawlError::ServerError { .. }),
                SERVICE_UNAVAILABLE_SUFFIX,
            ),
            (
                504,
                |e| matches!(e, CrawlError::ServerError { .. }),
                GATEWAY_TIMEOUT_SUFFIX,
            ),
        ];

        for (status, is_expected_variant, message_fragment) in cases {
            let error = fetch_status(*status, ResponseTemplate::new(*status)).await;
            assert!(
                is_expected_variant(&error),
                "status {status} produced the wrong error variant: {error:?}"
            );
            assert!(
                error.to_string().contains(message_fragment),
                "status {status} message must contain {message_fragment:?}, got: {error}"
            );
        }
    }

    /// A 403 that carries no WAF fingerprint is a plain forbidden, not a WAF block.
    #[tokio::test]
    async fn http_fetch_reports_a_plain_403_as_forbidden() {
        let error = fetch_status(403, ResponseTemplate::new(403).set_body_string("nope")).await;
        assert!(
            matches!(error, CrawlError::Forbidden { .. }),
            "expected Forbidden, got {error:?}"
        );
    }

    /// A 403 whose body fingerprints must name the vendor the corpus identifies. ~keep
    #[tokio::test]
    async fn http_fetch_reports_a_waf_block_when_a_403_body_fingerprints() {
        let error = fetch_status(403, ResponseTemplate::new(403).set_body_string("cf-chl- challenge")).await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "cloudflare"),
            "expected a cloudflare WafBlocked, got {error:?}"
        );
        assert!(
            error.to_string().contains("waf/blocked detected: cloudflare"),
            "unexpected message: {error}"
        );
    }

    /// A 2xx whose headers alone fingerprint is a WAF interstitial, reported before the
    /// body is treated as page content. ~keep
    #[tokio::test]
    async fn http_fetch_reports_a_header_waf_block_on_a_2xx() {
        let error = fetch_status(
            200,
            ResponseTemplate::new(200)
                .append_header("x-datadome", "protected")
                .set_body_string("<html></html>"),
        )
        .await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "datadome"),
            "expected a datadome WafBlocked, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("waf/blocked detected on 2xx (header): datadome"),
            "unexpected message: {error}"
        );
    }

    /// A 2xx that only fingerprints once its body is read is reported as a body block.
    #[tokio::test]
    async fn http_fetch_reports_a_body_waf_block_on_a_2xx() {
        let error = fetch_status(
            200,
            ResponseTemplate::new(200).set_body_string("<html>cf-chl- x</html>"),
        )
        .await;
        assert!(
            matches!(&error, CrawlError::WafBlocked { vendor, .. } if vendor == "cloudflare"),
            "expected a cloudflare WafBlocked, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("waf/blocked detected on 2xx (body): cloudflare"),
            "unexpected message: {error}"
        );
    }

    /// A 3xx whose `Location` does not resolve to a URL is returned as the response
    /// rather than followed or rejected. ~keep
    #[tokio::test]
    async fn http_fetch_returns_a_3xx_with_an_unresolvable_location_as_the_response() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/here"))
            .respond_with(
                ResponseTemplate::new(302)
                    .append_header("location", "http://")
                    .set_body_string("moved"),
            )
            .mount(&mock)
            .await;

        let config = permissive_config();
        let client = build_client(&config).expect("client must build");
        let response = http_fetch(&format!("{}/here", mock.uri()), &config, &HashMap::new(), &client)
            .await
            .expect("an unresolvable Location must not fail the fetch");

        assert_eq!(response.status, 302, "the 3xx itself must be returned");
        assert_eq!(response.body, "moved", "its body must be read");
    }

    fn permissive_config() -> CrawlConfig {
        CrawlConfig {
            ssrf: SsrfPolicy {
                deny_private: false,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        }
    }

    /// Fetch a single mocked response and return the error it produced.
    async fn fetch_status(status: u16, template: ResponseTemplate) -> CrawlError {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(template)
            .mount(&mock)
            .await;

        let config = permissive_config();
        let client = build_client(&config).expect("client must build");
        http_fetch(&format!("{}/probe", mock.uri()), &config, &HashMap::new(), &client)
            .await
            .map(|_| ())
            .expect_err(&format!("status {status} must produce an error"))
    }
}
