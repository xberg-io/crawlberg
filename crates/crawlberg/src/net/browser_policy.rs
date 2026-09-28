//! Bridges the real [`SsrfPolicy`] into the native browser backend.
//!
//! `crawlberg-browser` cannot name `SsrfPolicy` (it must not depend on this crate), so
//! it declares the [`SsrfValidator`] seam instead. This module is the implementation
//! that carries the configured policy — allowlist included — across that boundary, so
//! the browser layer enforces exactly what the HTTP layer does.

use std::sync::Arc;

use crawlberg_browser::adapter::SsrfValidator;
use url::Url;

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
    async fn an_empty_scheme_allowlist_denies_every_scheme() {
        let mut policy = SsrfPolicy::default();
        policy.scheme_allowlist.clear();

        validator_for(&policy)
            .validate(&"http://1.1.1.1/".parse::<Url>().expect("valid URL"))
            .await
            .expect_err("an empty scheme allowlist must reject every URL");
    }
}
