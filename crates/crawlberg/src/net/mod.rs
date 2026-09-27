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

/// True when `url` parses as a WebSocket address (`ws` or `wss` scheme).
///
/// A URL scheme is case-insensitive (RFC 3986 §3.1). This parses `url` and reads the
/// parsed scheme instead of testing the raw text for a lower-case `ws://`/`wss://`
/// prefix, so `WS://host` and `Wss://host` are accepted the same as `ws://host`: the
/// `url` crate lower-cases the scheme while parsing.
pub fn is_websocket_scheme(url: &str) -> bool {
    url::Url::parse(url)
        .map(|parsed| matches!(parsed.scheme(), "ws" | "wss"))
        .unwrap_or(false)
}

#[cfg(test)]
mod websocket_scheme_tests {
    use super::is_websocket_scheme;

    #[test]
    fn accepts_lower_case_scheme() {
        assert!(is_websocket_scheme("ws://example.com/"));
        assert!(is_websocket_scheme("wss://example.com/"));
    }

    #[test]
    fn accepts_upper_case_scheme() {
        assert!(is_websocket_scheme("WS://example.com/"));
        assert!(is_websocket_scheme("WSS://example.com/"));
    }

    #[test]
    fn accepts_mixed_case_scheme() {
        assert!(is_websocket_scheme("Ws://example.com/"));
        assert!(is_websocket_scheme("wSs://example.com/"));
    }

    #[test]
    fn rejects_non_websocket_scheme() {
        assert!(!is_websocket_scheme("http://example.com/"));
        assert!(!is_websocket_scheme("HTTP://example.com/"));
    }

    #[test]
    fn rejects_unparseable_url() {
        assert!(!is_websocket_scheme("not a url"));
        assert!(!is_websocket_scheme(""));
    }
}
