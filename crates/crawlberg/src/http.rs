//! HTTP fetching with redirect handling, retry logic, and cookie extraction.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
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

/// Re-decode `body_bytes` using `charset` when it names a recognized non-UTF-8 encoding.
///
/// ~keep Shared by the scrape path (`scrape.rs`) and the crawl path
/// (`engine/crawl_loop.rs`) so both apply the charset `detect_charset` reports instead
/// of only reporting it. `resp.body`/`fetch.body` is always a `String::from_utf8_lossy`
/// decode of the raw bytes (see `read_body_bounded` callers in this file and in
/// `tower/service.rs`); for any non-UTF-8/us-ascii charset that lossy decode has
/// already replaced every non-ASCII byte with U+FFFD, so callers must re-decode from
/// `body_bytes` rather than post-process the lossy string.
///
/// Returns `None` — meaning "keep the caller's existing lossy-UTF-8 body" — when
/// `charset` is `"utf-8"`/`"us-ascii"`, is not a label `encoding_rs` recognizes, or
/// decoding hit unmappable sequences (an unreliable decode is worse than the lossy
/// fallback, which at least round-trips the ASCII-safe portion of the page).
pub(crate) fn redecode_with_charset(charset: &str, body_bytes: &[u8]) -> Option<String> {
    if charset == "utf-8" || charset == "us-ascii" {
        return None;
    }
    let encoding = encoding_rs::Encoding::for_label(charset.as_bytes())?;
    let (decoded, _, had_errors) = encoding.decode(body_bytes);
    if had_errors { None } else { Some(decoded.into_owned()) }
}

/// Read a response body in bounded chunks, stopping once more than `max_size` bytes
/// have been received. Returns the bytes read together with whether the read stopped
/// early because the cap was hit (as opposed to a natural end-of-body).
///
/// ~keep `resp.bytes()` buffers the *entire* body — including whatever reqwest's
/// transparent gzip/brotli decompression produces — before `max_body_size` truncation
/// ever runs downstream, so a decompression bomb (e.g. a 10 GB gzip response behind a
/// 1 MB cap) still allocates its full decompressed size. Reading chunk-by-chunk via
/// `Response::chunk` (available without the `stream` cargo feature, unlike
/// `bytes_stream`) and stopping as soon as the cap is crossed bounds peak memory to
/// roughly `max_size` plus one chunk width, regardless of the declared or true
/// decompressed size — the cap is enforced while reading, not after the fact.
///
/// When `max_size` is `None`, reads to completion exactly as `resp.bytes()` would.
///
/// Takes `resp` by value: every call site reads the body as its last operation on the
/// response before returning or moving on to the next redirect hop, and the native
/// implementation needs `&mut` access to `Response::chunk` internally.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn read_body_bounded(
    mut resp: reqwest::Response,
    max_size: Option<usize>,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    let mut buf: Vec<u8> = Vec::with_capacity(max_size.unwrap_or(8192).min(1 << 20));
    while let Some(chunk) = resp.chunk().await? {
        buf.extend_from_slice(&chunk);
        if let Some(max_size) = max_size
            && buf.len() > max_size
        {
            return Ok((buf, true));
        }
    }
    Ok((buf, false))
}

/// wasm32 fallback: the browser-`fetch`-backed `reqwest::Response` on this target does
/// not expose `Response::chunk` (only `bytes()`, which reads to completion, or
/// `bytes_stream()`, which requires the `stream` cargo feature this crate does not
/// enable). The memory-exhaustion threat this bounds — a malicious server streaming an
/// unbounded decompression bomb at a long-running native crawler process — does not
/// apply the same way inside a browser's sandboxed wasm runtime, so this reads to
/// completion and always reports `hit_cap = false`; callers still apply
/// `max_body_size` truncation to the result afterwards, matching prior wasm behavior.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn read_body_bounded(
    resp: reqwest::Response,
    _max_size: Option<usize>,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    Ok((resp.bytes().await?.to_vec(), false))
}

/// Read a response body via [`read_body_bounded`] and lossily decode it as UTF-8,
/// returning an empty string on any read error.
///
/// Used by WAF-classification paths that only need best-effort body text and already
/// tolerate a missing body (they previously used `resp.text().await.unwrap_or_default()`,
/// which has the same unbounded-memory problem `read_body_bounded` fixes).
pub(crate) async fn read_text_bounded(resp: reqwest::Response, max_size: Option<usize>) -> String {
    match read_body_bounded(resp, max_size).await {
        Ok((bytes, _)) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
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
    let initial_url =
        url::Url::parse(url).map_err(|e| CrawlError::ssrf_violation(url, format!("invalid URL: {e}")))?;

    validate_url(&initial_url, &config.ssrf)
        .await
        .map_err(|e| CrawlError::ssrf_violation(url.to_string(), e.to_string()))?;

    let mut current_url = initial_url.clone();
    let mut final_url_str: String;
    let mut redirects_followed: usize = 0;

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
                        let (body_bytes_vec, _) =
                            read_body_bounded(resp, config.max_body_size).await.unwrap_or_default();
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
                    return Err(CrawlError::ssrf_violation(next_url.to_string(), e.to_string()));
                }

                redirects_followed += 1;
                // ~keep `CrawlConfig.max_redirects` is the single effective redirect-hop bound for
                // every GET, matching `follow_redirects` in `engine/crawl_loop.rs`. It is the only
                // one of the two redirect-count fields exposed by the builder (see
                // `types/builder.rs::max_redirects`); `SsrfPolicy.max_redirects` is kept only for
                // backward-compatible (de)serialization of the SSRF policy shape (it is part of the
                // fixtures/schema.json contract) and is not read at runtime — SSRF safety itself is
                // unaffected because every redirect target is still validated by `validate_url` below.
                if redirects_followed > config.max_redirects {
                    return Err(CrawlError::ssrf_violation(next_url.to_string(), "too many redirects".to_string()));
                }

                current_url = next_url;
                continue;
            }
        }

        match status {
            401 => return Err(CrawlError::Unauthorized("unauthorized".into())),
            403 => {
                let body = read_text_bounded(resp, config.max_body_size).await;
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
                return Err(CrawlError::ServerError(format!("server_error: {SERVICE_UNAVAILABLE_SUFFIX}")));
            }
            504 => {
                return Err(CrawlError::ServerError(format!("server_error: {GATEWAY_TIMEOUT_SUFFIX}")));
            }
            _ => {}
        }

        // ~keep Header-only WAF fingerprints must fire before reading a 2xx body as real content.
        // ~keep The TOML corpus is the single WAF source of truth; do not hardcode header lists here.
        if (200..300).contains(&status) {
            let headers_only_response = build_partial_response(status, "", &headers);
            let classifier = TomlClassifier::builtin();
            if let Ok(Some(signal)) = classifier.classify(&headers_only_response) {
                let body = read_text_bounded(resp, config.max_body_size).await;
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

        let (body_bytes_vec, hit_cap) = read_body_bounded(resp, config.max_body_size).await.map_err(|e| {
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

        // ~keep A capped read stopping short of `content-length` is expected (that is the
        // point of `max_body_size`), not evidence of a truncated/failed transfer.
        if !hit_cap
            && let Some(expected) = expected_len
            && body_bytes_vec.len() < expected
            && expected - body_bytes_vec.len() > 100
        {
            return Err(CrawlError::DataLoss(format!(
                "data_loss: expected {expected} bytes, got {}",
                body_bytes_vec.len()
            )));
        }

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

/// Identity of the `reqwest::Client` configuration knobs that legitimately vary per
/// fetch — timeout, cookie jar, proxy, and auth — used to key the shared client cache
/// in [`build_client`] so that requests sharing an identity reuse one connection pool
/// instead of paying a fresh TCP/TLS handshake on every call.
///
/// Auth is included even though it is applied as a per-request header (not baked into
/// the `reqwest::Client` itself) because a shared client's cookie jar (when
/// `cookies_enabled`) must not be reused across distinct credentials — otherwise two
/// concurrent sessions to the same host with different auth would leak session cookies
/// between them.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ClientCacheKey {
    timeout_micros: u128,
    cookies_enabled: bool,
    proxy: String,
    auth: String,
}

impl ClientCacheKey {
    fn from_config(config: &CrawlConfig) -> Self {
        Self {
            timeout_micros: config.request_timeout.as_micros(),
            cookies_enabled: config.cookies_enabled,
            proxy: proxy_identity(config),
            auth: auth_identity(config),
        }
    }
}

/// Encode `config`'s proxy configuration as an opaque identity string.
///
/// A `ProxyProvider` is a trait object with no `Eq`/`Hash` impl, so its identity is its
/// `Arc` data address — two `CrawlConfig`s sharing the same provider `Arc` (the normal
/// case: one engine, cloned config) resolve to the same key.
fn proxy_identity(config: &CrawlConfig) -> String {
    if let Some(ref provider) = config.proxy_provider {
        format!("provider:{:p}", std::sync::Arc::as_ptr(provider))
    } else if let Some(ref proxy) = config.proxy {
        format!(
            "static:{}:{}:{}",
            proxy.url,
            proxy.username.as_deref().unwrap_or(""),
            proxy.password.as_deref().unwrap_or("")
        )
    } else {
        "none".to_owned()
    }
}

/// Encode `config`'s auth configuration as an opaque identity string.
fn auth_identity(config: &CrawlConfig) -> String {
    match &config.auth {
        Some(AuthConfig::Basic { username, password }) => format!("basic:{username}:{password}"),
        Some(AuthConfig::Bearer { token }) => format!("bearer:{token}"),
        Some(AuthConfig::Header { name, value }) => format!("header:{name}:{value}"),
        None => "none".to_owned(),
    }
}

/// Process-wide cache of built `reqwest::Client`s, keyed by [`ClientCacheKey`].
///
/// ~keep `build_client` is called on the hot fetch path (once per tier attempt in
/// `engine/mod.rs::run_tier`), so without this cache every HTTP request pays a fresh
/// TCP/TLS handshake and gets no connection-pool reuse. `reqwest::Client` is
/// `Arc`-backed internally, so cloning a cached entry is cheap, and each distinct
/// proxy/auth/timeout/cookie identity still gets its own client rather than one client
/// silently serving unrelated sessions (see [`ClientCacheKey`]).
fn client_cache() -> &'static Mutex<HashMap<ClientCacheKey, reqwest::Client>> {
    static CACHE: OnceLock<Mutex<HashMap<ClientCacheKey, reqwest::Client>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Upper bound on distinct cached clients.
///
/// ~keep An unbounded cache is a slow leak: a long-lived process that rotates proxies
/// or per-tenant auth mints a new identity per rotation and never releases the old
/// client — nor the provider `Arc` captured inside it. Past this many entries the cache
/// is cleared wholesale rather than evicted by recency; entries are interchangeable
/// (rebuilding one costs a handshake, not correctness), so tracking access order would
/// buy nothing for the extra state.
const MAX_CACHED_CLIENTS: usize = 64;

/// Whether a cached client already exists for `config`'s identity. Test-only
/// introspection for verifying [`build_client`]'s caching behavior.
#[cfg(test)]
pub(crate) fn client_cache_contains(config: &CrawlConfig) -> bool {
    let key = ClientCacheKey::from_config(config);
    client_cache()
        .lock()
        .map(|cache| cache.contains_key(&key))
        .unwrap_or(false)
}

/// Build a `reqwest::Client` with the given configuration (redirect policy, timeout, cookies, proxy).
///
/// Returns a cached, cheaply-cloned client when one matching this configuration's
/// [`ClientCacheKey`] already exists; otherwise builds one and caches it for reuse.
#[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
pub(crate) fn build_client(config: &CrawlConfig) -> Result<reqwest::Client, CrawlError> {
    let key = ClientCacheKey::from_config(config);
    if let Ok(cache) = client_cache().lock()
        && let Some(client) = cache.get(&key)
    {
        return Ok(client.clone());
    }

    let mut builder = reqwest::Client::builder();

    #[cfg(not(target_arch = "wasm32"))]
    {
        builder = builder
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.request_timeout);
    }

    // ~keep `cookie_provider`, not `cookie_store(true)`: reqwest's default jar loads no
    // public-suffix list, so a host may set Domain= to a shared multi-tenant suffix
    // (herokuapp.com, github.io, a bare TLD). One client is reused across a whole crawl
    // that can span hosts, so that would be a supercookie leak between unrelated tenants.
    #[cfg(not(target_arch = "wasm32"))]
    if config.cookies_enabled {
        builder = builder.cookie_provider(std::sync::Arc::new(crate::net::cookie::PolicyCookieStore::default()));
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

    let client = builder
        .build()
        .map_err(|e| CrawlError::Other(format!("Failed to build HTTP client: {e}")))?;

    if let Ok(mut cache) = client_cache().lock() {
        if cache.len() >= MAX_CACHED_CLIENTS {
            tracing::debug!(
                cached = cache.len(),
                cap = MAX_CACHED_CLIENTS,
                "HTTP client cache full, clearing"
            );
            cache.clear();
        }
        cache.insert(key, client.clone());
    }

    Ok(client)
}

/// Substring appended to a `CrawlError::ServerError` message for a 503 response.
///
/// ~keep `CrawlError::ServerError` carries a single free-form `String` with no numeric
/// status field (see `error.rs`), so 500, 503, and 504 all raise the same enum variant.
/// `should_retry_status` matches on this substring to tell them apart when deciding
/// which `retry_codes` entry governs a given failure — without it, configuring a retry
/// for one of the three silently also retried (or failed to retry) the other two.
const SERVICE_UNAVAILABLE_SUFFIX: &str = "service unavailable";

/// Substring appended to a `CrawlError::ServerError` message for a 504 response.
/// See [`SERVICE_UNAVAILABLE_SUFFIX`] for why this disambiguation is needed.
const GATEWAY_TIMEOUT_SUFFIX: &str = "gateway timeout";

/// Decide whether `error` should trigger a retry, given the configured `retry_codes`.
///
/// Each configured status code retries exactly the failures that produced it: 500 only
/// retries a plain `ServerError`, 503 only a `ServerError` carrying
/// [`SERVICE_UNAVAILABLE_SUFFIX`], 504 only one carrying [`GATEWAY_TIMEOUT_SUFFIX`], 502
/// only `BadGateway`, 408 only `Timeout`, and 429 only `RateLimited`. Any other error
/// (including statuses not covered by `retry_codes`) never retries.
fn should_retry_status(error: &CrawlError, retry_codes: &[u16]) -> bool {
    match error {
        CrawlError::ServerError(msg) if msg.contains(GATEWAY_TIMEOUT_SUFFIX) => retry_codes.contains(&504),
        CrawlError::ServerError(msg) if msg.contains(SERVICE_UNAVAILABLE_SUFFIX) => retry_codes.contains(&503),
        CrawlError::ServerError(_) => retry_codes.contains(&500),
        CrawlError::BadGateway(_) => retry_codes.contains(&502),
        CrawlError::Timeout(_) => retry_codes.contains(&408),
        CrawlError::RateLimited(_) => retry_codes.contains(&429),
        _ => false,
    }
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
                let should_retry = should_retry_status(&e, &retry_codes);
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

    #[test]
    fn redecode_with_charset_decodes_windows_1252_bytes_exactly() {
        // ~keep Real Windows-1252 bytes for "café €100" (verified via Python's `str.encode`).
        // A UTF-8-lossy decode of these bytes would replace 0xE9 and 0x80 with U+FFFD.
        let bytes: &[u8] = &[0x63, 0x61, 0x66, 0xE9, 0x20, 0x80, 0x31, 0x30, 0x30];
        let decoded = redecode_with_charset("windows-1252", bytes);
        assert_eq!(
            decoded,
            Some("café €100".to_owned()),
            "windows-1252 bytes must decode to the exact original string, got {decoded:?}"
        );
    }

    #[test]
    fn redecode_with_charset_decodes_shift_jis_bytes_exactly() {
        // ~keep Real Shift_JIS bytes for "日本語 テスト" (verified via Python's `str.encode`).
        let bytes: &[u8] = &[
            0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA, 0x20, 0x83, 0x65, 0x83, 0x58, 0x83, 0x67,
        ];
        let decoded = redecode_with_charset("shift_jis", bytes);
        assert_eq!(
            decoded,
            Some("日本語 テスト".to_owned()),
            "shift_jis bytes must decode to the exact original string, got {decoded:?}"
        );
    }

    #[test]
    fn redecode_with_charset_is_a_noop_for_utf8() {
        let decoded = redecode_with_charset("utf-8", "hello".as_bytes());
        assert_eq!(
            decoded, None,
            "utf-8 must be a no-op (caller keeps its existing lossy body), got {decoded:?}"
        );
    }

    #[test]
    fn redecode_with_charset_returns_none_for_unrecognized_label() {
        let decoded = redecode_with_charset("not-a-real-charset", b"hello");
        assert_eq!(
            decoded, None,
            "an unrecognized charset label must not panic and must return None, got {decoded:?}"
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

    #[tokio::test]
    async fn read_body_bounded_stops_reading_once_max_size_is_exceeded() {
        let mock = MockServer::start().await;

        // ~keep A ~5 MB response with max_size(1024) proves the reader stops early —
        // simulates the "declared/actual size exceeds the cap" scenario without
        // needing a real decompression bomb for this specific unit test.
        let full_size = 5 * 1024 * 1024;
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'a'; full_size]))
            .mount(&mock)
            .await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");
        let url = format!("{}/big", mock.uri());
        let resp = client.get(&url).send().await.expect("request must succeed");

        let max_size = 1024usize;
        let (bytes, hit_cap) = read_body_bounded(resp, Some(max_size))
            .await
            .expect("bounded read must not error");

        assert!(hit_cap, "hit_cap must be true once the body exceeds max_size");
        assert!(
            bytes.len() < full_size / 100,
            "bounded read must stop far short of the full {full_size}-byte body, got {} bytes",
            bytes.len()
        );
        assert!(
            bytes.len() > max_size,
            "the chunk that crosses the cap should still be included, got {} bytes",
            bytes.len()
        );
    }

    /// Security regression: a gzip decompression bomb (~5 MB decompressed from a few
    /// hundred compressed bytes) behind a small `max_body_size` must not allocate
    /// anywhere near its full decompressed size. `read_body_bounded` reads
    /// `Response::chunk`-by-chunk (decompressed by reqwest's transparent gzip layer as
    /// it streams) and stops as soon as the cap is crossed, rather than buffering the
    /// entire decompressed body via `resp.bytes()` before any cap is applied.
    #[tokio::test]
    async fn read_body_bounded_caps_a_decompression_bomb() {
        let mock = MockServer::start().await;

        let decompressed_size = 5 * 1024 * 1024;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &vec![b'a'; decompressed_size]).expect("gzip write must succeed");
        let compressed = encoder.finish().expect("gzip finish must succeed");
        assert!(
            compressed.len() < 10_000,
            "test fixture must actually compress well (highly repetitive input), got {} bytes",
            compressed.len()
        );

        Mock::given(method("GET"))
            .and(path("/bomb"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-encoding", "gzip")
                    .set_body_raw(compressed, "application/octet-stream"),
            )
            .mount(&mock)
            .await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");
        let url = format!("{}/bomb", mock.uri());
        let resp = client.get(&url).send().await.expect("request must succeed");

        let max_size = 1024usize;
        let (bytes, hit_cap) = read_body_bounded(resp, Some(max_size))
            .await
            .expect("bounded read must not error");

        assert!(
            hit_cap,
            "hit_cap must be true once the decompressed body exceeds max_size"
        );
        assert!(
            bytes.len() < decompressed_size / 100,
            "bounded read must stop far short of the bomb's full decompressed size \
             ({decompressed_size} bytes), got {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn build_client_reuses_cached_client_for_matching_config() {
        // ~keep A distinct, unlikely-to-collide timeout so this test's cache entry
        // cannot already be populated by another test running in parallel.
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_273),
            ..CrawlConfig::default()
        };
        assert!(
            !client_cache_contains(&config),
            "precondition failed: another test already cached this exact config identity"
        );

        let _first = build_client(&config).expect("first build must succeed");
        assert!(
            client_cache_contains(&config),
            "build_client must populate the cache after building a client"
        );

        let _second = build_client(&config).expect("second build with the same config must succeed");
        assert!(
            client_cache_contains(&config),
            "the cache entry must still be present after a second build with a matching identity"
        );
    }

    #[test]
    fn build_client_uses_distinct_cache_entries_for_distinct_timeouts() {
        let config_a = CrawlConfig {
            request_timeout: Duration::from_millis(918_274),
            ..CrawlConfig::default()
        };
        let config_b = CrawlConfig {
            request_timeout: Duration::from_millis(918_275),
            ..CrawlConfig::default()
        };

        let _a = build_client(&config_a).expect("client a must build");
        assert!(
            client_cache_contains(&config_a),
            "config_a's identity must be cached after building it"
        );
        assert!(
            !client_cache_contains(&config_b),
            "building a client for config_a must not also cache config_b's distinct identity"
        );
    }
}
