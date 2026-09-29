//! Proxy provider trait + baseline impl.
//!
//! Substrate-level extension point for per-host proxy rotation. The engine
//! calls [`ProxyProvider::next_proxy`] from `reqwest::Proxy::custom` per HTTP
//! request, so implementations can rotate by host, by counter, or by external
//! state. Returning `None` short-circuits to a direct connection.
//!
//! Crawlberg ships [`StaticProxyProvider`] — a fixed-pool round-robin
//! rotator. Cloud impls (e.g. `BrightDataProxyProvider`) plug in via
//! [`crate::CrawlEngineBuilder::with_proxy_provider`].
//!
//! Browser-backend proxies (`config.browser.proxy`) are still configured at
//! launch time via the static `ProxyConfig` value on `CrawlConfig`; the
//! provider only routes the reqwest HTTP path. Mixing both is supported.
//!
//! ```
//! use std::sync::Arc;
//! use crawlberg::{ProxyConfig, ProxyProvider, StaticProxyProvider};
//!
//! let pool = StaticProxyProvider::new(vec![
//!     ProxyConfig { url: "http://p1:8080".into(), username: None, password: None },
//!     ProxyConfig { url: "http://p2:8080".into(), username: None, password: None },
//! ]);
//! let _arc: Arc<dyn ProxyProvider> = Arc::new(pool);
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::CrawlError;
use crate::types::ProxyConfig;

/// Proxy URL schemes `reqwest` and the native browser worker can build a proxy
/// connection from.
pub(crate) const SUPPORTED_SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

/// Parses `raw` as a proxy URL, refusing to guess a scheme.
///
/// `url::Url::parse` treats any `word:` prefix as a scheme, even when `raw` has no real
/// scheme delimiter — for a scheme-less proxy string such as `user:pass@host:port`, that
/// reads the embedded username as the scheme. Requiring an explicit `scheme://` up front
/// means a credential can never be mistaken for a scheme, here or in any error built from
/// this function's result.
pub(crate) fn parse_proxy_url(raw: &str) -> Result<url::Url, CrawlError> {
    let has_scheme = raw.split_once(':').is_some_and(|(_, rest)| rest.starts_with("//"));
    if !has_scheme {
        return Err(CrawlError::invalid_config(
            "proxy URL is missing a scheme (expected http://, https://, socks5://, or socks5h://)",
        ));
    }
    url::Url::parse(raw).map_err(|e| CrawlError::invalid_config(format!("invalid proxy URL: {e}")))
}

/// Rejects `url` unless its scheme is one of [`SUPPORTED_SCHEMES`].
pub(crate) fn ensure_supported_scheme(url: &url::Url) -> Result<(), CrawlError> {
    let scheme = url.scheme();
    if !SUPPORTED_SCHEMES.contains(&scheme) {
        return Err(CrawlError::invalid_config(format!(
            "invalid proxy URL scheme '{scheme}' (expected http, https, socks5, or socks5h)"
        )));
    }
    Ok(())
}

/// Embeds `proxy`'s username/password into its URL as percent-encoded userinfo, for
/// backends that take a proxy connection as a single URL string rather than separate
/// credential fields (the native browser worker). Returns the base URL unchanged when no
/// credentials are configured.
///
/// Percent-encoding via `Url::set_username`/`set_password` (rather than a manual
/// `format!("{scheme}://{user}:{pass}@{rest}")` splice) means a `:`, `@`, or `/` in a
/// credential cannot corrupt the authority — e.g. terminate it early and smuggle a
/// different host in, or misdirect the connection to an unintended proxy.
///
/// Only the native browser backend (`native_browser.rs`, `interact/native.rs`) hands a
/// proxy connection to another process as a single URL string; every other caller passes
/// credentials separately (e.g. `reqwest::Proxy::basic_auth` in `http/client.rs`).
#[cfg(feature = "browser-native")]
pub(crate) fn proxy_url_with_credentials(proxy: &ProxyConfig) -> Result<String, CrawlError> {
    if proxy.username.is_none() && proxy.password.is_none() {
        return Ok(proxy.url.clone());
    }

    let mut parsed = parse_proxy_url(&proxy.url)?;
    parsed
        .set_username(proxy.username.as_deref().unwrap_or(""))
        .map_err(|()| credentials_unsupported_error(&parsed))?;
    parsed
        .set_password(proxy.password.as_deref())
        .map_err(|()| credentials_unsupported_error(&parsed))?;
    Ok(parsed.to_string())
}

/// `url` has already been through [`parse_proxy_url`], so its scheme is never a
/// misread credential — safe to name in an error.
#[cfg(feature = "browser-native")]
fn credentials_unsupported_error(url: &url::Url) -> CrawlError {
    CrawlError::invalid_config(format!(
        "proxy scheme '{}' does not support embedded credentials",
        url.scheme()
    ))
}

/// Resolves a [`ProxyConfig`] for an outbound HTTP request.
///
/// Implementations must be cheap (called per request from inside
/// `reqwest::Proxy::custom`) and thread-safe. Returning `None` routes the
/// request directly without a proxy.
pub trait ProxyProvider: std::fmt::Debug + Send + Sync + 'static {
    /// Pick a proxy for the given target host. `host` is the URL host string
    /// (no scheme, no port) — implementations may key on it for sticky
    /// per-host routing or ignore it for stateless rotation.
    fn next_proxy(&self, host: &str) -> Option<ProxyConfig>;
}

/// Round-robin pool of statically-configured proxies. Baseline impl shipped
/// with crawlberg.
///
/// Threadsafe; uses an [`AtomicUsize`] counter incremented per call. Empty
/// pools always return `None` (direct connection).
pub struct StaticProxyProvider {
    entries: Vec<ProxyConfig>,
    counter: AtomicUsize,
}

impl std::fmt::Debug for StaticProxyProvider {
    /// Redacted: [`ProxyConfig`]'s own derived `Debug` prints `username`/`password` and
    /// any userinfo embedded in `url` verbatim, and `ProxyProvider: std::fmt::Debug`
    /// means any consumer holding a trait object can trigger this via `{:?}` — including
    /// through `tracing`'s `?field` capture. Show only each entry's URL origin, as
    /// `ProxyConfig` does, plus whether credentials are configured, never the credentials
    /// themselves.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_urls: Vec<String> = self
            .entries
            .iter()
            .map(|entry| crate::net::redact::redact_url_to_origin(&entry.url))
            .collect();
        let has_credentials = self
            .entries
            .iter()
            .any(|entry| entry.username.is_some() || entry.password.is_some());
        f.debug_struct("StaticProxyProvider")
            .field("entries", &redacted_urls)
            .field("has_credentials", &has_credentials)
            .field("counter", &self.counter.load(Ordering::Relaxed))
            .finish()
    }
}

impl StaticProxyProvider {
    /// Build a provider with the given pool. Order is preserved; rotation
    /// is round-robin starting from index 0.
    pub fn new(entries: Vec<ProxyConfig>) -> Self {
        Self {
            entries,
            counter: AtomicUsize::new(0),
        }
    }

    /// Build an empty provider — always returns `None`. Useful as a default
    /// placeholder in substrate-only setups.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Number of proxies in the pool.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no proxies are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl ProxyProvider for StaticProxyProvider {
    fn next_proxy(&self, _host: &str) -> Option<ProxyConfig> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % self.entries.len();
        Some(self.entries[idx].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(url: &str) -> ProxyConfig {
        ProxyConfig {
            url: url.into(),
            username: None,
            password: None,
        }
    }

    #[test]
    fn empty_provider_returns_none() {
        let provider = StaticProxyProvider::empty();
        assert!(provider.next_proxy("example.com").is_none());
        assert!(provider.is_empty());
        assert_eq!(provider.len(), 0);
    }

    #[test]
    fn debug_format_never_exposes_proxy_credentials() {
        // ~keep `ProxyProvider: std::fmt::Debug` means any consumer holding a trait
        // object (`tracing::debug!(?provider, ...)`, a Debug-derived struct that embeds
        // one) can print this. The derived Debug would show `password: Some("hunter2")`
        // and the credential-bearing URL verbatim.
        let provider = StaticProxyProvider::new(vec![ProxyConfig {
            url: "http://svc-account:hunter2@proxy.internal:8080".into(),
            username: Some("svc-account".into()),
            password: Some("hunter2".into()),
        }]);
        let rendered = format!("{provider:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug output must not contain the raw password, got '{rendered}'"
        );
        assert!(
            !rendered.contains("svc-account:hunter2"),
            "Debug output must not contain raw proxy URL userinfo, got '{rendered}'"
        );
    }

    #[test]
    fn scheme_less_url_is_rejected_without_naming_the_embedded_username() {
        // ~keep `alice` sits where `url::Url::parse` would read a scheme from, so the
        // regression this guards is the parser mistaking it for one.
        let err = parse_proxy_url("alice:s3cr3t@proxy.test:8080")
            .expect_err("a proxy URL with no scheme delimiter must be rejected")
            .to_string();
        assert!(
            !err.contains("alice"),
            "error must not name the embedded username, got: {err}"
        );
        assert!(
            !err.contains("s3cr3t"),
            "error must not leak the embedded password, got: {err}"
        );
        assert!(
            err.contains("missing a scheme"),
            "error should explain the actual problem, got: {err}"
        );
    }

    #[test]
    fn bare_host_port_without_a_scheme_is_also_rejected() {
        let err = parse_proxy_url("proxy.test:8080").expect_err("a bare host:port must be rejected");
        assert!(err.to_string().contains("missing a scheme"));
    }

    #[test]
    fn issue_315_and_330_literal_repro_urls_never_echo_the_username() {
        // ~keep The literal repro strings from #315 and #330: `operator` and `KEY` sit where a
        // naive `url::Url::parse` would read the scheme from, and `KEY:@host:1` additionally
        // has an empty password (nothing between `:` and `@`).
        for raw in ["operator:s3cr3t@proxy:8080", "KEY:@host:1"] {
            let err = parse_proxy_url(raw)
                .expect_err(&format!("{raw} has no scheme delimiter and must be rejected"))
                .to_string();
            let lowered = err.to_lowercase();
            assert!(
                !lowered.contains("operator") && !lowered.contains("key"),
                "error for {raw} must not name the embedded username, got: {err}"
            );
            assert!(
                !lowered.contains("s3cr3t"),
                "error for {raw} must not leak the password, got: {err}"
            );
        }
    }

    #[test]
    fn a_url_with_an_explicit_scheme_parses_normally() {
        let url = parse_proxy_url("http://proxy.test:8080").expect("a well-formed URL must parse");
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("proxy.test"));
    }

    #[test]
    fn unsupported_but_named_scheme_is_reported_by_name() {
        let url = parse_proxy_url("ftp://proxy.internal:2121").expect("ftp:// has an explicit scheme");
        let err = ensure_supported_scheme(&url)
            .expect_err("ftp is not a supported proxy scheme")
            .to_string();
        assert!(err.contains("'ftp'"), "unexpected error: {err}");
    }

    #[test]
    #[cfg(feature = "browser-native")]
    fn password_with_special_characters_is_percent_encoded_and_round_trips() {
        let secret_password = "p@ss:w/ord #1 two";
        let proxy = ProxyConfig {
            url: "http://proxy.test:8080".into(),
            username: Some("alice".into()),
            password: Some(secret_password.into()),
        };

        let resolved = proxy_url_with_credentials(&proxy).expect("credentials must resolve");
        assert!(
            !resolved.contains(secret_password),
            "the raw password must not appear unencoded in the resolved URL, got '{resolved}'"
        );

        let reparsed = url::Url::parse(&resolved).expect("the resolved proxy URL must itself be valid");
        assert_eq!(
            reparsed.host_str(),
            Some("proxy.test"),
            "special characters in the password must not corrupt the host, got '{resolved}'"
        );
        assert_eq!(reparsed.port(), Some(8080));
        let decoded_password = percent_decode(reparsed.password().expect("password must be set"));
        assert_eq!(
            decoded_password, secret_password,
            "the password must decode back to its original value"
        );
    }

    #[test]
    #[cfg(feature = "browser-native")]
    fn socks5_credentials_are_embedded_via_userinfo_not_dropped() {
        let proxy = ProxyConfig {
            url: "socks5://proxy.test:1080".into(),
            username: Some("alice".into()),
            password: Some("s3cr3t".into()),
        };
        let resolved = proxy_url_with_credentials(&proxy).expect("socks5 with credentials must resolve");
        assert_eq!(resolved, "socks5://alice:s3cr3t@proxy.test:1080");
    }

    #[test]
    #[cfg(feature = "browser-native")]
    fn credential_free_proxy_is_passed_through_unchanged() {
        let proxy = ProxyConfig {
            url: "http://proxy.test:8080".into(),
            username: None,
            password: None,
        };
        assert_eq!(
            proxy_url_with_credentials(&proxy).expect("credential-free proxy must resolve"),
            "http://proxy.test:8080"
        );
    }

    #[test]
    #[cfg(feature = "browser-native")]
    fn scheme_less_base_url_with_credentials_is_rejected_without_naming_the_username() {
        let proxy = ProxyConfig {
            url: "alice:s3cr3t@proxy.test:8080".into(),
            username: Some("alice".into()),
            password: Some("s3cr3t".into()),
        };
        let err = proxy_url_with_credentials(&proxy)
            .expect_err("a scheme-less base URL must be rejected")
            .to_string();
        assert!(!err.contains("alice"), "error must not name the username, got: {err}");
        assert!(!err.contains("s3cr3t"), "error must not leak the password, got: {err}");
    }

    /// Minimal ASCII percent-decoder for test assertions only; production code never
    /// needs to decode a proxy password, only encode one via `Url::set_password`.
    fn percent_decode(input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 3 <= bytes.len()
                && let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap(), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).expect("test fixture is valid UTF-8")
    }

    #[test]
    fn percent_decode_round_trips_reserved_characters() {
        assert_eq!(percent_decode("p%40ss%3Aw%2Ford%20%231"), "p@ss:w/ord #1");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn round_robin_cycles_through_pool() {
        let provider = StaticProxyProvider::new(vec![
            proxy("http://p1:8080"),
            proxy("http://p2:8080"),
            proxy("http://p3:8080"),
        ]);
        let urls: Vec<_> = (0..6)
            .map(|_| provider.next_proxy("example.com").unwrap().url)
            .collect();
        assert_eq!(
            urls,
            vec![
                "http://p1:8080",
                "http://p2:8080",
                "http://p3:8080",
                "http://p1:8080",
                "http://p2:8080",
                "http://p3:8080",
            ]
        );
    }
}
