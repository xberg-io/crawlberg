//! Network utilities: SSRF policy, validation, and security.

#[cfg(feature = "browser-native")]
pub(crate) mod browser_policy;
// ~keep `reqwest::cookie` (and thus PolicyCookieStore's `Jar` wrapper) only exists under
// reqwest's hyper backend; wasm32 uses the browser's own fetch/cookie handling instead.
#[cfg(not(target_arch = "wasm32"))]
pub mod cookie;
pub(crate) mod origin;
pub mod redact;
// ~keep `reqwest::dns::Resolve` only exists under reqwest's hyper backend; wasm32 has no
// DNS surface at all (see the wasm32 note on `ssrf::validate_url`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod resolver;
pub mod ssrf;

pub use redact::redact_url_credentials;
pub use ssrf::{HostMatcher, SsrfError, SsrfPolicy, validate_url};

/// Confirm `url` parses as an address with an `http` or `https` scheme.
///
/// A URL scheme is case-insensitive (RFC 3986 3.1), so this parses `url` and reads the
/// parsed scheme instead of testing the raw text for a lower-case `http://`/`https://`
/// prefix. The `url` crate lower-cases the scheme while parsing, so `HTTP://example.com/`
/// and `Https://example.com/` are accepted the same as `http://example.com/`.
pub(crate) fn has_http_scheme(url: &str) -> bool {
    url::Url::parse(url)
        .map(|parsed| matches!(parsed.scheme(), "http" | "https"))
        .unwrap_or(false)
}

#[cfg(test)]
mod scheme_tests {
    use super::has_http_scheme;

    #[test]
    fn accepts_lower_case_scheme() {
        assert!(has_http_scheme("http://example.com/"));
        assert!(has_http_scheme("https://example.com/"));
    }

    #[test]
    fn accepts_upper_case_scheme() {
        assert!(has_http_scheme("HTTP://example.com/"));
        assert!(has_http_scheme("HTTPS://example.com/"));
    }

    #[test]
    fn accepts_mixed_case_scheme() {
        assert!(has_http_scheme("HtTp://example.com/"));
        assert!(has_http_scheme("HtTpS://example.com/"));
    }

    #[test]
    fn rejects_non_http_scheme() {
        assert!(!has_http_scheme("ftp://example.com/"));
        assert!(!has_http_scheme("file:///etc/passwd"));
    }

    #[test]
    fn rejects_unparseable_url() {
        assert!(!has_http_scheme("not a url"));
        assert!(!has_http_scheme(""));
    }
}
