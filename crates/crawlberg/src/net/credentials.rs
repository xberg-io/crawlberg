//! Credential scope: which host receives the caller's credentials, and which header carries them.
//!
//! The engine fixes the scope once, when it admits the seed URL, and every fetch asks
//! [`seed_host_headers`] for the headers to send. Only a request to the seed's host gets any
//! credential; admitted subdomains receive only explicitly allowlisted custom headers.

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

    /// Whether `url` is on a proper subdomain of the scoped host (`docs.example.com` for a seed
    /// on `example.com`), by the same suffix rule the link scope uses to admit it.
    pub(crate) fn covers_subdomain(&self, url: &Url) -> bool {
        let (Some(seed), Some(target)) = (self.seed.host_str(), url.host_str()) else {
            return false;
        };
        let (seed, target) = (seed.to_ascii_lowercase(), target.to_ascii_lowercase());
        target.len() > seed.len() + 1 && target.ends_with(&format!(".{seed}"))
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
/// seed's host or an admitted subdomain explicitly authorized for each custom header.
///
/// ~keep A blank `user-agent` entry is dropped here, not just at the judging side
/// (`crate::helpers::custom_user_agent_header`): every consumer of this function (the HTTP
/// tier's `apply_headers`, the chromiumoxide SSRF interceptor, and both native-browser
/// `origin_headers` builders) would otherwise put an empty `User-Agent` on the wire for a
/// caller who unset the header by emptying its value instead of removing the key
/// (crawlberg#423).
pub(crate) fn seed_host_headers(config: &CrawlConfig, url: &Url) -> Vec<(String, String)> {
    let Some(scope) = config.credential_scope.as_ref() else {
        return Vec::new();
    };
    if !scope.covers(url) {
        if config.allow_subdomains && scope.covers_subdomain(url) {
            return subdomain_headers(config);
        }
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

/// Whether an individual custom header is authorized for a request to `url`.
pub(crate) fn custom_header_is_scoped(config: &CrawlConfig, url: &Url, name: &str) -> bool {
    config.credential_scope.as_ref().is_some_and(|scope| {
        scope.covers(url) || (scope.covers_subdomain(url) && subdomain_header_is_allowed(config, name))
    })
}

fn subdomain_header_is_allowed(config: &CrawlConfig, name: &str) -> bool {
    config.allow_subdomains
        && config
            .subdomain_header_allowlist
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(name))
        && !super::redact::is_sensitive_header(name)
        && !matches!(&config.auth, Some(AuthConfig::Header { name: own, .. }) if own.eq_ignore_ascii_case(name))
}

/// Headers explicitly authorized for an admitted subdomain, excluding credential headers.
fn subdomain_headers(config: &CrawlConfig) -> Vec<(String, String)> {
    config
        .custom_headers
        .iter()
        .filter(|(name, value)| !crate::helpers::is_blank_user_agent_override(name, value))
        .filter(|(name, _)| subdomain_header_is_allowed(config, name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// Whether a browser request header may have been injected from the caller's configuration.
#[cfg(feature = "browser-chromiumoxide")]
pub(crate) fn is_configured_header(config: &CrawlConfig, name: &str) -> bool {
    config
        .custom_headers
        .iter()
        .any(|(own, value)| own.eq_ignore_ascii_case(name) && !crate::helpers::is_blank_user_agent_override(own, value))
        || match &config.auth {
            Some(AuthConfig::Header { name: own, .. }) => own.eq_ignore_ascii_case(name),
            Some(AuthConfig::Basic { .. } | AuthConfig::Bearer { .. }) => name.eq_ignore_ascii_case("authorization"),
            None => false,
        }
        || (name.eq_ignore_ascii_case("authorization")
            && config
                .credential_scope
                .as_ref()
                .is_some_and(CredentialScope::has_url_credentials))
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

/// Whether a request carries caller-specific headers, excluding it from URL-only caches.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn is_credentialed(config: &CrawlConfig, url: &Url) -> bool {
    config
        .credential_scope
        .as_ref()
        .is_some_and(|scope| scope.covers(url) && (scope.has_url_credentials() || config.auth.is_some()))
        || config.custom_headers.iter().any(|(name, value)| {
            !crate::helpers::is_blank_user_agent_override(name, value) && custom_header_is_scoped(config, url, name)
        })
}

/// Whether a redirect-capable request could send caller-specific headers on any hop.
///
/// ~keep A robots fetch can start on an uncredentialed host and redirect back to the seed.
/// ~keep Its URL-only cache must therefore consider the whole possible chain, not just its start.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn may_send_scoped_headers(config: &CrawlConfig) -> bool {
    config.auth.is_some()
        || config
            .credential_scope
            .as_ref()
            .is_some_and(CredentialScope::has_url_credentials)
        || config
            .custom_headers
            .iter()
            .any(|(name, value)| !crate::helpers::is_blank_user_agent_override(name, value))
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

    fn subdomain_config(allow_subdomains: bool, auth: Option<AuthConfig>) -> CrawlConfig {
        CrawlConfig {
            allow_subdomains,
            subdomain_header_allowlist: vec!["Accept".to_owned()],
            custom_headers: std::collections::HashMap::from([
                ("Accept".to_owned(), "text/html".to_owned()),
                ("Authorization".to_owned(), "custom".to_owned()),
                ("COOKIE".to_owned(), "sid=1".to_owned()),
                ("Proxy-Authorization".to_owned(), "Basic x".to_owned()),
                ("x-token".to_owned(), "tok".to_owned()),
            ]),
            ..config_with(CredentialScope::for_seed(&url("http://example.com/"), None), auth)
        }
    }

    #[test]
    fn a_subdomain_gets_only_explicitly_allowlisted_custom_headers() {
        let config = subdomain_config(
            true,
            Some(AuthConfig::Header {
                name: "X-Token".to_owned(),
                value: "tok".to_owned(),
            }),
        );
        let mut headers = seed_host_headers(&config, &url("https://Docs.Example.com/a"));
        headers.sort();
        assert_eq!(headers, [("Accept".to_owned(), "text/html".to_owned())]);
        assert_eq!(credential_header(&config, &url("http://docs.example.com/")), None);
        assert!(is_credentialed(&config, &url("http://docs.example.com/")));
    }

    #[test]
    fn unlisted_custom_headers_stay_on_the_seed_host_regardless_of_their_names() {
        let config = CrawlConfig {
            subdomain_header_allowlist: vec!["accept-language".to_owned()],
            custom_headers: std::collections::HashMap::from([
                ("Accept-Language".to_owned(), "en".to_owned()),
                ("X-Auth-Token".to_owned(), "a".to_owned()),
                ("Private-Token".to_owned(), "b".to_owned()),
                ("X-Goog-Api-Key".to_owned(), "c".to_owned()),
                ("X-CSRF-Token".to_owned(), "d".to_owned()),
                ("X-Session-Id".to_owned(), "e".to_owned()),
                ("X-Client-Secret".to_owned(), "f".to_owned()),
            ]),
            ..subdomain_config(true, None)
        };
        assert_eq!(
            seed_host_headers(&config, &url("http://docs.example.com/")),
            [("Accept-Language".to_owned(), "en".to_owned())]
        );
        assert_eq!(seed_host_headers(&config, &url("http://example.com/")).len(), 7);
    }

    #[test]
    fn a_subdomain_gets_no_custom_headers_without_explicit_opt_in() {
        let config = CrawlConfig {
            subdomain_header_allowlist: Vec::new(),
            custom_headers: [
                ("X-Access-Value".to_owned(), "arbitrary-secret".to_owned()),
                ("Accept".to_owned(), "text/html".to_owned()),
            ]
            .into(),
            ..subdomain_config(true, None)
        };
        assert!(seed_host_headers(&config, &url("http://docs.example.com/")).is_empty());
        assert_eq!(seed_host_headers(&config, &url("http://example.com/")).len(), 2);
    }

    #[test]
    fn explicit_opt_in_never_forwards_standard_credentials() {
        let mut config = subdomain_config(true, None);
        config.subdomain_header_allowlist = config.custom_headers.keys().cloned().collect();
        let mut headers = seed_host_headers(&config, &url("http://docs.example.com/"));
        headers.sort();
        assert_eq!(
            headers,
            [
                ("Accept".to_owned(), "text/html".to_owned()),
                ("x-token".to_owned(), "tok".to_owned()),
            ]
        );
    }

    #[test]
    fn the_header_named_by_header_auth_is_withheld_from_a_subdomain_even_without_a_credential_name() {
        let config = CrawlConfig {
            subdomain_header_allowlist: vec!["Accept".to_owned(), "X-Tenant".to_owned()],
            custom_headers: std::collections::HashMap::from([
                ("X-Tenant".to_owned(), "t".to_owned()),
                ("Accept".to_owned(), "text/html".to_owned()),
            ]),
            ..subdomain_config(
                true,
                Some(AuthConfig::Header {
                    name: "x-tenant".to_owned(),
                    value: "t".to_owned(),
                }),
            )
        };

        assert_eq!(
            seed_host_headers(&config, &url("http://docs.example.com/")),
            [("Accept".to_owned(), "text/html".to_owned())]
        );
    }

    #[test]
    fn a_subdomain_gets_no_headers_unless_subdomains_are_allowed() {
        let config = subdomain_config(false, None);
        assert!(seed_host_headers(&config, &url("http://docs.example.com/")).is_empty());
    }

    #[test]
    fn only_a_true_subdomain_of_the_seed_host_counts() {
        let config = subdomain_config(true, None);
        for other in [
            "http://other.test/",
            "http://example.com.evil.test/",
            "http://notexample.com/",
            "http://.example.com/",
            "http://example.org/",
        ] {
            let Ok(parsed) = Url::parse(other) else { continue };
            assert!(
                seed_host_headers(&config, &parsed).is_empty(),
                "{other} must get nothing"
            );
        }
        let seed_on_subdomain = CrawlConfig {
            credential_scope: CredentialScope::for_seed(&url("http://docs.example.com/"), None),
            ..subdomain_config(true, None)
        };
        assert!(seed_host_headers(&seed_on_subdomain, &url("http://example.com/")).is_empty());
        assert!(seed_host_headers(&seed_on_subdomain, &url("http://api.example.com/")).is_empty());
        assert!(!seed_host_headers(&seed_on_subdomain, &url("http://a.docs.example.com/")).is_empty());
    }
}
