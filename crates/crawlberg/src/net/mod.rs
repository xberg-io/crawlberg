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
///
/// ~keep `pub(crate)`, not `pub`: alef treats every `pub` item in this crate as part of
/// the FFI-bound surface it generates bindings for, with no way to mark one Rust-only.
/// `crawlberg-cli` needs the identical check but cannot see a `pub(crate)` item across
/// the crate boundary, so it keeps its own copy (`crawlberg-cli/src/cli.rs`) rather than
/// force this into the managed surface for a binding that never calls it.
pub(crate) fn is_websocket_scheme(url: &str) -> bool {
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

    #[test]
    fn rejects_host_less_scheme() {
        // ~keep `ws`/`wss` are WHATWG special schemes, so `Url::parse` itself refuses an
        // empty host (`EmptyHost`) rather than returning a URL with no host to check. Measured:
        // `ws:///path` is NOT one of these forms even though it looks host-less — a special
        // scheme's extra slash is ignored, so the text after it (`path`) parses as the host,
        // not as a path on an empty host (`ws:///path` == `ws://path/`).
        assert!(!is_websocket_scheme("ws://"));
        assert!(!is_websocket_scheme("ws:///"));
        assert!(!is_websocket_scheme("ws://@"));
        assert!(!is_websocket_scheme("ws://:1234"));
    }

    #[test]
    fn accepts_no_slash_and_whitespace_padded_forms() {
        // ~keep The old `starts_with("ws://")` prefix check rejected both of these; `Url::parse`
        // accepts them because `ws`/`wss` are special schemes (a missing `//` still parses an
        // authority, so `ws:host` == `ws://host/`) and because the parser trims leading/trailing
        // C0 control and space before it looks at the scheme at all. Both carry a real host, so
        // both are the same address as the slashed, untrimmed spelling and are accepted.
        assert!(is_websocket_scheme("ws:host"));
        assert!(is_websocket_scheme(" ws://host "));
    }
}
