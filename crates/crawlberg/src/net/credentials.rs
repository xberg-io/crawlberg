//! Credential scope: which host receives the caller's credentials, and which header carries them.
//!
//! The engine fixes the scope once, when it admits the seed URL, and every fetch asks
//! [`credential_header`] for the header to send. Only a request to the seed's host gets one.

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

/// The credential header for the native browser, scoped to the seed's host.
///
/// ~keep The native clients add it per request after checking the host, the same rule as
/// ~keep `credential_header`; `extra_headers` would send it to every host the page loads from.
#[cfg(feature = "browser-native")]
pub(crate) fn origin_credential(config: &CrawlConfig) -> Option<crawlberg_browser::adapter::OriginCredential> {
    let scope = config.credential_scope.as_ref()?;
    let (name, value) = credential_header(config, &scope.seed)?;
    Some(crawlberg_browser::adapter::OriginCredential {
        host: scope.host().to_owned(),
        name,
        value,
    })
}

/// Whether a request to `url` carries credentials, which keeps it out of shared caches.
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

    #[test]
    fn debug_output_hides_the_password() {
        let rendered = format!("{:?}", url_scope().expect("scope must build"));
        assert!(!rendered.contains("pw"), "{rendered}");
        assert!(rendered.contains("example.com"), "{rendered}");
    }
}
