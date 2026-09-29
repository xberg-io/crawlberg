//! Network utilities: SSRF policy, validation, and security.

#[cfg(feature = "browser-native")]
pub(crate) mod browser_policy;
// ~keep `reqwest::cookie` (and thus PolicyCookieStore's `Jar` wrapper) only exists under
// reqwest's hyper backend; wasm32 uses the browser's own fetch/cookie handling instead.
#[cfg(not(target_arch = "wasm32"))]
pub mod cookie;
pub(crate) mod credentials;
pub(crate) mod origin;
pub mod redact;
// ~keep `reqwest::dns::Resolve` only exists under reqwest's hyper backend; wasm32 has no
// DNS surface at all (see the wasm32 note on `ssrf::validate_url`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod resolver;
pub mod ssrf;
pub(crate) mod userinfo;

#[doc(hidden)]
pub use credentials::CredentialScope;
pub use redact::redact_url_credentials;
pub use ssrf::{HostMatcher, SsrfError, SsrfPolicy, validate_url};

/// How many refusals of one browser page or session are logged one by one. The rest are
/// counted, and one warning reports the count when the page or session ends.
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
pub(crate) const LOGGED_REFUSALS: usize = 5;

/// Parse `url` and confirm its scheme is one of `schemes`.
///
/// A URL scheme is case-insensitive (RFC 3986 §3.1); the `url` crate lower-cases the scheme
/// while parsing, so this reads the parsed scheme instead of testing the raw text for a
/// lower-case prefix. Every entry in `schemes` must already be lower case. The HTTP check and
/// the WebSocket check both go through this one parse, so a spelling either accepts is the
/// address the other treats the same way.
fn parse_url_with_scheme(url: &str, schemes: &[&str]) -> Option<url::Url> {
    url::Url::parse(url)
        .ok()
        .filter(|parsed| schemes.contains(&parsed.scheme()))
}

/// Confirm `url` parses as an address with an `http` or `https` scheme.
// ~keep Both callers (the REST handler, the MCP tool) live behind the `api`/`mcp` features;
// gate this the same way, or a default-feature build (neither on) sees no caller and denies
// it as dead code.
#[cfg(any(feature = "api", feature = "mcp"))]
pub(crate) fn has_http_scheme(url: &str) -> bool {
    parse_url_with_scheme(url, &["http", "https"]).is_some()
}

/// Parse `url` as a WebSocket address (`ws` or `wss` scheme), or `None` when it is not one.
///
/// The returned URL is in normalized form: a lower-case scheme, no surrounding spaces, and the
/// `//` before the host. The endpoint checks and the browser connect both use this one parse,
/// so a spelling a check accepts is the address the connect uses.
pub(crate) fn parse_websocket_url(url: &str) -> Option<url::Url> {
    parse_url_with_scheme(url, &["ws", "wss"])
}

/// True when `url` parses as a WebSocket address (`ws` or `wss` scheme).
pub fn is_websocket_scheme(url: &str) -> bool {
    parse_websocket_url(url).is_some()
}

#[cfg(all(test, any(feature = "api", feature = "mcp")))]
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
