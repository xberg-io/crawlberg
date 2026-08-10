//! HTTP fetching with redirect handling, retry logic, and cookie extraction.

use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderMap, USER_AGENT};

use crate::error::{CrawlError, classify_reqwest_error, error_chain_string};
use crate::net::ssrf::validate_url;
#[cfg(not(target_arch = "wasm32"))]
use crate::types::CookieInfo;
use crate::types::WafClassifier;
use crate::types::{AuthConfig, CrawlConfig, ResponseMeta};
use crate::waf::TomlClassifier;

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

/// Truncate `body` to at most `max_size` bytes without splitting a UTF-8 character.
///
/// `String::truncate` panics when the index is not a char boundary. `max_size` comes
/// from `CrawlConfig::max_body_size`, so on any non-ASCII page an unlucky byte count
/// would otherwise panic the crawl.
pub(crate) fn truncate_body_at_char_boundary(body: &mut String, max_size: usize) {
    if body.len() <= max_size {
        return;
    }
    let mut boundary = max_size;
    while boundary > 0 && !body.is_char_boundary(boundary) {
        boundary -= 1;
    }
    body.truncate(boundary);
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
    let initial_url = url::Url::parse(url).map_err(|e| CrawlError::SsrfPolicyViolation {
        url: url.to_string(),
        reason: format!("invalid URL: {e}"),
    })?;

    validate_url(&initial_url, &config.ssrf)
        .await
        .map_err(|e| CrawlError::SsrfPolicyViolation {
            url: url.to_string(),
            reason: e.to_string(),
        })?;

    let mut current_url = initial_url.clone();
    let mut final_url_str: String;
    let mut redirects_followed = 0u8;

    loop {
        let mut req = client.get(current_url.to_string());

        if let Some(ref ua) = config.user_agent {
            req = req.header(USER_AGENT, ua.as_str());
        } else {
            req = req.header(USER_AGENT, concat!("crawlberg/", env!("CARGO_PKG_VERSION")));
        }

        match config.auth {
            Some(AuthConfig::Basic {
                ref username,
                ref password,
            }) => {
                req = req.basic_auth(username, Some(password));
            }
            Some(AuthConfig::Bearer { ref token }) => {
                req = req.bearer_auth(token);
            }
            Some(AuthConfig::Header { ref name, ref value }) => {
                req = req.header(name.as_str(), value.as_str());
            }
            None => {}
        }

        for (k, v) in &config.custom_headers {
            req = req.header(k.as_str(), v.as_str());
        }

        for (k, v) in extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = req.send().await.map_err(|e| classify_reqwest_error(&e))?;

        let status = resp.status().as_u16();
        final_url_str = resp.url().to_string();

        let content_type = resp
            .headers()
            .get_all(CONTENT_TYPE)
            .iter()
            .next_back()
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let headers = resp.headers().clone();

        if (300..400).contains(&status) {
            let location_header = headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);

            if let Some(location) = location_header {
                let next_url = match current_url.join(&location) {
                    Ok(u) => u,
                    Err(_) => {
                        let body_bytes_vec = resp.bytes().await.unwrap_or_default().to_vec();
                        let body = String::from_utf8_lossy(&body_bytes_vec).into_owned();
                        let mut headers_map: std::collections::HashMap<String, Vec<String>> =
                            std::collections::HashMap::new();
                        for (name, value) in headers.iter() {
                            if let Ok(v) = value.to_str() {
                                headers_map
                                    .entry(name.as_str().to_lowercase())
                                    .or_default()
                                    .push(v.to_string());
                            }
                        }
                        return Ok(HttpResponse {
                            status,
                            content_type,
                            body,
                            body_bytes: body_bytes_vec,
                            headers: headers_map,
                            browser_extras: None,
                            final_url: final_url_str,
                        });
                    }
                };

                if let Err(e) = validate_url(&next_url, &config.ssrf).await {
                    return Err(CrawlError::SsrfPolicyViolation {
                        url: next_url.to_string(),
                        reason: e.to_string(),
                    });
                }

                redirects_followed += 1;
                if redirects_followed > config.ssrf.max_redirects {
                    return Err(CrawlError::SsrfPolicyViolation {
                        url: next_url.to_string(),
                        reason: "too many redirects".to_string(),
                    });
                }

                current_url = next_url;
                continue;
            }
        }

        match status {
            401 => return Err(CrawlError::Unauthorized("unauthorized".into())),
            403 => {
                let body = resp.text().await.unwrap_or_default();
                let partial_response = build_partial_response(status, &body, &headers);
                let classifier = TomlClassifier::builtin();
                if let Ok(Some(signal)) = classifier.classify(&partial_response) {
                    return Err(CrawlError::WafBlocked {
                        vendor: signal.vendor.clone(),
                        message: format!("waf/blocked detected: {}", signal.vendor),
                    });
                }
                return Err(CrawlError::Forbidden("forbidden".into()));
            }
            404 => return Err(CrawlError::NotFound(format!("not_found: {url}"))),
            408 => return Err(CrawlError::Timeout("timeout: request timed out".into())),
            410 => return Err(CrawlError::Gone("gone".into())),
            429 => return Err(CrawlError::RateLimited("rate_limited".into())),
            500 => return Err(CrawlError::ServerError("server_error".into())),
            502 => return Err(CrawlError::BadGateway("bad_gateway".into())),
            503 => {
                return Err(CrawlError::ServerError("server_error: service unavailable".into()));
            }
            _ => {}
        }

        // ~keep Header-only WAF fingerprints must fire before reading a 2xx body as real content.
        // ~keep The TOML corpus is the single WAF source of truth; do not hardcode header lists here.
        if (200..300).contains(&status) {
            let headers_only_response = build_partial_response(status, "", &headers);
            let classifier = TomlClassifier::builtin();
            if let Ok(Some(signal)) = classifier.classify(&headers_only_response) {
                let body = resp.text().await.unwrap_or_default();
                let partial_response = build_partial_response(status, &body, &headers);
                let vendor = classifier
                    .classify(&partial_response)
                    .ok()
                    .flatten()
                    .map(|s| s.vendor)
                    .unwrap_or(signal.vendor);
                return Err(CrawlError::WafBlocked {
                    message: format!("waf/blocked detected on 2xx (header): {vendor}"),
                    vendor,
                });
            }
        }

        let expected_len = headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok());

        let body_bytes = resp.bytes().await.map_err(|e| {
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
                CrawlError::DataLoss(format!("data_loss: {e}"))
            } else {
                classify_reqwest_error(&e)
            }
        })?;

        if let Some(expected) = expected_len
            && body_bytes.len() < expected
            && expected - body_bytes.len() > 100
        {
            return Err(CrawlError::DataLoss(format!(
                "data_loss: expected {expected} bytes, got {}",
                body_bytes.len()
            )));
        }

        let body_bytes_vec = body_bytes.to_vec();
        let body = String::from_utf8_lossy(&body_bytes_vec).into_owned();

        // ~keep Small 2xx bodies with high-confidence vendor JS fingerprints are treated as WAF interstitials.
        if (200..300).contains(&status) {
            let partial_response = build_partial_response_with_bytes(status, &body_bytes_vec, &body, &headers);
            let classifier = TomlClassifier::builtin();
            if let Ok(Some(signal)) = classifier.classify(&partial_response) {
                return Err(CrawlError::WafBlocked {
                    vendor: signal.vendor.clone(),
                    message: format!("waf/blocked detected on 2xx (body): {}", signal.vendor),
                });
            }
        }

        let mut headers_map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for (name, value) in headers.iter() {
            if let Ok(v) = value.to_str() {
                headers_map
                    .entry(name.as_str().to_lowercase())
                    .or_default()
                    .push(v.to_string());
            }
        }

        return Ok(HttpResponse {
            status,
            content_type,
            body,
            body_bytes: body_bytes_vec,
            headers: headers_map,
            browser_extras: None,
            final_url: final_url_str,
        });
    }
}

/// Build a `reqwest::Client` with the given configuration (redirect policy, timeout, cookies, proxy).
#[cfg_attr(target_arch = "wasm32", allow(unused_variables, unused_mut))]
pub(crate) fn build_client(config: &CrawlConfig) -> Result<reqwest::Client, CrawlError> {
    let mut builder = reqwest::Client::builder();

    #[cfg(not(target_arch = "wasm32"))]
    {
        builder = builder
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.request_timeout);
    }

    #[cfg(not(target_arch = "wasm32"))]
    if config.cookies_enabled {
        builder = builder.cookie_store(true);
    }

    // ~keep `proxy_provider` takes precedence over static proxy so reqwest can rotate per request.
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(provider) = config.proxy_provider.clone() {
        let proxy = reqwest::Proxy::custom(move |url| {
            let host = url.host_str().unwrap_or("");
            let cfg = provider.next_proxy(host)?;
            let mut parsed = reqwest::Url::parse(&cfg.url).ok()?;
            if let (Some(user), Some(pass)) = (&cfg.username, &cfg.password) {
                let _ = parsed.set_username(user);
                let _ = parsed.set_password(Some(pass));
            }
            Some(parsed)
        });
        builder = builder.proxy(proxy);
    } else if let Some(ref proxy_config) = config.proxy {
        let mut proxy = reqwest::Proxy::all(&proxy_config.url)
            .map_err(|e| CrawlError::InvalidConfig(format!("invalid proxy URL: {e}")))?;
        if let (Some(user), Some(pass)) = (&proxy_config.username, &proxy_config.password) {
            proxy = proxy.basic_auth(user, pass);
        }
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|e| CrawlError::Other(format!("Failed to build HTTP client: {e}")))
}

/// Fetch a URL with retry logic based on configuration.
///
/// Retries on server errors and rate limiting if the corresponding status codes
/// are included in `config.retry_codes`. Uses exponential backoff between retries.
pub(crate) async fn fetch_with_retry(
    url: &str,
    config: &CrawlConfig,
    extra_headers: &std::collections::HashMap<String, String>,
    client: &reqwest::Client,
) -> Result<HttpResponse, CrawlError> {
    let retries = config.retry_count;
    let retry_codes = config.retry_codes.clone();

    let mut last_err = None;
    for attempt in 0..=retries {
        match http_fetch(url, config, extra_headers, client).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let should_retry = match &e {
                    CrawlError::ServerError(_) => retry_codes.contains(&503) || retry_codes.contains(&500),
                    CrawlError::RateLimited(_) => retry_codes.contains(&429),
                    _ => false,
                };
                if should_retry && attempt < retries {
                    let delay = Duration::from_millis(100 * (1 << attempt));
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| CrawlError::Other("retry exhausted".into())))
}

/// Extract cookies from a `HashMap<String, Vec<String>>` of response headers.
///
/// Looks for the `"set-cookie"` key and parses each value as an individual
/// Set-Cookie header, preserving all cookies from the response.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn extract_cookies_from_hashmap(
    headers: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<CookieInfo> {
    let mut cookies = Vec::new();
    if let Some(values) = headers.get("set-cookie") {
        for raw in values {
            let parts: Vec<&str> = raw.split(';').collect();
            if let Some(nv) = parts.first()
                && let Some((name, value)) = nv.split_once('=')
            {
                let mut cookie = CookieInfo {
                    name: name.trim().to_owned(),
                    value: value.trim().to_owned(),
                    domain: None,
                    path: None,
                };
                for attr in &parts[1..] {
                    let attr = attr.trim().to_lowercase();
                    if let Some(d) = attr.strip_prefix("domain=") {
                        cookie.domain = Some(d.to_owned());
                    } else if let Some(p) = attr.strip_prefix("path=") {
                        cookie.path = Some(p.to_owned());
                    }
                }
                cookies.push(cookie);
            }
        }
    }
    cookies
}

/// Extract response metadata from a `HashMap<String, Vec<String>>` of headers.
pub(crate) fn extract_response_meta_from_hashmap(
    headers: &std::collections::HashMap<String, Vec<String>>,
) -> ResponseMeta {
    ResponseMeta {
        etag: headers.get("etag").and_then(|v| v.first().cloned()),
        last_modified: headers.get("last-modified").and_then(|v| v.first().cloned()),
        cache_control: headers.get("cache-control").and_then(|v| v.first().cloned()),
        server: headers.get("server").and_then(|v| v.first().cloned()),
        x_powered_by: headers.get("x-powered-by").and_then(|v| v.first().cloned()),
        content_language: headers.get("content-language").and_then(|v| v.first().cloned()),
        content_encoding: headers.get("content-encoding").and_then(|v| v.first().cloned()),
    }
}

/// Build a partial [`HttpResponse`] from reqwest header map + body string.
///
/// Used in the early-exit detection paths where we need to pass a response
/// to [`crate::types::WafClassifier::classify`] before the full
/// [`HttpResponse`] struct is assembled.
fn build_partial_response(status: u16, body: &str, headers: &HeaderMap) -> HttpResponse {
    let body_bytes = body.as_bytes().to_vec();
    build_partial_response_with_bytes(status, &body_bytes, body, headers)
}

/// Build a partial [`HttpResponse`] with a pre-computed byte vec.
fn build_partial_response_with_bytes(status: u16, body_bytes: &[u8], body: &str, headers: &HeaderMap) -> HttpResponse {
    let mut headers_map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            headers_map
                .entry(name.as_str().to_lowercase())
                .or_default()
                .push(v.to_string());
        }
    }
    HttpResponse {
        status,
        content_type: String::new(),
        body: body.to_string(),
        body_bytes: body_bytes.to_vec(),
        headers: headers_map,
        browser_extras: None,
        final_url: String::new(),
    }
}

/// Identify the WAF vendor from server header value and body content.
///
/// Delegates to [`TomlClassifier::builtin`]. Kept for backward compatibility
/// with callers in `tower/service.rs`.
///
/// Callers are all gated behind `#[cfg(not(target_arch = "wasm32"))]`; the
/// function is gated here to keep the wasm build warning-free under `-D warnings`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn detect_waf_vendor(server: &str, body: &str) -> String {
    let body_bytes = body.as_bytes().to_vec();
    let mut headers_map: HashMap<String, Vec<String>> = HashMap::new();
    if !server.is_empty() {
        headers_map
            .entry("server".to_string())
            .or_default()
            .push(server.to_string());
    }
    let response = HttpResponse {
        status: 403,
        content_type: String::new(),
        body: body.to_string(),
        body_bytes,
        headers: headers_map,
        browser_extras: None,
        final_url: String::new(),
    };
    TomlClassifier::builtin()
        .classify(&response)
        .ok()
        .flatten()
        .map(|s| s.vendor)
        .unwrap_or_else(|| "unknown".to_string())
}

/// Returns true if `response` is a WAF block.
///
/// Delegates to [`TomlClassifier::builtin`]. Kept for backward compatibility
/// with callers outside `http_fetch` (e.g. the browser backend).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn is_waf_blocked(server: &str, body: &str, headers: &HashMap<String, Vec<String>>) -> bool {
    let body_bytes = body.as_bytes().to_vec();
    let mut headers_map: HashMap<String, Vec<String>> = HashMap::new();
    for (k, values) in headers {
        headers_map.insert(k.to_lowercase(), values.clone());
    }
    if !server.is_empty() {
        headers_map
            .entry("server".to_string())
            .or_default()
            .push(server.to_string());
    }
    let response = HttpResponse {
        status: 403,
        content_type: String::new(),
        body: body.to_string(),
        body_bytes,
        headers: headers_map,
        browser_extras: None,
        final_url: String::new(),
    };
    TomlClassifier::builtin().classify(&response).ok().flatten().is_some()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    #[test]
    fn truncate_body_never_splits_a_utf8_character() {
        // ~keep Regression: String::truncate panics on a non-char-boundary index, and
        // max_body_size is user-supplied, so any non-ASCII page could panic the crawl.
        let original = "héllo wörld";
        for max_size in 0..=original.len() {
            let mut body = original.to_string();
            truncate_body_at_char_boundary(&mut body, max_size);
            assert!(
                body.len() <= max_size,
                "truncation to {max_size} produced {} bytes",
                body.len()
            );
            assert!(
                original.starts_with(&body),
                "truncation to {max_size} must yield a prefix, got {body:?}"
            );
        }
    }

    #[test]
    fn truncate_body_leaves_short_bodies_untouched() {
        let mut body = "abc".to_string();
        truncate_body_at_char_boundary(&mut body, 100);
        assert_eq!(body, "abc", "a body under the limit must not be modified");
    }
}
