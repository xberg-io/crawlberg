//! Header-map conversion and the metadata extracted from response headers.

use std::borrow::Cow;
use std::collections::HashMap;

use reqwest::header::{HeaderMap, HeaderValue};

#[cfg(not(target_arch = "wasm32"))]
use crate::net::cookie::validate_cookie_domain;
#[cfg(not(target_arch = "wasm32"))]
use crate::types::CookieInfo;
use crate::types::ResponseMeta;

/// Extract the cookies a response for `url` stores from a `HashMap<String, Vec<String>>` of
/// its headers.
///
/// Looks for the `"set-cookie"` key and parses each value as an individual Set-Cookie header
/// with the RFC 6265 storage rules of [`cookie_store`], the parser behind reqwest's cookie jar.
/// A cookie the jar would not store is left out: one with no `name=value` pair, one that is
/// deleted (`Max-Age` of zero or less, or an `Expires` in the past), or one whose last
/// `Domain=` attribute does not domain-match `url`'s host (an IP host matches only its own
/// address). A `Domain=` that names a known public suffix fails [`validate_cookie_domain`] and
/// drops the cookie whole as well, matching [`crate::net::cookie::PolicyCookieStore`]'s policy
/// for the same headers on the plain-HTTP path. These cookies feed `browser::page_fetch`'s
/// `prior_cookies`, which are replayed into a page's CDP session, so an unvalidated `Domain=`
/// here would let one crawled origin plant a cookie for another.
///
/// `domain` is the `Domain` attribute without its leading dot, `None` for a host-only cookie;
/// `path` is the `Path` attribute when it starts with `/`, `None` otherwise.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn extract_cookies_from_hashmap(
    url: &url::Url,
    headers: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<CookieInfo> {
    let Some(values) = headers.get("set-cookie") else {
        return Vec::new();
    };
    values.iter().filter_map(|raw| parse_set_cookie(url, raw)).collect()
}

/// The crawl-result cookies of the response for `url`: [`extract_cookies_from_hashmap`] with
/// `domain` and `path` as [`CookieInfo`] documents them. An unparsable `url` stores nothing.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn result_cookies_from_hashmap(
    url: &str,
    headers: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<CookieInfo> {
    let Ok(url) = url::Url::parse(url) else {
        return Vec::new();
    };
    let host = url.host_str().unwrap_or_default();
    extract_cookies_from_hashmap(&url, headers)
        .into_iter()
        .map(|cookie| cookie.with_stored_scope(host, url.path()))
        .collect()
}

/// Parse one `Set-Cookie` header value of a response for `url` into a [`CookieInfo`].
///
/// Returns `None` for a cookie the jar would not store (see [`extract_cookies_from_hashmap`]):
/// the cookie is dropped whole rather than kept with an attribute stripped. ~keep
#[cfg(not(target_arch = "wasm32"))]
fn parse_set_cookie(url: &url::Url, raw: &str) -> Option<CookieInfo> {
    let cookie = match cookie_store::Cookie::parse(raw, url) {
        Ok(cookie) => cookie,
        Err(error) => {
            tracing::debug!(url = %url, error = %error, "dropping a Set-Cookie header the jar would not store");
            return None;
        }
    };
    if cookie.expires.is_expired() {
        tracing::debug!(url = %url, cookie.name = %cookie.name(), "dropping a deleted cookie");
        return None;
    }
    let domain = match &cookie.domain {
        cookie_store::CookieDomain::Suffix(domain) => {
            if let Err(error) = validate_cookie_domain(url.host_str().unwrap_or_default(), domain) {
                tracing::warn!(
                    url = %url,
                    cookie.name = %cookie.name(),
                    error = %error,
                    "dropping cookie with invalid Set-Cookie domain"
                );
                return None;
            }
            Some(domain.clone())
        }
        _ => None,
    };
    Some(CookieInfo {
        name: cookie.name().to_owned(),
        value: cookie.value().to_owned(),
        domain,
        path: cookie.path.is_from_path_attr().then(|| String::from(&cookie.path)),
    })
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

/// Decode an HTTP field value as browsers expose it.
///
/// ~keep HTTP permits `obs-text` bytes above ASCII. Browser header lists preserve those bytes by
/// ~keep isomorphic decoding (one byte to the same-valued Unicode scalar), rather than treating
/// ~keep them as UTF-8 and replacing or dropping them.
pub(crate) fn decode_header_value(value: &HeaderValue) -> Cow<'_, str> {
    value
        .to_str()
        .map(Cow::Borrowed)
        .unwrap_or_else(|_| Cow::Owned(value.as_bytes().iter().copied().map(char::from).collect()))
}

/// Build a `HashMap<String, Vec<String>>` of lowercase header names to browser-decoded values
/// from a `reqwest::HeaderMap`.
pub(crate) fn build_headers_map(headers: &HeaderMap) -> HashMap<String, Vec<String>> {
    let mut headers_map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in headers.iter() {
        headers_map
            .entry(name.as_str().to_lowercase())
            .or_default()
            .push(decode_header_value(value).into_owned());
    }
    headers_map
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use reqwest::header::{LOCATION, SET_COOKIE};

    #[test]
    fn response_headers_isomorphically_decode_non_ascii_bytes() {
        let mut headers = HeaderMap::new();
        headers.append(
            LOCATION,
            HeaderValue::from_bytes(b"/caf\xe9").expect("an obs-text Location must be valid"),
        );
        headers.append(
            "refresh",
            HeaderValue::from_bytes(b"0; url=/cr\xe8me").expect("an obs-text Refresh must be valid"),
        );
        headers.append(
            "x-opaque",
            HeaderValue::from_bytes(b"\x80\xc3\xa9\xff").expect("obs-text bytes must be valid"),
        );

        let decoded = build_headers_map(&headers);

        assert_eq!(decoded.get("location"), Some(&vec!["/caf\u{e9}".to_owned()]));
        assert_eq!(decoded.get("refresh"), Some(&vec!["0; url=/cr\u{e8}me".to_owned()]));
        assert_eq!(
            decoded["x-opaque"].first().map(|value| {
                value
                    .chars()
                    .map(|character| u8::try_from(u32::from(character)).expect("isomorphic scalar must fit in a byte"))
                    .collect::<Vec<_>>()
            }),
            Some(b"\x80\xc3\xa9\xff".to_vec()),
            "decoding must preserve each field byte rather than reinterpret it as UTF-8"
        );
    }

    #[test]
    fn decoded_sensitive_response_headers_stay_redacted_in_debug_output() {
        let mut headers = HeaderMap::new();
        let mut cookie = HeaderValue::from_bytes(b"session=secret-\xff").expect("an obs-text cookie must be valid");
        cookie.set_sensitive(true);
        headers.insert(SET_COOKIE, cookie);

        let decoded = build_headers_map(&headers);
        let rendered = format!("{:?}", crate::net::redact::RedactedHeaders(&decoded));

        assert!(!rendered.contains("secret"), "sensitive value leaked: {rendered}");
        assert!(rendered.contains(crate::net::redact::REDACTED_PLACEHOLDER));
    }

    /// Build a `set-cookie` headers `HashMap` with a single raw `Set-Cookie` value, as
    /// `extract_cookies_from_hashmap` expects to receive from the fetch layer.
    fn set_cookie_headers(raw: &str) -> std::collections::HashMap<String, Vec<String>> {
        std::collections::HashMap::from([("set-cookie".to_owned(), vec![raw.to_owned()])])
    }

    /// The URL of a response from `host` at `/`.
    fn from(host: &str) -> url::Url {
        url::Url::parse(&format!("http://{host}/")).expect("a host")
    }

    #[test]
    fn extract_cookies_drops_cross_origin_domain_spoof() {
        // ~keep The literal attack: a response from evil.example claims Domain=victim.example.
        let headers = set_cookie_headers("session=stolen; Domain=victim.example; Path=/");
        let cookies = extract_cookies_from_hashmap(&from("evil.example"), &headers);
        assert!(
            cookies.is_empty(),
            "a cookie whose Domain does not domain-match the response host must be dropped, got {cookies:?}"
        );
    }

    #[test]
    fn extract_cookies_drops_known_public_suffix_domain() {
        let headers = set_cookie_headers("session=stolen; Domain=herokuapp.com");
        let cookies = extract_cookies_from_hashmap(&from("evil-tenant.herokuapp.com"), &headers);
        assert!(
            cookies.is_empty(),
            "a cookie scoped to a public-suffix Domain must be dropped, got {cookies:?}"
        );
    }

    #[test]
    fn extract_cookies_keeps_host_only_cookie() {
        let headers = set_cookie_headers("session=abc123; Path=/");
        let cookies = extract_cookies_from_hashmap(&from("example.com"), &headers);
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
    fn result_cookies_have_the_domain_and_path_the_browser_stores() {
        let headers = std::collections::HashMap::from([(
            "set-cookie".to_owned(),
            [
                "a=1",
                "b=2; Domain=127.0.0.1",
                "c=3; Path=/X/",
                "d=4; Domain=.127.0.0.1",
                "e=5; Path=relative",
            ]
            .map(str::to_owned)
            .to_vec(),
        )]);
        let fields: Vec<_> = result_cookies_from_hashmap("http://127.0.0.1:8080/a/b/c", &headers)
            .into_iter()
            .map(|c| (c.name, c.domain.unwrap_or_default(), c.path.unwrap_or_default()))
            .collect();
        let expected = [
            ("a", "127.0.0.1", "/a/b"),
            ("b", "127.0.0.1", "/a/b"),
            ("c", "127.0.0.1", "/X/"),
            ("d", "127.0.0.1", "/a/b"),
            ("e", "127.0.0.1", "/a/b"),
        ]
        .map(|(n, d, p)| (n.to_owned(), d.to_owned(), p.to_owned()));
        assert_eq!(fields, expected);

        let headers = set_cookie_headers("s=1; domain=Example.COM");
        let cookies = result_cookies_from_hashmap("https://api.example.com/", &headers);
        assert_eq!(cookies[0].domain.as_deref(), Some(".example.com"));
        assert_eq!(cookies[0].path.as_deref(), Some("/"));

        for (raw, domain) in [
            ("s=1; Domain=.example.com", ".example.com"),
            ("s=1; Domain=", "api.example.com"),
        ] {
            let cookies = result_cookies_from_hashmap("https://api.example.com/", &set_cookie_headers(raw));
            assert_eq!(cookies[0].domain.as_deref(), Some(domain), "{raw}");
        }
    }

    /// ~keep RFC 6265 5.3: `Max-Age=0` or a past `Expires` deletes the cookie (step 11); an IP
    /// host domain-matches only its own address (5.1.3); the last `Domain` attribute wins (step 4).
    #[test]
    fn result_cookies_follow_the_rfc_6265_storage_rules() {
        let headers = std::collections::HashMap::from([(
            "set-cookie".to_owned(),
            [
                "plain=1",
                "gone=0; Max-Age=0",
                "expired=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
                "ipsuffix=15; Domain=0.0.1",
                "lastwins=16; Domain=other.test; Domain=127.0.0.1",
                "future=21; Max-Age=3600",
            ]
            .map(str::to_owned)
            .to_vec(),
        )]);
        let fields: Vec<_> = result_cookies_from_hashmap("http://127.0.0.1:8080/", &headers)
            .into_iter()
            .map(|c| (c.name, c.domain.unwrap_or_default(), c.path.unwrap_or_default()))
            .collect();
        let expected = [
            ("plain", "127.0.0.1", "/"),
            ("lastwins", "127.0.0.1", "/"),
            ("future", "127.0.0.1", "/"),
        ]
        .map(|(n, d, p)| (n.to_owned(), d.to_owned(), p.to_owned()));
        assert_eq!(fields, expected);
    }

    #[test]
    fn extract_cookies_keeps_legitimate_parent_domain_widening() {
        let headers = set_cookie_headers("session=abc123; Domain=example.com");
        let cookies = extract_cookies_from_hashmap(&from("api.example.com"), &headers);
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
        let cookies = extract_cookies_from_hashmap(&from("evil.example"), &headers);
        assert_eq!(
            cookies.len(),
            1,
            "only the spoofed cookie must be dropped, got {cookies:?}"
        );
        assert_eq!(cookies[0].name, "good");
    }
}
