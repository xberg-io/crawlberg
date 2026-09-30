//! Proxy provider trait + baseline impl.
//!
//! Substrate-level extension point for per-host proxy rotation. The engine
//! calls [`ProxyProvider::next_proxy`] once for each HTTP request, so
//! implementations can rotate by host, by counter, or by external
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
    let admitted = admit_proxy(proxy)?;
    let url = admitted.address().as_url();
    let scheme = url.scheme();
    if !CHROME_SCHEMES.contains(&scheme) {
        return Err(CrawlError::invalid_config(format!(
            "invalid proxy URL scheme '{scheme}': Chrome takes http, https, socks4 or socks5"
        )));
    }
    if admitted.credentials().is_some() {
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

/// A configured proxy after [`admit_proxy`]: an address that holds no user name or password,
/// and the credentials apart from it.
///
/// ~keep The credentials join the address only where a connection is made (a client's
/// ~keep `basic_auth`), so no proxy URL string that reaches a log or an error can hold them.
pub(crate) struct AdmittedProxy {
    address: ProxyUrl,
    credentials: Option<ProxyCredentials>,
}

/// A proxy user name and password, percent-decoded.
///
/// ~keep wasm32 has no proxy client, so there the value only records that credentials are set.
pub(crate) struct ProxyCredentials {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) username: String,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) password: String,
}

impl AdmittedProxy {
    /// The address, with no user name or password.
    pub(crate) fn address(&self) -> &ProxyUrl {
        &self.address
    }

    pub(crate) fn credentials(&self) -> Option<&ProxyCredentials> {
        self.credentials.as_ref()
    }

    /// Consumes the proxy for a backend that takes the parts.
    #[cfg(feature = "browser-native")]
    pub(crate) fn into_parts(self) -> (url::Url, Option<ProxyCredentials>) {
        (self.address.0, self.credentials)
    }

    /// The reqwest proxy, with the credentials sent as `Proxy-Authorization`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reqwest_proxy(&self) -> Result<reqwest::Proxy, CrawlError> {
        let proxy = reqwest::Proxy::all(self.address.as_url().as_str()).map_err(|_| {
            CrawlError::invalid_config("invalid proxy URL: expected an address such as http://proxy:8080")
        })?;
        Ok(match &self.credentials {
            Some(credentials) => proxy.basic_auth(&credentials.username, &credentials.password),
            None => proxy,
        })
    }
}

/// Shows the scheme, host and port, and whether credentials are set.
impl std::fmt::Debug for AdmittedProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmittedProxy")
            .field("address", &self.address)
            .field("credentials", &self.credentials.is_some())
            .finish()
    }
}

/// Reads `proxy` once: its address with [`parse_proxy_url`], and its credentials from the
/// URL userinfo or from the `username`/`password` fields, never both.
///
/// ~keep A raw `@` after the authority means a `#`, `/` or `?` in a credential ended the
/// ~keep authority early (#285): the parser then reads the user name as the host and the start
/// ~keep of the password as the port, or keeps the rest of the password in the path or the
/// ~keep fragment. It is refused. Without a userinfo, the same text can also be an `@` in a
/// ~keep path, query or fragment, so that error names both.
pub(crate) fn admit_proxy(proxy: &ProxyConfig) -> Result<AdmittedProxy, CrawlError> {
    let ProxyUrl(mut url) = parse_proxy_url(&proxy.url)?;
    let in_url = !url.username().is_empty() || url.password().is_some();
    if after_authority(&proxy.url).contains('@') {
        return Err(CrawlError::invalid_config(if in_url {
            "invalid proxy URL: a user name or password in it holds a character that ends the address \
             (such as #, / or ?); percent-encode it, or set it in username and password"
        } else {
            "invalid proxy URL: it holds an @ after the host, and a proxy address takes no path, query \
             or fragment; if the @ is part of a password, percent-encode the password, or set it in \
             username and password"
        }));
    }
    let in_fields = proxy.username.is_some() || proxy.password.is_some();
    if in_url && in_fields {
        return Err(CrawlError::invalid_config(
            "the proxy URL holds a user name or password and username or password is also set; \
             set the credentials in one place",
        ));
    }
    let credentials = if in_url {
        // ~keep wasm32 keeps no credentials but refuses the same URLs as every other target.
        #[cfg(target_arch = "wasm32")]
        {
            percent_decoded(url.username())?;
            percent_decoded(url.password().unwrap_or(""))?;
        }
        let credentials = ProxyCredentials {
            #[cfg(not(target_arch = "wasm32"))]
            username: percent_decoded(url.username())?,
            #[cfg(not(target_arch = "wasm32"))]
            password: percent_decoded(url.password().unwrap_or(""))?,
        };
        // ~keep Cannot fail: the address has a host, so it can hold userinfo and lose it.
        let _ = url.set_username("");
        let _ = url.set_password(None);
        Some(credentials)
    } else {
        in_fields.then(|| ProxyCredentials {
            #[cfg(not(target_arch = "wasm32"))]
            username: proxy.username.clone().unwrap_or_default(),
            #[cfg(not(target_arch = "wasm32"))]
            password: proxy.password.clone().unwrap_or_default(),
        })
    };
    Ok(AdmittedProxy {
        address: ProxyUrl(url),
        credentials,
    })
}

/// The raw text from the first `/`, `?`, `#` or `\` after the scheme: the part that is not
/// the authority, so an `@` in it cannot end a userinfo.
fn after_authority(raw: &str) -> &str {
    let rest = raw.split_once("://").map_or(raw, |(_, rest)| rest);
    rest.find(['/', '?', '#', '\\']).map_or("", |start| &rest[start..])
}

fn percent_decoded(part: &str) -> Result<String, CrawlError> {
    percent_encoding::percent_decode_str(part)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| CrawlError::invalid_config("invalid proxy URL: a user name or password in it is not UTF-8"))
}

/// The scheme, host and port of `proxy` as [`admit_proxy`] reads it, or `***` when it refuses
/// it: the `Debug` text for a proxy that may never have been validated.
pub(crate) fn redacted_proxy_address(proxy: &ProxyConfig) -> String {
    match admit_proxy(proxy) {
        Ok(admitted) => {
            let url = admitted.address.as_url();
            format!(
                "{}://{}",
                url.scheme(),
                &url[url::Position::BeforeHost..url::Position::AfterPort]
            )
        }
        Err(_) => "***".to_owned(),
    }
}

/// Resolves a [`ProxyConfig`] for an outbound HTTP request.
///
/// Implementations must be cheap (called once for each request, redirect hops
/// included) and thread-safe. Returning `None` routes the
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
        let redacted_urls: Vec<String> = self.entries.iter().map(redacted_proxy_address).collect();
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
            let ours =
                parse_proxy_url(raw).and_then(|url| ensure_supported_scheme(&url).map(|()| url.as_url().clone()));
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

    fn admitted(proxy: &ProxyConfig) -> (url::Url, Option<(String, String)>) {
        let admitted = admit_proxy(proxy).expect("a usable proxy");
        let credentials = admitted
            .credentials()
            .map(|credentials| (credentials.username.clone(), credentials.password.clone()));
        (admitted.address().as_url().clone(), credentials)
    }

    #[test]
    fn credentials_in_the_fields_stay_out_of_the_address() {
        let secret_password = "p@ss:w/ord #1 two";
        let proxy = ProxyConfig {
            url: "http://proxy.test:8080".into(),
            username: Some("alice".into()),
            password: Some(secret_password.into()),
        };
        let (address, credentials) = admitted(&proxy);
        assert_eq!(address.as_str(), "http://proxy.test:8080/");
        assert_eq!(credentials, Some(("alice".to_owned(), secret_password.to_owned())));
    }

    #[test]
    fn credentials_in_the_url_move_out_of_the_address_decoded() {
        for (raw, address, user, password) in [
            (
                "http://alice:p%40ss%3Aw%2Ford%20%231@proxy.test:8080",
                "http://proxy.test:8080/",
                "alice",
                "p@ss:w/ord #1",
            ),
            ("operator:s3cr3t@proxy:8080", "http://proxy:8080/", "operator", "s3cr3t"),
            (
                "http://operator@proxy.test:8080",
                "http://proxy.test:8080/",
                "operator",
                "",
            ),
        ] {
            let (admitted_address, credentials) = admitted(&proxy(raw));
            assert_eq!(admitted_address.as_str(), address, "{raw}");
            assert_eq!(credentials, Some((user.to_owned(), password.to_owned())), "{raw}");
        }
    }

    #[test]
    fn credential_free_proxy_is_returned_as_parsed() {
        for (raw, expected) in [
            ("http://proxy.test:8080", "http://proxy.test:8080/"),
            ("127.0.0.1:3128", "http://127.0.0.1:3128/"),
        ] {
            assert_eq!(
                admitted(&proxy(raw)),
                (url::Url::parse(expected).expect("parses"), None)
            );
        }
    }

    #[test]
    fn scheme_less_base_url_takes_the_configured_credentials() {
        // ~keep #421: the address is read once, the way reqwest reads it, and the configured
        // ~keep credentials go with that address.
        let proxy = ProxyConfig {
            url: "127.0.0.1:3128".into(),
            username: Some("alice".into()),
            password: Some("s3cr3t".into()),
        };
        let (address, credentials) = admitted(&proxy);
        assert_eq!(address.as_str(), "http://127.0.0.1:3128/");
        assert_eq!(credentials, Some(("alice".to_owned(), "s3cr3t".to_owned())));
    }

    /// #285: an unencoded `#`, `/` or `?` ends the authority, so the url crate reads
    /// `operator` as the host and the start of the password as the port.
    const UNENCODED_PASSWORDS: [&str; 3] = [
        "http://operator:4242#IMPL385-285@proxy.test:8080",
        "http://operator:4242/IMPL385-285@proxy.test:8080",
        "http://operator:4242?IMPL385-285@proxy.test:8080",
    ];

    fn assert_hides_the_password(raw: &str, text: &str) {
        for part in ["4242", "IMPL385-285", "operator"] {
            assert!(!text.contains(part), "{raw}: '{part}' is shown: {text}");
        }
    }

    /// A config that validates `proxy` with no Chrome render, so only the proxy check can refuse it.
    fn crawl_through(proxy: ProxyConfig) -> crate::CrawlConfig {
        let mut config = crate::CrawlConfig {
            proxy: Some(proxy),
            ..crate::CrawlConfig::default()
        };
        config.browser.mode = crate::BrowserMode::Never;
        config
    }

    #[test]
    fn a_password_with_an_unencoded_hash_slash_or_question_mark_is_refused_without_showing_it() {
        for raw in UNENCODED_PASSWORDS {
            let err = crawl_through(proxy(raw))
                .validate()
                .expect_err("an unencoded password must be refused, not read as a host")
                .to_string();
            assert!(
                err.contains("percent-encode"),
                "{raw}: the error must name the fix: {err}"
            );
            assert_hides_the_password(raw, &err);
        }
        crawl_through(proxy("http://operator:4242%23IMPL385-285@proxy.test:8080"))
            .validate()
            .expect("positive twin: the percent-encoded password is accepted");
    }

    #[test]
    fn a_password_with_a_raw_at_sign_then_a_hash_is_refused_without_showing_its_tail() {
        for raw in [
            "http://op:pa@h#FIX385D-TAIL@proxy.test:8080",
            "http://op:pa@h/FIX385D-TAIL@proxy.test:8080",
            "http://op:pa@h?FIX385D-TAIL@proxy.test:8080",
            "op:pa@h#FIX385D-TAIL@proxy.test:8080",
            "http://op:4242\\RB385Z@proxy.test:8080",
        ] {
            let err = crawl_through(proxy(raw))
                .validate()
                .expect_err("an @ past the userinfo means the password ended the address early")
                .to_string();
            assert!(
                err.contains("percent-encode"),
                "{raw}: the error must name the fix: {err}"
            );
            let shown = format!(
                "{err} {:?} {:?} {}",
                proxy(raw),
                StaticProxyProvider::new(vec![proxy(raw)]),
                redacted_proxy_address(&proxy(raw))
            );
            for part in ["FIX385D-TAIL", "RB385Z", "pa@h"] {
                assert!(!shown.contains(part), "{raw}: '{part}' is shown: {shown}");
            }
        }
        crawl_through(proxy("http://op:pa%40h%23FIX385D-TAIL@proxy.test:8080"))
            .validate()
            .expect("positive twin: the percent-encoded password is accepted");
    }

    #[test]
    fn an_at_sign_in_the_path_query_or_fragment_of_a_proxy_is_refused_for_the_path_not_a_password() {
        for raw in [
            "http://proxy.test:8080/p@th",
            "http://proxy.test:8080/?q=a@b",
            "http://proxy.test:8080?q=a@b",
            "http://proxy.test:8080/#f@g",
        ] {
            let err = crawl_through(proxy(raw))
                .validate()
                .expect_err("an @ after the host is refused")
                .to_string();
            assert!(
                err.contains("a proxy address takes no path, query or fragment"),
                "{raw}: the error must name the path, query or fragment: {err}"
            );
            assert!(
                !err.contains("user name or password in it"),
                "{raw}: the address holds no credentials, so the error must not blame one: {err}"
            );
            for part in ["p@th", "a@b", "f@g"] {
                assert!(!err.contains(part), "{raw}: '{part}' is shown: {err}");
            }
        }
    }

    #[test]
    fn the_debug_of_a_proxy_with_an_unencoded_password_shows_no_part_of_it() {
        for raw in UNENCODED_PASSWORDS {
            assert_hides_the_password(raw, &format!("{:?}", proxy(raw)));
            assert_hides_the_password(raw, &format!("{:?}", StaticProxyProvider::new(vec![proxy(raw)])));
        }
        let shown = format!("{:?}", StaticProxyProvider::new(vec![proxy("http://proxy.test:8080")]));
        assert!(
            shown.contains("proxy.test:8080"),
            "positive twin: the address is shown: {shown}"
        );
        let shown = format!("{:?}", proxy("http://operator:IMPL385-DBG@proxy.test:8080"));
        assert!(
            shown.contains("proxy.test:8080"),
            "positive twin: the address is shown: {shown}"
        );
        assert!(!shown.contains("IMPL385-DBG"), "{shown}");
    }

    #[test]
    fn credentials_in_both_the_url_and_the_fields_are_a_config_error() {
        let both = ProxyConfig {
            url: "http://operator:IMPL385-BOTH-URL@proxy.test:8080".into(),
            username: Some("other".into()),
            password: Some("IMPL385-BOTH-FIELD".into()),
        };
        let err = crawl_through(both)
            .validate()
            .expect_err("two sources of proxy credentials must be refused")
            .to_string();
        assert!(err.contains("username"), "the error must name the fields: {err}");
        assert!(
            !err.contains("IMPL385-BOTH") && !err.contains("operator"),
            "the error shows a credential: {err}"
        );
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
