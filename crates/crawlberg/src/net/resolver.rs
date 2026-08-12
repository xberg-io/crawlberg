//! SSRF policy enforcement inside DNS resolution.
//!
//! [`validate_url`] resolves a hostname and checks every answer against the policy, but
//! the connection is opened later by hyper, which performs its **own** independent
//! lookup. The validated addresses are discarded, so an attacker controlling
//! authoritative DNS for a host with `TTL=0` can answer the validation lookup with a
//! public address and the connect lookup with `169.254.169.254` — a classic
//! time-of-check/time-of-use rebind that no amount of checking at validation time can
//! close.
//!
//! [`PolicyResolver`] closes it by moving the check into the resolution hyper actually
//! uses: the addresses it returns are the addresses that get connected to, so there is no
//! second lookup to disagree with the first.
//!
//! [`validate_url`]: crate::net::ssrf::validate_url

use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::net::ssrf::{SsrfError, SsrfPolicy, classify_private_ip, is_ip_permitted};

/// Port used for the resolution lookup.
///
/// ~keep `reqwest::dns::Resolve` receives a bare `Name` with no port, and hyper rewrites
/// port `0` in every returned address to the URL's port (or the scheme's default) before
/// connecting. Resolving against `0` is therefore both the required and the correct
/// input — the port plays no part in an A/AAAA lookup.
const RESOLUTION_PORT: u16 = 0;

/// The error type [`Resolving`] resolves to.
///
/// ~keep reqwest's own `BoxError` alias is `pub(crate)`, so the trait's error type has to
/// be spelled out here rather than imported.
type ResolveError = Box<dyn std::error::Error + Send + Sync>;

/// A [`reqwest::dns::Resolve`] that applies an [`SsrfPolicy`] to every address it returns.
///
/// Refuses the whole resolution — rather than filtering the offending addresses out — when
/// any answer violates the policy. A host answering with one permitted and one denied
/// address is the signature of a rebind attempt, not a routing detail to be worked around.
#[derive(Debug, Clone)]
pub(crate) struct PolicyResolver {
    policy: Arc<SsrfPolicy>,
}

impl PolicyResolver {
    /// Build a resolver enforcing `policy`.
    pub(crate) fn new(policy: SsrfPolicy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }
}

impl Resolve for PolicyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        Box::pin(async move {
            let host = name.as_str().to_owned();

            // ~keep Mirrors `validate_url`'s precedence exactly: a host matching an Exact or
            // Suffix entry is permitted *before* resolution, so its addresses are never
            // tested against the deny-list. Diverging here would reject at connect time a
            // host that validation had just approved.
            let host_allowlisted = policy.allowlist.iter().any(|matcher| matcher.matches_host(&host));

            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), RESOLUTION_PORT))
                .await
                .map_err(|e| SsrfError::DnsResolutionFailed(format!("{host}: {e}")))?
                .collect();

            if addresses.is_empty() {
                return Err(Box::new(SsrfError::DnsResolutionFailed(format!(
                    "no addresses resolved for {host}"
                ))) as ResolveError);
            }

            if !host_allowlisted {
                for address in &addresses {
                    let ip = address.ip();
                    if !is_ip_permitted(ip, &policy) {
                        let reason = classify_private_ip(ip);
                        tracing::warn!(
                            host = %host,
                            reason,
                            "refusing to connect: a resolved address violates the SSRF policy"
                        );
                        return Err(Box::new(SsrfError::DeniedByPolicy { reason }) as ResolveError);
                    }
                }
            }

            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::str::FromStr;

    use super::*;
    use crate::net::ssrf::HostMatcher;

    fn deny_private_policy() -> SsrfPolicy {
        SsrfPolicy {
            deny_private: true,
            ..Default::default()
        }
    }

    async fn resolve_host(resolver: &PolicyResolver, host: &str) -> Result<Vec<IpAddr>, String> {
        let name = Name::from_str(host).expect("test host must be a valid DNS name");
        resolver
            .resolve(name)
            .await
            .map(|addrs| addrs.map(|addr| addr.ip()).collect())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn refuses_a_host_resolving_into_denied_space() {
        let resolver = PolicyResolver::new(deny_private_policy());

        let error = resolve_host(&resolver, "localhost")
            .await
            .expect_err("localhost resolves to loopback and must be refused");

        assert_eq!(error, "denied by SSRF policy: loopback", "expected a loopback denial");
    }

    #[tokio::test]
    async fn permits_a_host_resolving_into_denied_space_when_deny_private_is_off() {
        let resolver = PolicyResolver::new(SsrfPolicy {
            deny_private: false,
            ..Default::default()
        });

        let addresses = resolve_host(&resolver, "localhost")
            .await
            .expect("deny_private=false must permit loopback");

        assert!(
            addresses.iter().all(std::net::IpAddr::is_loopback),
            "expected only loopback addresses, got {addresses:?}"
        );
    }

    #[tokio::test]
    async fn permits_a_denied_address_for_an_allowlisted_host() {
        let resolver = PolicyResolver::new(SsrfPolicy {
            deny_private: true,
            allowlist: vec![HostMatcher::exact("localhost")],
            ..Default::default()
        });

        let addresses = resolve_host(&resolver, "localhost")
            .await
            .expect("an allowlisted host must be permitted before its addresses are tested");

        assert!(
            addresses.iter().all(std::net::IpAddr::is_loopback),
            "expected only loopback addresses, got {addresses:?}"
        );
    }

    #[tokio::test]
    async fn reports_a_resolution_failure_rather_than_a_policy_denial() {
        let resolver = PolicyResolver::new(deny_private_policy());

        let error = resolve_host(&resolver, "no-such-host.invalid")
            .await
            .expect_err(".invalid never resolves, so this must fail");

        assert!(
            error.starts_with("dns resolution failed"),
            "expected a resolution failure, got {error}"
        );
    }

    #[tokio::test]
    async fn returns_addresses_with_the_resolution_port_for_hyper_to_rewrite() {
        let resolver = PolicyResolver::new(SsrfPolicy {
            deny_private: false,
            ..Default::default()
        });
        let name = Name::from_str("localhost").expect("localhost must be a valid DNS name");

        let addresses: Vec<SocketAddr> = resolver.resolve(name).await.expect("localhost must resolve").collect();

        assert!(
            addresses.iter().all(|addr| addr.port() == RESOLUTION_PORT),
            "hyper rewrites port 0 with the URL's port; got {addresses:?}"
        );
    }
}
