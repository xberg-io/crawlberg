//! Credential scope: which host receives the caller's credentials, and which header carries them.
//!
//! The engine fixes the scope once, when it admits the seed URL, and every fetch asks
//! [`seed_host_headers`] for the headers to send. Only a request to the seed's host gets any.

use base64::Engine as _;
use url::Url;

use super::origin::same_host;
use crate::types::{AuthConfig, CrawlConfig};

/// The seed host that credentials are scoped to, and the credentials the seed URL carried.
///
/// Built only by the engine when it admits a seed URL. Its fields are private, so a caller
/// can only leave the carrier on [`CrawlConfig`] empty.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialScope {
    seed: Url,
    basic: Option<(String, String)>,
}

impl std::fmt::Debug for CredentialScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialScope")
            .field("host", &self.host())
            .field("basic", &self.basic.as_ref().map(|_| "***"))
            .finish()
    }
}

impl CredentialScope {
    /// Scope credentials to `seed`'s host, with the `(username, password)` its URL carried.
    ///
    /// Returns `None` for a URL without a host, which never receives credentials.
    pub(crate) fn for_seed(seed: &Url, basic: Option<(String, String)>) -> Option<Self> {
        seed.host_str()?;
        Some(Self {
            seed: seed.clone(),
            basic,
        })
    }

    /// Whether the seed URL carried userinfo.
    pub(crate) fn has_url_credentials(&self) -> bool {
        self.basic.is_some()
    }

    /// Whether `url` is on the scoped host. Scheme and port are not compared.
    pub(crate) fn covers(&self, url: &Url) -> bool {
        same_host(&self.seed, url)
    }

    /// The scoped host.
    pub(crate) fn host(&self) -> &str {
        self.seed.host_str().unwrap_or_default()
    }
}

/// The credential header (name, value) to send with a request to `url`, if any.
///
/// Returns `None` unless the engine admitted a seed and `url` is on the seed's host. The
/// seed URL's own userinfo wins; otherwise the configured [`AuthConfig`] applies.
pub(crate) fn credential_header(config: &CrawlConfig, url: &Url) -> Option<(String, String)> {
    let scope = config.credential_scope.as_ref()?;
    if !scope.covers(url) {
        if scope.has_url_credentials() || config.auth.is_some() {
            tracing::debug!(
                origin = scope.host(),
                target = url.host_str().unwrap_or(""),
                "withholding credentials from a request to another host"
            );
        }
        return None;
    }
    if let Some((username, password)) = &scope.basic {
        return Some(basic_header(username, password));
    }
    match config.auth.as_ref()? {
        AuthConfig::Basic { username, password } => Some(basic_header(username, password)),
        AuthConfig::Bearer { token } => Some(("Authorization".to_owned(), format!("Bearer {token}"))),
        AuthConfig::Header { name, value } => Some((name.clone(), value.clone())),
    }
}

/// The headers a request for `url` carries: the custom headers, then the credential
/// header, which replaces a custom header of the same name. Empty unless `url` is on the
/// seed's host.
///
/// ~keep A blank `user-agent` entry is dropped here, not just at the judging side
/// (`crate::helpers::custom_user_agent_header`): every consumer of this function (the HTTP
/// tier's `apply_headers`, the chromiumoxide SSRF interceptor, and both native-browser
/// `origin_headers` builders) would otherwise put an empty `User-Agent` on the wire for a
/// caller who unset the header by emptying its value instead of removing the key
/// (crawlberg#423).
pub(crate) fn seed_host_headers(config: &CrawlConfig, url: &Url) -> Vec<(String, String)> {
    if !config.credential_scope.as_ref().is_some_and(|scope| scope.covers(url)) {
        return Vec::new();
    }
    let credential = credential_header(config, url);
    let mut headers: Vec<(String, String)> = config
        .custom_headers
        .iter()
        .filter(|(name, value)| !crate::helpers::is_blank_user_agent_override(name, value))
        .filter(|(name, _)| {
            credential
                .as_ref()
                .is_none_or(|(own, _)| !own.eq_ignore_ascii_case(name))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    headers.extend(credential);
    headers
}

/// The seed-host headers of one crawl, for a holder that must not keep the crawl's config.
///
/// ~keep The check of a pooled browser keeps one per watched page in its listener, which the
/// ~keep browser pool owns. A config there carries the pool itself, so the pool then owned a
/// ~keep reference to itself and never dropped (xberg-io/crawlberg#594).
#[cfg(feature = "browser-chromiumoxide")]
pub(crate) struct SeedHostHeaders {
    scope: CredentialScope,
    headers: Vec<(String, String)>,
}

#[cfg(feature = "browser-chromiumoxide")]
impl SeedHostHeaders {
    /// The headers `config` gives a request to its seed's host, or `None` when it gives none.
    pub(crate) fn of(config: &CrawlConfig) -> Option<Self> {
        let scope = config.credential_scope.clone()?;
        let headers = seed_host_headers(config, &scope.seed);
        (!headers.is_empty()).then_some(Self { scope, headers })
    }

    /// The headers a request for `url` carries: empty unless `url` is on the seed's host.
    pub(crate) fn for_url(&self, url: &Url) -> &[(String, String)] {
        if self.scope.covers(url) { &self.headers } else { &[] }
    }
}

/// The seed-host headers for the native browser, scoped to the seed's host.
///
/// ~keep The native clients add them per request after checking the host, the same rule as
/// ~keep `seed_host_headers`; `extra_headers` would send them to every host the page loads from.
#[cfg(feature = "browser-native")]
pub(crate) fn origin_headers(config: &CrawlConfig) -> Option<crawlberg_browser::adapter::OriginHeaders> {
    let scope = config.credential_scope.as_ref()?;
    let headers = seed_host_headers(config, &scope.seed);
    (!headers.is_empty()).then(|| crawlberg_browser::adapter::OriginHeaders {
        host: scope.host().to_owned(),
        headers,
    })
}

/// Whether a request to `url` carries credentials, which keeps it out of shared caches.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn is_credentialed(config: &CrawlConfig, url: &Url) -> bool {
    config
        .credential_scope
        .as_ref()
        .is_some_and(|scope| scope.covers(url) && (scope.has_url_credentials() || config.auth.is_some()))
}

fn basic_header(username: &str, password: &str) -> (String, String) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    ("Authorization".to_owned(), format!("Basic {encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test URL must parse")
    }

    fn config_with(scope: Option<CredentialScope>, auth: Option<AuthConfig>) -> CrawlConfig {
        CrawlConfig {
            credential_scope: scope,
            auth,
            ..CrawlConfig::default()
        }
    }

    fn url_scope() -> Option<CredentialScope> {
        CredentialScope::for_seed(&url("http://example.com/"), Some(("user".to_owned(), "pw".to_owned())))
    }

    #[test]
    fn url_credentials_go_to_the_seed_host_as_basic() {
        let config = config_with(url_scope(), None);
        let header = credential_header(&config, &url("https://EXAMPLE.com:8443/page"));
        assert_eq!(
            header,
            Some(("Authorization".to_owned(), "Basic dXNlcjpwdw==".to_owned()))
        );
    }

    #[test]
    fn url_credentials_never_go_to_another_host() {
        let config = config_with(url_scope(), None);
        for other in [
            "http://other.test/",
            "http://sub.example.com/",
            "http://example.com.evil.test/",
        ] {
            assert_eq!(
                credential_header(&config, &url(other)),
                None,
                "{other} must get nothing"
            );
        }
    }

    #[test]
    fn configured_auth_is_scoped_to_the_seed_host() {
        let scope = CredentialScope::for_seed(&url("http://example.com/"), None);
        let config = config_with(
            scope,
            Some(AuthConfig::Bearer {
                token: "tok".to_owned(),
            }),
        );
        assert_eq!(
            credential_header(&config, &url("http://example.com/a")),
            Some(("Authorization".to_owned(), "Bearer tok".to_owned()))
        );
        assert_eq!(credential_header(&config, &url("http://cdn.other.test/a.js")), None);
    }

    #[test]
    fn configured_header_auth_keeps_its_name() {
        let scope = CredentialScope::for_seed(&url("http://example.com/"), None);
        let config = config_with(
            scope,
            Some(AuthConfig::Header {
                name: "X-Api-Key".to_owned(),
                value: "k".to_owned(),
            }),
        );
        assert_eq!(
            credential_header(&config, &url("http://example.com/a")),
            Some(("X-Api-Key".to_owned(), "k".to_owned()))
        );
    }

    #[test]
    fn nothing_is_sent_without_an_admitted_seed() {
        let config = config_with(
            None,
            Some(AuthConfig::Bearer {
                token: "tok".to_owned(),
            }),
        );
        assert_eq!(credential_header(&config, &url("http://example.com/a")), None);
        assert!(!is_credentialed(&config, &url("http://example.com/a")));
    }

    #[test]
    fn a_request_is_credentialed_only_on_the_seed_host_with_credentials() {
        let config = config_with(url_scope(), None);
        assert!(is_credentialed(&config, &url("http://example.com/a")));
        assert!(!is_credentialed(&config, &url("http://other.test/a")));
        let bare = config_with(CredentialScope::for_seed(&url("http://example.com/"), None), None);
        assert!(!is_credentialed(&bare, &url("http://example.com/a")));
    }

    #[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
    #[test]
    fn the_custom_headers_go_to_the_seed_host_only_and_the_credential_replaces_one_of_its_name() {
        let config = CrawlConfig {
            custom_headers: std::collections::HashMap::from([
                ("authorization".to_owned(), "custom".to_owned()),
                ("x-custom".to_owned(), "value".to_owned()),
            ]),
            ..config_with(url_scope(), None)
        };

        let mut seed_host = seed_host_headers(&config, &url("https://example.com:8443/a"));
        seed_host.sort();
        assert_eq!(
            seed_host,
            [
                ("Authorization".to_owned(), "Basic dXNlcjpwdw==".to_owned()),
                ("x-custom".to_owned(), "value".to_owned()),
            ]
        );
        assert!(seed_host_headers(&config, &url("http://other.test/a")).is_empty());
        let unadmitted = CrawlConfig {
            credential_scope: None,
            ..config
        };
        assert!(
            seed_host_headers(&unadmitted, &url("http://example.com/a")).is_empty(),
            "no scope means no headers"
        );
    }

    #[test]
    fn a_blank_user_agent_custom_header_is_dropped_not_sent_empty() {
        let config = CrawlConfig {
            custom_headers: std::collections::HashMap::from([
                ("user-agent".to_owned(), "   ".to_owned()),
                ("x-custom".to_owned(), "value".to_owned()),
            ]),
            ..config_with(url_scope(), None)
        };

        let seed_host = seed_host_headers(&config, &url("https://example.com:8443/a"));
        assert!(
            !seed_host
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("user-agent")),
            "a blank custom user-agent must be dropped, not sent as an empty header: {seed_host:?}"
        );
        assert!(
            seed_host.contains(&("x-custom".to_owned(), "value".to_owned())),
            "every other custom header must still go through unchanged"
        );
    }

    #[test]
    fn debug_output_hides_the_password() {
        let rendered = format!("{:?}", url_scope().expect("scope must build"));
        assert!(!rendered.contains("pw"), "{rendered}");
        assert!(rendered.contains("example.com"), "{rendered}");
    }
}
