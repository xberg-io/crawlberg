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
//! Browser-backend proxies (`config.browser.proxy`) still come from the static
//! `ProxyConfig` value on `CrawlConfig`; the provider only routes the reqwest
//! HTTP path. Mixing both is supported.
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

/// Proxy URL schemes the crawl config accepts: the only ones every HTTP client in the
/// workspace can use.
pub(crate) const SUPPORTED_SCHEMES: [&str; 2] = ["http", "https"];

/// A proxy URL read the way reqwest reads one. Only [`parse_proxy_url`] makes one, so a
/// scheme taken from it is never a user name that the url crate misread as a scheme.
pub(crate) struct ProxyUrl(url::Url);

impl ProxyUrl {
    pub(crate) fn as_url(&self) -> &url::Url {
        &self.0
    }

    #[cfg(feature = "browser-native")]
    pub(crate) fn into_url(self) -> url::Url {
        self.0
    }
}

/// Shows the scheme and host only: the URL can carry a password.
impl std::fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyUrl")
            .field("scheme", &self.0.scheme())
            .field("host", &self.0.host_str())
            .finish()
    }
}

/// Parses `raw` as a proxy URL, adding `http://` exactly where reqwest's proxy parser does.
///
/// ~keep reqwest retries `http://<raw>` when `raw` has no scheme (`127.0.0.1:3128`) and when
/// ~keep it parses without a host (`localhost:3128`, `user:pass@proxy:8080`, whose first word
/// ~keep the url crate reads as a scheme). Matching it keeps every address reqwest takes.
pub(crate) fn parse_proxy_url(raw: &str) -> Result<ProxyUrl, CrawlError> {
    let url = match url::Url::parse(raw) {
        Ok(url) if url.has_host() => url,
        Ok(_) | Err(url::ParseError::RelativeUrlWithoutBase) => url::Url::parse(&format!("http://{raw}"))
            .ok()
            .filter(url::Url::has_host)
            .ok_or_else(|| {
                CrawlError::invalid_config("invalid proxy URL: expected an address such as http://proxy:8080")
            })?,
        Err(e) => return Err(CrawlError::invalid_config(format!("invalid proxy URL: {e}"))),
    };
    Ok(ProxyUrl(url))
}

/// Rejects `url` unless its scheme is one of [`SUPPORTED_SCHEMES`].
pub(crate) fn ensure_supported_scheme(url: &ProxyUrl) -> Result<(), CrawlError> {
    let scheme = url.as_url().scheme();
    if SUPPORTED_SCHEMES.contains(&scheme) {
        return Ok(());
    }
    let reason = if scheme.starts_with("socks") {
        "SOCKS proxies are not supported; use http or https"
    } else {
        "expected http or https"
    };
    Err(CrawlError::invalid_config(format!(
        "invalid proxy URL scheme '{scheme}': {reason}"
    )))
}

/// Proxy URL schemes Chrome accepts in `--proxy-server`: the HTTP ones plus its own SOCKS ones.
const CHROME_SCHEMES: [&str; 4] = ["http", "https", "socks4", "socks5"];

/// The proxy a Chrome browser uses, read by [`chrome_proxy`]: `browser.proxy`, else the
/// crawl-wide `proxy`.
#[cfg(feature = "browser-chromiumoxide")]
pub(crate) fn chrome_proxy_for(config: &crate::types::CrawlConfig) -> Result<Option<ChromeProxy>, CrawlError> {
    config
        .browser
        .proxy
        .as_ref()
        .or(config.proxy.as_ref())
        .map(chrome_proxy)
        .transpose()
}

/// A proxy as Chrome takes it.
#[derive(Debug)]
#[cfg_attr(not(feature = "browser-chromiumoxide"), allow(dead_code))]
pub(crate) struct ChromeProxy {
    /// `scheme://host:port`, the value of `--proxy-server`.
    pub(crate) server: String,
}

/// Reads `proxy` for Chrome, with [`parse_proxy_url`], so Chrome gets the same address as
/// every HTTP client.
///
/// ~keep A proxy with credentials is refused: Chrome ignores credentials in `--proxy-server`,
/// ~keep and chromiumoxide answers every proxy authentication challenge itself before a caller
/// ~keep can (`handler/network.rs` `on_fetch_auth_required`), so no credentials can reach it.
pub(crate) fn chrome_proxy(proxy: &ProxyConfig) -> Result<ChromeProxy, CrawlError> {
    let parsed = parse_proxy_url(&proxy.url)?;
    let url = parsed.as_url();
    let scheme = url.scheme();
    if !CHROME_SCHEMES.contains(&scheme) {
        return Err(CrawlError::invalid_config(format!(
            "invalid proxy URL scheme '{scheme}': Chrome takes http, https, socks4 or socks5"
        )));
    }
    if has_credentials(proxy, &parsed) {
        return Err(CrawlError::invalid_config(
            "the Chrome backend cannot use a proxy with a username or password; \
             use a proxy that needs no credentials, or the native backend",
        ));
    }
    Ok(ChromeProxy {
        server: format!(
            "{scheme}://{}",
            &url[url::Position::BeforeHost..url::Position::AfterPort]
        ),
    })
}

/// Whether `proxy` carries a username or password, in its fields or in `url`, its address.
pub(crate) fn has_credentials(proxy: &ProxyConfig, url: &ProxyUrl) -> bool {
    let url = url.as_url();
    proxy.username.is_some() || proxy.password.is_some() || !url.username().is_empty() || url.password().is_some()
}

/// Embeds `proxy`'s username/password into its URL as percent-encoded userinfo, for
/// backends that take a proxy connection as a single URL string rather than separate
/// credential fields (the native browser worker). With no credentials configured, returns
/// the address as [`parse_proxy_url`] reads it.
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
    let mut parsed = parse_proxy_url(&proxy.url)?.into_url();
    if proxy.username.is_none() && proxy.password.is_none() {
        return Ok(parsed.to_string());
    }

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
    /// through `tracing`'s `?field` capture. Show only the redacted URL per entry plus
    /// whether credentials are configured, never the credentials themselves.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_urls: Vec<String> = self
            .entries
            .iter()
            .map(|entry| crate::net::redact_url_credentials(&entry.url))
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

    /// One proxy address of every input class, for the equivalence tests below.
    pub(crate) const EQUIVALENCE_INPUTS: [&str; 17] = [
        "http://proxy.test:8080",
        "https://u:p@proxy.test:8443",
        "127.0.0.1:3128",
        "[::1]:3128",
        "localhost:3128",
        "myproxy.corp:3128",
        "operator:s3cr3t@proxy:8080",
        "KEY:@host:1",
        "user@127.0.0.1:3128",
        "proxy.test",
        "http:proxy.test:8080",
        "gopher://proxy.test:70",
        "socks5://proxy.test:1080",
        "://operator:s3cr3t@proxy.test:8080",
        "operator:s3cr3t@proxy:99999",
        "mailto:someone",
        "",
    ];

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn proxy_addresses_are_read_exactly_as_reqwest_reads_them() {
        for raw in EQUIVALENCE_INPUTS {
            match (reqwest::Proxy::all(raw), parse_proxy_url(raw)) {
                (Ok(theirs), Ok(ours)) => {
                    let (theirs, ours) = (format!("{theirs:?}"), format!("{:?}", ours.as_url()));
                    assert!(
                        theirs.contains(&ours),
                        "{raw:?}: reqwest reads {theirs}, we read {ours}"
                    );
                }
                (Err(_), Err(_)) => {}
                (theirs, ours) => panic!(
                    "{raw:?}: reqwest accepts it: {}, we accept it: {}",
                    theirs.is_ok(),
                    ours.is_ok()
                ),
            }
        }
    }

    #[test]
    #[cfg(feature = "browser-native")]
    fn the_native_browser_reads_and_accepts_proxies_exactly_as_the_config_check_does() {
        assert_eq!(SUPPORTED_SCHEMES, crawlberg_browser::adapter::SUPPORTED_PROXY_SCHEMES);
        for raw in EQUIVALENCE_INPUTS {
            let ours = parse_proxy_url(raw).and_then(|url| ensure_supported_scheme(&url).map(|()| url.into_url()));
            let theirs = crawlberg_browser::adapter::check_proxy_url(raw);
            assert_eq!(
                ours.as_ref().ok().map(url::Url::as_str),
                theirs.as_ref().ok().map(url::Url::as_str),
                "{raw:?}: the config check and the native browser disagree"
            );
        }
    }

    #[test]
    fn a_scheme_less_proxy_is_read_as_http_with_its_credentials_in_the_userinfo() {
        // ~keep The literal repro strings from #315 and #330: `operator` and `KEY` sit where a
        // naive `url::Url::parse` reads a scheme from. They are user names, not schemes.
        for (raw, user, host) in [
            ("operator:s3cr3t@proxy:8080", "operator", "proxy"),
            ("KEY:@host:1", "KEY", "host"),
            ("localhost:3128", "", "localhost"),
            ("127.0.0.1:3128", "", "127.0.0.1"),
        ] {
            let url = parse_proxy_url(raw).expect("reqwest takes this address, so we must");
            let url = url.as_url();
            assert_eq!(
                (url.scheme(), url.username(), url.host_str()),
                ("http", user, Some(host)),
                "{raw}"
            );
        }
    }

    #[test]
    fn an_unusable_proxy_address_is_refused_without_showing_it() {
        for raw in ["://operator:s3cr3t@proxy.test:8080", "operator:s3cr3t@proxy:99999"] {
            let err = parse_proxy_url(raw)
                .expect_err("reqwest refuses this address")
                .to_string();
            assert!(
                !err.contains("operator") && !err.contains("s3cr3t"),
                "the error for {raw} shows the credential: {err}"
            );
        }
    }

    #[test]
    fn a_url_with_an_explicit_scheme_parses_normally() {
        let url = parse_proxy_url("http://proxy.test:8080").expect("a well-formed URL must parse");
        assert_eq!(url.as_url().scheme(), "http");
        assert_eq!(url.as_url().host_str(), Some("proxy.test"));
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
    fn socks_is_refused_with_a_reason() {
        for raw in ["socks5://proxy.test:1080", "socks5h://proxy.test:1080"] {
            let url = parse_proxy_url(raw).expect("a socks URL parses");
            let err = ensure_supported_scheme(&url)
                .expect_err("no client speaks SOCKS")
                .to_string();
            assert!(err.contains("SOCKS proxies are not supported"), "{raw}: {err}");
        }
    }

    #[test]
    fn chrome_gets_the_scheme_host_and_port() {
        for (raw, server) in [
            ("127.0.0.1:3128", "http://127.0.0.1:3128"),
            ("http:proxy.test:8080", "http://proxy.test:8080"),
            ("[::1]:3128", "http://[::1]:3128"),
            ("https://proxy.test", "https://proxy.test"),
            ("http://proxy.test:8080/path", "http://proxy.test:8080"),
            ("socks5://proxy.test:1080", "socks5://proxy.test:1080"),
            ("socks4://proxy.test:1080", "socks4://proxy.test:1080"),
        ] {
            let proxy = chrome_proxy(&proxy(raw)).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(proxy.server, server, "{raw}");
        }
    }

    #[test]
    fn chrome_refuses_a_proxy_with_credentials_without_showing_them() {
        let with_fields = ProxyConfig {
            url: "http://proxy.test:8080".into(),
            username: Some("operator".into()),
            password: Some("s3cr3t".into()),
        };
        let password_only = ProxyConfig {
            url: "http://proxy.test:8080".into(),
            username: None,
            password: Some("s3cr3t".into()),
        };
        for config in [
            proxy("operator:s3cr3t@proxy.test:8080"),
            proxy("http://operator:s3cr3t@proxy.test:8080"),
            proxy("http://operator@proxy.test:8080"),
            proxy("socks5://operator:s3cr3t@proxy.test:1080"),
            with_fields,
            password_only,
        ] {
            let err = chrome_proxy(&config)
                .expect_err("Chrome cannot use credentials")
                .to_string();
            assert!(err.contains("username or password"), "{err}");
            assert!(!err.contains("s3cr3t") && !err.contains("operator"), "{err}");
        }
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
    fn credential_free_proxy_is_returned_as_parsed() {
        for (raw, expected) in [
            ("http://proxy.test:8080", "http://proxy.test:8080/"),
            ("127.0.0.1:3128", "http://127.0.0.1:3128/"),
        ] {
            let proxy = ProxyConfig {
                url: raw.into(),
                username: None,
                password: None,
            };
            assert_eq!(
                proxy_url_with_credentials(&proxy).expect("credential-free proxy must resolve"),
                expected
            );
        }
    }

    #[test]
    #[cfg(feature = "browser-native")]
    fn scheme_less_base_url_takes_the_configured_credentials() {
        // ~keep #421: the address is read once, the way reqwest reads it, and the configured
        // ~keep credentials go into that address.
        let proxy = ProxyConfig {
            url: "127.0.0.1:3128".into(),
            username: Some("alice".into()),
            password: Some("s3cr3t".into()),
        };
        assert_eq!(
            proxy_url_with_credentials(&proxy).expect("a bare ip:port with credentials must resolve"),
            "http://alice:s3cr3t@127.0.0.1:3128/"
        );
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
