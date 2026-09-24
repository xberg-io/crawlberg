//! Header-map conversion and the metadata extracted from response headers.

use std::collections::HashMap;

use reqwest::header::HeaderMap;

#[cfg(not(target_arch = "wasm32"))]
use crate::net::cookie::validate_cookie_domain;
#[cfg(not(target_arch = "wasm32"))]
use crate::types::CookieInfo;
use crate::types::ResponseMeta;

/// Extract cookies from a `HashMap<String, Vec<String>>` of response headers.
///
/// Looks for the `"set-cookie"` key and parses each value as an individual
/// Set-Cookie header, preserving all cookies from the response.
///
/// `host` is the host that sent the response; a cookie whose `Domain=` attribute fails
/// [`validate_cookie_domain`] (cross-origin spoof or public-suffix) is dropped entirely
/// rather than accepted with the attribute stripped, matching [`crate::net::cookie::PolicyCookieStore`]'s
/// policy for the same headers on the plain-HTTP path. These cookies feed
/// `browser::page_fetch`'s `prior_cookies`, which are replayed into a page's CDP session,
/// so an unvalidated `Domain=` here would let one crawled origin plant a cookie for another.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn extract_cookies_from_hashmap(
    host: &str,
    headers: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<CookieInfo> {
    let Some(values) = headers.get("set-cookie") else {
        return Vec::new();
    };
    values.iter().filter_map(|raw| parse_set_cookie(host, raw)).collect()
}

/// Parse one `Set-Cookie` header value into a [`CookieInfo`].
///
/// Returns `None` when the value carries no `name=value` pair, or when its `Domain=`
/// attribute fails [`validate_cookie_domain`] — the cookie is dropped whole rather than
/// kept with the attribute stripped, matching the policy documented on
/// [`extract_cookies_from_hashmap`]. ~keep
#[cfg(not(target_arch = "wasm32"))]
fn parse_set_cookie(host: &str, raw: &str) -> Option<CookieInfo> {
    let parts: Vec<&str> = raw.split(';').collect();
    let (name, value) = parts.first()?.split_once('=')?;
    let mut cookie = CookieInfo {
        name: name.trim().to_owned(),
        value: value.trim().to_owned(),
        domain: None,
        path: None,
    };

    for attr in &parts[1..] {
        let attr = attr.trim().to_lowercase();
        if let Some(d) = attr.strip_prefix("domain=") {
            if let Err(e) = validate_cookie_domain(host, d) {
                tracing::warn!(
                    host = %host,
                    cookie.name = %cookie.name,
                    error = %e,
                    "dropping cookie with invalid Set-Cookie domain"
                );
                return None;
            }
            cookie.domain = Some(d.to_owned());
        } else if let Some(p) = attr.strip_prefix("path=") {
            cookie.path = Some(p.to_owned());
        }
    }

    Some(cookie)
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

/// Build a `HashMap<String, Vec<String>>` of lowercase header names to values from a
/// `reqwest::HeaderMap`, dropping values that aren't valid UTF-8 (mirrors the header
/// filtering `HttpResponse.headers` has always applied on this path).
pub(super) fn build_headers_map(headers: &HeaderMap) -> HashMap<String, Vec<String>> {
    let mut headers_map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            headers_map
                .entry(name.as_str().to_lowercase())
                .or_default()
                .push(v.to_string());
        }
    }
    headers_map
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// Build a `set-cookie` headers `HashMap` with a single raw `Set-Cookie` value, as
    /// `extract_cookies_from_hashmap` expects to receive from the fetch layer.
    fn set_cookie_headers(raw: &str) -> std::collections::HashMap<String, Vec<String>> {
        std::collections::HashMap::from([("set-cookie".to_owned(), vec![raw.to_owned()])])
    }

    #[test]
    fn extract_cookies_drops_cross_origin_domain_spoof() {
        // ~keep The literal attack: a response from evil.example claims Domain=victim.example.
        let headers = set_cookie_headers("session=stolen; Domain=victim.example; Path=/");
        let cookies = extract_cookies_from_hashmap("evil.example", &headers);
        assert!(
            cookies.is_empty(),
            "a cookie whose Domain does not domain-match the response host must be dropped, got {cookies:?}"
        );
    }

    #[test]
    fn extract_cookies_drops_known_public_suffix_domain() {
        let headers = set_cookie_headers("session=stolen; Domain=herokuapp.com");
        let cookies = extract_cookies_from_hashmap("evil-tenant.herokuapp.com", &headers);
        assert!(
            cookies.is_empty(),
            "a cookie scoped to a public-suffix Domain must be dropped, got {cookies:?}"
        );
    }

    #[test]
    fn extract_cookies_keeps_host_only_cookie() {
        let headers = set_cookie_headers("session=abc123; Path=/");
        let cookies = extract_cookies_from_hashmap("example.com", &headers);
        assert_eq!(
            cookies.len(),
            1,
            "a host-only cookie (no Domain attribute) must be kept"
        );
        assert_eq!(cookies[0].name, "session");
        assert_eq!(cookies[0].value, "abc123");
        assert_eq!(cookies[0].domain, None);
    }

    #[test]
    fn extract_cookies_keeps_legitimate_parent_domain_widening() {
        let headers = set_cookie_headers("session=abc123; Domain=example.com");
        let cookies = extract_cookies_from_hashmap("api.example.com", &headers);
        assert_eq!(
            cookies.len(),
            1,
            "a Domain that domain-matches its own registrable parent must be kept"
        );
        assert_eq!(cookies[0].domain, Some("example.com".to_owned()));
    }

    #[test]
    fn extract_cookies_drops_only_the_offending_cookie_in_a_mixed_batch() {
        let headers = std::collections::HashMap::from([(
            "set-cookie".to_owned(),
            vec!["good=1; Path=/".to_owned(), "bad=2; Domain=victim.example".to_owned()],
        )]);
        let cookies = extract_cookies_from_hashmap("evil.example", &headers);
        assert_eq!(
            cookies.len(),
            1,
            "only the spoofed cookie must be dropped, got {cookies:?}"
        );
        assert_eq!(cookies[0].name, "good");
    }
}
