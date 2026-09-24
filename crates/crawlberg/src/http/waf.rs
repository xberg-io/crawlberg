//! WAF classification: the shared classifier and the vendor lookups the fetch path uses.

use std::collections::HashMap;
use std::sync::LazyLock;

use super::HttpResponse;
use crate::types::WafClassifier;
use crate::waf::TomlClassifier;

/// Process-wide WAF classifier built once from the embedded fingerprint corpus.
///
/// ~keep `TomlClassifier::builtin()` re-parses `waf_fingerprints.toml` (via
/// `include_str!`) and rebuilds the Aho-Corasick matcher set on every call — it was
/// previously constructed fresh per response on the `http_fetch` hot path (robots.txt,
/// every asset download, every sitemap fetch, and every page fetch), so this cache
/// turns a per-response parse+compile into a one-time process-wide cost. `classify`
/// only needs `&self`, so a shared immutable instance is safe across concurrent fetches.
static WAF_CLASSIFIER: LazyLock<TomlClassifier> = LazyLock::new(TomlClassifier::builtin);

/// Build a partial [`HttpResponse`] from a pre-built header map + body string.
///
/// Used in the early-exit detection paths where we need to pass a response
/// to [`crate::types::WafClassifier::classify`] before the full
/// [`HttpResponse`] struct is assembled.
fn build_partial_response(status: u16, body: &str, headers_map: &HashMap<String, Vec<String>>) -> HttpResponse {
    let body_bytes = body.as_bytes().to_vec();
    build_partial_response_with_bytes(status, &body_bytes, body, headers_map)
}

/// Build a partial [`HttpResponse`] with a pre-computed byte vec and a pre-built header map.
///
/// ~keep Takes an already-built `headers_map` (rather than a `reqwest::HeaderMap` it
/// rebuilds internally) so callers checking WAF signals at multiple points for the same
/// response — `http_fetch`'s header-only and body checks — can build the map once and
/// share it instead of re-walking `HeaderMap` and re-lowercasing every header name per check.
fn build_partial_response_with_bytes(
    status: u16,
    body_bytes: &[u8],
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> HttpResponse {
    HttpResponse {
        status,
        content_type: String::new(),
        body: body.to_string(),
        body_bytes: body_bytes.to_vec(),
        headers: headers_map.clone(),
        browser_extras: None,
        final_url: String::new(),
        screenshot: None,
    }
}

/// The WAF vendor `classify` reports for a response assembled from `body` and
/// `headers_map`, or `None` when nothing in it fingerprints.
pub(super) fn waf_vendor_from_body(
    status: u16,
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> Option<String> {
    classify_vendor(&build_partial_response(status, body, headers_map))
}

/// As [`waf_vendor_from_body`], for a body whose raw bytes the caller already holds.
pub(super) fn waf_vendor_from_bytes(
    status: u16,
    body_bytes: &[u8],
    body: &str,
    headers_map: &HashMap<String, Vec<String>>,
) -> Option<String> {
    classify_vendor(&build_partial_response_with_bytes(
        status,
        body_bytes,
        body,
        headers_map,
    ))
}

fn classify_vendor(response: &HttpResponse) -> Option<String> {
    WAF_CLASSIFIER
        .classify(response)
        .ok()
        .flatten()
        .map(|signal| signal.vendor)
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
        screenshot: None,
    };
    WAF_CLASSIFIER
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
        screenshot: None,
    };
    WAF_CLASSIFIER.classify(&response).ok().flatten().is_some()
}
