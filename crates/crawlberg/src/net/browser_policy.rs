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
    #[serial_test::serial]
    async fn browser_fallback_validator_decides_embedded_ipv4_forms_like_the_core_policy() {
        // ~keep The fallback validator keeps its own copy of the embedded-IPv4 check; the CIDR
        // parity test above cannot see that copy drift.
        //
        // ~keep The reason is compared, not just the allow/deny bit, and that is what gives this
        // test teeth: drift can change which reason an address gets while leaving the decision
        // alone. Measured: swapping the fallback's `link_local` and `unique_local` table entries
        // leaves every row decided the same way, and this test reports `feaa::1` and both
        // `fe80::` ISATAP rows as `unique_local`. Candidate order is the other case: trying the
        // embedded address before `ip` reports `fe80::200:5efe:10.0.0.5` as `private_network`.
        //
        // ~keep Serial because the fallback reads CRAWLBERG_ALLOW_PRIVATE_NETWORK, which other
        // serial tests set. A non-serial test that sets it would still race this one; the env-var
        // read is the flaky part, not the serial marker.
        let fallback = crawlberg_browser::adapter::DefaultSsrfValidator::from_env();
        let mut mismatches = Vec::new();
        for &(literal, expected) in crate::net::ssrf::EMBEDDED_IPV4_CASES {
            let url = format!("http://[{literal}]/").parse::<Url>().expect("valid URL");
            let actual = fallback.validate(&url).await.err().map(|message| {
                // ~keep The fallback cannot name crawlberg's SsrfError, so it appends the reason
                // to its message. An IPv6 literal never contains ": ", so the last one is it.
                match message.rsplit_once(": ") {
                    Some((_, reason)) => reason.to_owned(),
                    None => message,
                }
            });
            if actual.as_deref() != expected {
                mismatches.push(format!("{literal}: core {expected:?}, fallback {actual:?}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "fallback validator drifted on {} of {} cases:\n{}",
            mismatches.len(),
            crate::net::ssrf::EMBEDDED_IPV4_CASES.len(),
            mismatches.join("\n")
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn browser_connect_time_resolution_checks_the_embedded_ipv4_address() {
        // ~keep An IP literal resolves to itself without a DNS query, so each case reaches the
        // check exactly as an AAAA answer carrying that address would. The bridge is what a crawl
        // uses; the fallback governs direct use of the browser crate and names no reason.
        let bridge = validator_for(&SsrfPolicy::default());
        let fallback = crawlberg_browser::adapter::DefaultSsrfValidator::from_env();
        let mut mismatches = Vec::new();
        for &(literal, expected) in crate::net::ssrf::EMBEDDED_IPV4_CASES {
            let actual = bridge.resolve(literal).await.err();
            let wanted = expected.map(|reason| format!("denied by SSRF policy: {reason}"));
            if actual != wanted {
                mismatches.push(format!("{literal}: bridge expected {wanted:?}, got {actual:?}"));
            }
            let refused = fallback.resolve(literal).await.is_err();
            if refused != expected.is_some() {
                mismatches.push(format!("{literal}: fallback refused={refused}, expected {expected:?}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "browser resolution decisions differ:\n{}",
            mismatches.join("\n")
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
