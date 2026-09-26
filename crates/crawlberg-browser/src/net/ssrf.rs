//! SSRF validation for the browser layer.
//!
//! This crate cannot depend on `crawlberg` (the dependency runs the other way, behind
//! the optional `browser-native` feature), so the real [`crawlberg::net::ssrf`] policy
//! cannot be named here. Instead the policy is *injected*: [`SsrfValidator`] is the
//! seam, and `crawlberg` supplies an implementation backed by the real `SsrfPolicy`,
//! allowlist included.
//!
//! [`DefaultSsrfValidator`] is the standalone fallback used when nobody injects one —
//! it re-implements only the default deny-list, never the allowlist matching, so there
//! is exactly one implementation of the security-relevant matching logic in the stack.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use ipnet::IpNet;
use url::Url;

/// Private / metadata / loopback CIDRs denied by [`DefaultSsrfValidator`].
///
/// Kept in sync with `crawlberg::net::ssrf::DEFAULT_DENY_NETS` by the parity test in
/// that module, which compares it against [`DEFAULT_DENY_NET_CIDRS`].
static DEFAULT_DENY_NETS: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    DEFAULT_DENY_NET_CIDRS
        .iter()
        .map(|c| c.parse().expect("literal CIDR"))
        .collect()
});

/// The deny-list as source strings, exported so `crawlberg` can assert the two copies
/// have not drifted.
pub const DEFAULT_DENY_NET_CIDRS: [&str; 13] = [
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "0.0.0.0/8",
    "224.0.0.0/4",
    // ~keep RFC 6598 shared address space. Not covered by any RFC 1918 range, but it carries
    // ~keep Alibaba Cloud's metadata endpoint (100.100.100.200) and Tailscale/CGNAT node addresses.
    "100.64.0.0/10",
    "::1/128",
    // ~keep The IPv6 analogue of 0.0.0.0: a kernel routes connect(::) to a local address, so it
    // ~keep is denied for the same reason 0.0.0.0/8 is. `::1/128` matches only loopback, not `::`.
    "::/128",
    "fe80::/10",
    "fc00::/7",
    "ff00::/8",
];

/// Decides whether the browser layer may fetch a URL.
///
/// Errors are plain strings: naming a typed error would require pulling `crawlberg`'s
/// `SsrfError` into this crate, which is the dependency this seam exists to avoid. The
/// stable denial-reason substrings from the core policy survive in the message.
#[async_trait::async_trait]
pub trait SsrfValidator: std::fmt::Debug + Send + Sync {
    /// Return `Ok(())` if `url` may be fetched.
    async fn validate(&self, url: &Url) -> Result<(), String>;
}

/// Parse the `CRAWLBERG_ALLOW_PRIVATE_NETWORK` override.
///
/// Anything that is not an explicit affirmative denies, so a typo or an empty value
/// cannot silently disable the policy.
fn parse_allow_private(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1") | Some("true")
    )
}

/// Deny-list-only validator used when the embedding application injects nothing.
///
/// This is never the validator in a `crawlberg` crawl — `crawlberg` always injects one
/// carrying the configured policy — so it only governs direct use of this crate.
#[derive(Debug)]
pub struct DefaultSsrfValidator {
    deny_private: bool,
}

impl DefaultSsrfValidator {
    /// Build a validator, reading the `CRAWLBERG_ALLOW_PRIVATE_NETWORK` override.
    pub fn from_env() -> Self {
        let raw = std::env::var("CRAWLBERG_ALLOW_PRIVATE_NETWORK").ok();
        Self {
            deny_private: !parse_allow_private(raw.as_deref()),
        }
    }
}

impl Default for DefaultSsrfValidator {
    fn default() -> Self {
        Self::from_env()
    }
}

#[async_trait::async_trait]
impl SsrfValidator for DefaultSsrfValidator {
    async fn validate(&self, url: &Url) -> Result<(), String> {
        let scheme = url.scheme();
        // ~keep Scheme is checked before the private-network override: allowing private
        // addresses is not a reason to start speaking ftp:// or gopher://.
        if scheme != "http" && scheme != "https" {
            return Err(format!(
                "Forbidden URL scheme '{scheme}' - only http and https are allowed"
            ));
        }

        if !self.deny_private {
            return Ok(());
        }

        // ~keep Localhost names are blocked before DNS to close rebinding gaps between
        // validation and request time. This validator does not resolve; the injected
        // crawlberg one does, and closes the gap properly.
        match url.host() {
            Some(url::Host::Ipv4(ip)) if is_ip_denied(ip.into()) => {
                Err(format!("Access to private/internal IP address {ip} is not allowed"))
            }
            Some(url::Host::Ipv6(ip)) if is_ip_denied(ip.into()) => {
                Err(format!("Access to private/internal IPv6 address {ip} is not allowed"))
            }
            Some(url::Host::Domain(domain)) if is_localhost_name(domain) => {
                Err(format!("Localhost rebinding attack blocked: {domain}"))
            }
            _ => Ok(()),
        }
    }
}

/// The IPv4 addresses an IPv6 address embeds, for each form that is routed to that IPv4 host.
///
/// Mirrors `embedded_ipv4s` in `crawlberg::net::ssrf`'s `validate` submodule, which cites
/// the RFC for each form and says which local-use NAT64 positions are skipped and why.
/// Without it, `::ffff:127.0.0.1` is only tested against the IPv6 deny-nets and slips past
/// `127.0.0.0/8`, while a dual-stack host routes it straight to loopback.
fn embedded_ipv4s(v6: Ipv6Addr) -> impl Iterator<Item = Ipv4Addr> {
    let octets = v6.octets();
    let at = |a: usize, b: usize, c: usize, d: usize| Ipv4Addr::new(octets[a], octets[b], octets[c], octets[d]);
    let segments = v6.segments();

    let fixed = if v6.is_unspecified() || v6.is_loopback() {
        None
    } else {
        v6.to_ipv4().or(match segments {
            [0, 0, 0, 0, 0xffff, 0, _, _] | [0x0064, 0xff9b, 0, 0, 0, 0, _, _] => Some(at(12, 13, 14, 15)),
            [0x2002, ..] => Some(at(2, 3, 4, 5)),
            _ => None,
        })
    };
    let isatap = matches!(segments, [_, _, _, _, 0 | 0x0200, 0x5efe, _, _]).then(|| at(12, 13, 14, 15));
    let local_nat64 = matches!(segments, [0x0064, 0xff9b, 0x0001, ..]).then(|| {
        let positions = [at(6, 7, 9, 10), at(7, 9, 10, 11), at(9, 10, 11, 12), at(12, 13, 14, 15)];
        let skipped = |v4: &Ipv4Addr| v4.octets()[0] == 0 || v4.is_multicast();
        let none_left = positions.iter().all(skipped);
        positions.into_iter().filter(move |v4| none_left || !skipped(v4))
    });

    fixed.into_iter().chain(isatap).chain(local_nat64.into_iter().flatten())
}

/// Whether `ip` itself, or any IPv4 address it embeds, falls in the deny-list.
fn is_ip_denied(ip: IpAddr) -> bool {
    let embedded = match ip {
        IpAddr::V6(v6) => Some(embedded_ipv4s(v6).map(IpAddr::V4)),
        IpAddr::V4(_) => None,
    };
    std::iter::once(ip)
        .chain(embedded.into_iter().flatten())
        .any(|candidate| DEFAULT_DENY_NETS.iter().any(|net| net.contains(&candidate)))
}

fn is_localhost_name(domain: &str) -> bool {
    let lower = domain.to_ascii_lowercase();
    lower == "localhost" || lower.ends_with(".localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        s.parse().expect("valid URL")
    }

    async fn validate(target: &str, deny_private: bool) -> Result<(), String> {
        DefaultSsrfValidator { deny_private }.validate(&url(target)).await
    }

    #[test]
    fn parse_allow_private_only_accepts_explicit_affirmatives() {
        // ~keep Regression: this used to be `env_var_os(..).is_some()`, so
        // CRAWLBERG_ALLOW_PRIVATE_NETWORK=0 disabled SSRF checking entirely.
        for affirmative in ["1", "true", "TRUE", " true "] {
            assert!(
                parse_allow_private(Some(affirmative)),
                "{affirmative:?} must enable the private-network override"
            );
        }

        for negative in ["0", "false", "FALSE", "", "banana", "2"] {
            assert!(
                !parse_allow_private(Some(negative)),
                "{negative:?} must NOT enable the private-network override"
            );
        }

        assert!(!parse_allow_private(None), "an unset variable must deny");
    }

    #[tokio::test]
    async fn default_validator_denies_private_and_metadata_addresses() {
        for denied in [
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://[fc00::1]/",
            // ~keep IPv4-mapped IPv6 forms route to IPv4 on a dual-stack host, so they
            // must be denied by the IPv4 nets rather than slipping past the IPv6 ones.
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://[64:ff9b::7f00:1]/",
            "http://[::ffff:0:a00:5]/",
            "http://[::a00:5]/",
            "http://[2002:a9fe:a9fe::]/",
            "http://[64:ff9b:1::a00:5]/",
            "http://[64:ff9b:1:a00:0:500::]/",
            "http://[64:ff9b:1:a:0:5::]/",
            "http://[64:ff9b:1:0:a:0:500:0]/",
            "http://[2001:db8::5efe:a00:5]/",
            "http://[2001:db8::200:5efe:7f00:1]/",
            "http://[fe80::5efe:808:808]/",
            "http://[64:ff9b:1:ac10:8:800::]/",
            "http://[64:ff9b:1::]/",
            "http://[64:ff9b:1:e000::]/",
            "http://[64:ff9b:1::e000:1]/",
        ] {
            assert!(
                validate(denied, true).await.is_err(),
                "{denied} must be denied by the default validator"
            );
        }
    }

    #[tokio::test]
    async fn default_validator_permits_public_addresses() {
        for permitted in [
            "http://1.1.1.1/",
            "http://[::ffff:0:808:808]/",
            "http://[::808:808]/",
            "http://[2002:808:808::]/",
            "http://[64:ff9b:1:808:8:800::]/",
            "http://[64:ff9b:1:8:8:808::]/",
            "http://[64:ff9b:1:0:8:808:800:0]/",
            "http://[64:ff9b:1::808:808]/",
            "http://[2001:db8::5efe:808:808]/",
            "http://[2001:db8::200:5efe:808:808]/",
            "http://[64:ff9b:1:0:8:808:e600:0]/",
        ] {
            validate(permitted, true)
                .await
                .unwrap_or_else(|e| panic!("{permitted} must be permitted: {e}"));
        }
    }

    #[tokio::test]
    async fn default_validator_denies_localhost_by_name() {
        for denied in ["http://localhost/", "http://api.localhost/"] {
            assert!(
                validate(denied, true).await.is_err(),
                "{denied} must be blocked before resolution"
            );
        }
    }

    #[tokio::test]
    async fn default_validator_denies_non_http_schemes_even_when_private_is_allowed() {
        // ~keep The old code returned Ok early on the env override, which also
        // re-enabled every non-http scheme.
        for denied in ["ftp://example.com/", "file:///etc/passwd", "gopher://example.com/"] {
            assert!(
                validate(denied, false).await.is_err(),
                "{denied} must be denied on scheme regardless of the private-network override"
            );
        }
    }

    #[tokio::test]
    async fn allowing_private_networks_permits_loopback() {
        validate("http://127.0.0.1/", false)
            .await
            .expect("loopback must be permitted when private networks are allowed");
    }

    #[test]
    fn deny_net_cidrs_all_parse() {
        assert_eq!(
            DEFAULT_DENY_NETS.len(),
            DEFAULT_DENY_NET_CIDRS.len(),
            "every exported CIDR string must parse into the deny-list"
        );
    }
}
