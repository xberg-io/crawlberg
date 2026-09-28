//! Bridges the real [`SsrfPolicy`] into the native browser backend.
//!
//! `crawlberg-browser` cannot name `SsrfPolicy` (it must not depend on this crate), so
//! it declares the [`SsrfValidator`] seam instead. This module is the implementation
//! that carries the configured policy — allowlist included — across that boundary, so
//! the browser layer enforces exactly what the HTTP layer does.

use std::net::IpAddr;
use std::sync::Arc;

use crawlberg_browser::adapter::SsrfValidator;
use url::Url;

use crate::net::resolver::resolve_permitted;
use crate::net::ssrf::{SsrfPolicy, validate_url};

/// [`SsrfValidator`] backed by the crawl's configured [`SsrfPolicy`].
#[derive(Debug)]
pub(crate) struct CoreSsrfValidator {
    policy: SsrfPolicy,
}

impl CoreSsrfValidator {
    fn new(policy: &SsrfPolicy) -> Self {
        Self { policy: policy.clone() }
    }
}

#[async_trait::async_trait]
impl SsrfValidator for CoreSsrfValidator {
    async fn validate(&self, url: &Url) -> Result<(), String> {
        validate_url(url, &self.policy).await.map_err(|e| e.to_string())
    }

    /// The HTTP client's connect-time resolution, so both paths decide a host the same way:
    /// a host on the name allowlist keeps its private addresses.
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        resolve_permitted(host, &self.policy)
            .await
            .map(|addresses| addresses.into_iter().map(|address| address.ip()).collect())
            .map_err(|e| e.to_string())
    }
}

/// Build the validator handed to `NativeBrowserConfig::ssrf`.
pub(crate) fn validator_for(policy: &SsrfPolicy) -> Arc<dyn SsrfValidator> {
    Arc::new(CoreSsrfValidator::new(policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::HostMatcher;

    #[test]
    fn browser_deny_list_matches_the_core_deny_list() {
        // ~keep The browser crate keeps its own copy for standalone use. If the two
        // drift, the fallback validator silently enforces a different policy.
        let browser: Vec<&str> = crawlberg_browser::adapter::DEFAULT_DENY_NET_CIDRS.to_vec();
        let core: Vec<&str> = crate::net::ssrf::DEFAULT_DENY_NET_CIDRS.to_vec();
        assert_eq!(
            core, browser,
            "crawlberg and crawlberg-browser default deny-lists have drifted"
        );
    }

    #[test]
    fn browser_named_schemes_match_the_core_named_schemes_apart_from_http_and_https() {
        // ~keep The two lists must stay in lockstep apart from http and https: core also
        // names those because a configured scheme_allowlist can refuse either, and the
        // browser layer never refuses them. Drift here means a scheme silently stops
        // being named on one side while the other still names it.
        let core: Vec<&str> = crate::net::ssrf::NAMED_SCHEMES
            .iter()
            .copied()
            .filter(|scheme| *scheme != "http" && *scheme != "https")
            .collect();
        let browser: Vec<&str> = crawlberg_browser::adapter::NAMED_SCHEMES.to_vec();
        assert_eq!(
            core, browser,
            "crawlberg and crawlberg-browser named-scheme lists have drifted (apart from http/https)"
        );
    }

    #[tokio::test]
    async fn default_policy_denies_loopback_through_the_bridge() {
        let validator = validator_for(&SsrfPolicy::default());
        let err = validator
            .validate(&"http://127.0.0.1/".parse::<Url>().expect("valid URL"))
            .await
            .expect_err("the default policy must deny loopback");

        assert!(
            err.contains("loopback"),
            "the core denial reason must survive the string conversion, got: {err}"
        );
    }

    #[tokio::test]
    async fn allowlisted_range_reaches_the_browser_layer() {
        // ~keep This is the whole point of the feature: an allowlist configured on
        // CrawlConfig must govern the browser backend, not just the HTTP path.
        let mut policy = SsrfPolicy::default();
        policy
            .allowlist
            .push(HostMatcher::cidr("127.0.0.0/8").expect("literal CIDR is valid"));

        validator_for(&policy)
            .validate(&"http://127.0.0.1/".parse::<Url>().expect("valid URL"))
            .await
            .expect("an allowlisted range must be permitted through the bridge");
    }

    #[tokio::test]
    async fn the_bridge_resolves_a_host_under_the_configured_policy() {
        let error = validator_for(&SsrfPolicy::default())
            .resolve("localhost")
            .await
            .expect_err("localhost resolves to loopback, which the default policy denies");
        assert_eq!(error, "denied by SSRF policy: loopback");

        // ~keep A host on the name allowlist keeps its private addresses, as on the HTTP path.
        // ~keep Checking each address as a literal instead refuses it: the name matches no IP.
        let mut policy = SsrfPolicy::default();
        policy.allowlist.push(HostMatcher::exact("localhost"));
        let addresses = validator_for(&policy)
            .resolve("localhost")
            .await
            .expect("an allowlisted host must resolve");
        assert!(
            !addresses.is_empty() && addresses.iter().all(IpAddr::is_loopback),
            "expected the loopback answers, got {addresses:?}"
        );
    }

    #[tokio::test]
    async fn an_empty_scheme_allowlist_denies_every_scheme() {
        let mut policy = SsrfPolicy::default();
        policy.scheme_allowlist.clear();

        validator_for(&policy)
            .validate(&"http://1.1.1.1/".parse::<Url>().expect("valid URL"))
            .await
            .expect_err("an empty scheme allowlist must reject every URL");
    }
}
