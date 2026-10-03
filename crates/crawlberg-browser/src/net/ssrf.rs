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
static DEFAULT_DENY_NETS: LazyLock<Vec<(IpNet, &'static str)>> = LazyLock::new(|| {
    DEFAULT_DENY_NET_CIDRS
        .iter()
        .zip(DENY_NET_REASONS)
        .map(|(cidr, reason)| (cidr.parse().expect("literal CIDR"), reason))
        .collect()
});

/// The deny-list as source strings, exported so `crawlberg` can assert the two copies
/// have not drifted.
pub const DEFAULT_DENY_NET_CIDRS: &[&str] = &[
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "0.0.0.0/8",
    "224.0.0.0/4",
    // ~keep RFC 1112 reserved range, which holds the broadcast address 255.255.255.255.
    "240.0.0.0/4",
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

/// The denial reason each entry of [`DEFAULT_DENY_NET_CIDRS`] reports, in the same order.
///
/// ~keep These are the reason strings `crawlberg::net::ssrf`'s `classify_private_ip`
/// produces, restated as a table rather than re-derived from the octets, so this crate
/// carries no second copy of the classification logic. The deny-nets are pairwise disjoint,
/// so the entry that matches is the entry that classifies. Sizing the array from
/// `DEFAULT_DENY_NET_CIDRS` makes adding a range without a reason a compile error, and
/// [`DEFAULT_DENY_NETS`] pairs the two at construction so no later lookup can miss and
/// return "not denied" for an address that is.
const DENY_NET_REASONS: [&str; DEFAULT_DENY_NET_CIDRS.len()] = [
    "loopback",
    "private_network",
    "private_network",
    "private_network",
    "link_local",
    "unspecified",
    "multicast",
    "private_network",
    "private_network",
    "loopback",
    "unspecified",
    "link_local",
    "unique_local",
    "multicast",
];

/// Refused schemes a refusal names. Any other scheme is not shown: an address written without
/// a scheme, such as `user:token@host`, parses with its user name as the scheme.
///
/// Kept in sync with `crawlberg::net::ssrf::NAMED_SCHEMES` apart from `http` and `https` (a
/// configured `scheme_allowlist` can refuse either there; this validator never refuses them)
/// by the parity test in `crawlberg::net::browser_policy`. Exported so that test can see it.
pub const NAMED_SCHEMES: [&str; 19] = [
    "ftp",
    "ftps",
    "sftp",
    "ssh",
    "telnet",
    "smb",
    "file",
    "data",
    "javascript",
    "mailto",
    "ws",
    "wss",
    "blob",
    "gopher",
    "dict",
    "ldap",
    "ldaps",
    "tftp",
    "about",
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

    /// Resolve `host` and return the addresses a connection to it may use.
    ///
    /// The native clients connect only to the addresses this returns (see
    /// [`ValidatorResolver`](crate::net::resolver::ValidatorResolver)), so a validator that checks
    /// resolved addresses does it here, on the lookup the connection uses. A check in `validate`
    /// alone is lost: its lookup is gone by the time the client resolves the host again, and a
    /// rebinding DNS answer differs.
    ///
    /// The default is the system lookup with no check, for a validator that decides by the URL
    /// alone.
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        system_lookup(host).await
    }
}

/// Resolve `host` with the system resolver.
async fn system_lookup(host: &str) -> Result<Vec<IpAddr>, String> {
    Ok(tokio::net::lookup_host((host, 0))
        .await
        .map_err(|e| format!("dns resolution failed: {host}: {e}"))?
        .map(|address| address.ip())
        .collect())
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

#[cfg(test)]
impl DefaultSsrfValidator {
    /// Build a validator with an explicit setting, independent of the environment.
    pub(crate) fn with_deny_private(deny_private: bool) -> Self {
        Self { deny_private }
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
            let shown = if NAMED_SCHEMES.contains(&scheme) {
                format!(" '{scheme}'")
            } else {
                String::new()
            };
            return Err(format!("Forbidden URL scheme{shown} - only http and https are allowed"));
        }

        if !self.deny_private {
            return Ok(());
        }

        // ~keep Localhost names are blocked before DNS. `validate` does not resolve; the
        // connect-time `resolve` checks every address the connection will use.
        match url.host() {
            Some(url::Host::Ipv4(ip)) => match denial_reason(ip.into()) {
                Some(reason) => Err(format!(
                    "Access to private/internal IP address {ip} is not allowed: {reason}"
                )),
                None => Ok(()),
            },
            Some(url::Host::Ipv6(ip)) => match denial_reason(ip.into()) {
                Some(reason) => Err(format!(
                    "Access to private/internal IPv6 address {ip} is not allowed: {reason}"
                )),
                None => Ok(()),
            },
            Some(url::Host::Domain(domain)) if is_localhost_name(domain) => {
                Err(format!("Localhost rebinding attack blocked: {domain}"))
            }
            _ => Ok(()),
        }
    }

    /// Refuses the host when any address it resolves to is in the deny-list.
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let addresses = system_lookup(host).await?;
        if self.deny_private
            && let Some((ip, reason)) = addresses
                .iter()
                .find_map(|ip| denial_reason(*ip).map(|reason| (ip, reason)))
        {
            return Err(format!(
                "{host} resolves to the private/internal address {ip}, which is not allowed: {reason}"
            ));
        }
        Ok(addresses)
    }
}

/// The IPv4 addresses an IPv6 address embeds, for each form that is routed to that IPv4 host.
///
/// Mirrors `embedded_ipv4s` in `crawlberg::net::ssrf`'s `validate` submodule, which cites
/// the RFC for each form and says when the local-use NAT64 reading is skipped and why.
/// Without it, `::ffff:127.0.0.1` is only tested against the IPv6 deny-nets and slips past
/// `127.0.0.0/8`, while a dual-stack host routes it straight to loopback.
fn embedded_ipv4s(v6: Ipv6Addr) -> impl Iterator<Item = Ipv4Addr> {
    let octets = v6.octets();
    let at = |a: usize, b: usize, c: usize, d: usize| Ipv4Addr::new(octets[a], octets[b], octets[c], octets[d]);
    let segments = v6.segments();

    // ~keep Unlike the core policy this validator has no allowlist, and `::` and `::1` are
    // ~keep matched by their own deny rows before any embedded reading, so it needs no carve-out
    // ~keep for them inside `::/96`.
    let fixed = v6.to_ipv4().or(match segments {
        [0, 0, 0, 0, 0xffff, 0, _, _] | [0x0064, 0xff9b, 0, 0, 0, 0, _, _] => Some(at(12, 13, 14, 15)),
        [0x0064, 0xff9b, 0x0001, ..] => {
            let v4 = at(12, 13, 14, 15);
            let padding = octets[6..12].iter().any(|&b| b != 0) && v4.octets()[1..] == [0, 0, 0];
            (!padding).then_some(v4)
        }
        [0x2002, ..] => Some(at(2, 3, 4, 5)),
        [0x2001, 0, ..] => Some(Ipv4Addr::from(!u32::from(at(12, 13, 14, 15)))),
        _ => None,
    });
    let isatap = matches!(segments, [_, _, _, _, 0 | 0x0200, 0x5efe, _, _]).then(|| at(12, 13, 14, 15));

    fixed.into_iter().chain(isatap)
}

/// The reason the deny-list refuses `ip`, or `None` when it does not.
///
/// Tries `ip` itself first, then each IPv4 address it embeds, so the reason names the
/// address the connection would actually reach. The core policy's `denied_address` walks
/// the same candidates in the same order.
fn denial_reason(ip: IpAddr) -> Option<&'static str> {
    let embedded = match ip {
        IpAddr::V6(v6) => Some(embedded_ipv4s(v6).map(IpAddr::V4)),
        IpAddr::V4(_) => None,
    };
    std::iter::once(ip)
        .chain(embedded.into_iter().flatten())
        .find_map(|candidate| {
            DEFAULT_DENY_NETS
                .iter()
                .find(|(net, _)| net.contains(&candidate))
                .map(|(_, reason)| *reason)
        })
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
            "http://[64:ff9b:1:a00::a00:5]/",
            "http://[2001:db8::5efe:a00:5]/",
            "http://[2001:db8::200:5efe:7f00:1]/",
            "http://[fe80::5efe:808:808]/",
            "http://[64:ff9b:1::]/",
            "http://[64:ff9b:1::e000:1]/",
            // ~keep RFC 4380 stores a Teredo client's IPv4 address as its one's complement:
            // 5601:5601 inverts to 169.254.169.254, the cloud metadata endpoint.
            "http://[2001:0:4136:e378:0:ffff:5601:5601]/",
            // The reserved range 240.0.0.0/4, which holds the broadcast address
            // 255.255.255.255, plain and Teredo-embedded (5fe:fdfc inverts to 250.1.2.3).
            "http://240.0.0.1/",
            "http://255.255.255.255/",
            "http://[2001:0:4136:e378:8000:63bf:5fe:fdfc]/",
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
            "http://[64:ff9b:1:a00::808:808]/",
            "http://[64:ff9b:1:0:8:808:a00:0]/",
            "http://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/",
            "http://[2001:db8::1]/",
            // Boundary: the last address below 224.0.0.0/4, plain and 6to4-embedded, stays
            // permitted; only 224.0.0.0/4 and above (multicast, then 240.0.0.0/4) are denied.
            "http://223.255.255.1/",
            "http://[2002:dfff:ff01::]/",
        ] {
            validate(permitted, true)
                .await
                .unwrap_or_else(|e| panic!("{permitted} must be permitted: {e}"));
        }
    }

    #[tokio::test]
    async fn default_validator_ends_its_message_with_the_denial_reason() {
        // ~keep The fallback cannot return crawlberg's typed error, so the reason travels as the
        // message suffix; crawlberg's parity test reads it back the same way.
        for (target, reason) in [
            ("http://127.0.0.1/", "loopback"),
            ("http://10.0.0.5/", "private_network"),
            ("http://[fd12::1]/", "unique_local"),
            ("http://[2002:a9fe:a9fe::]/", "link_local"),
        ] {
            let message = validate(target, true).await.expect_err("a denied address");
            assert!(
                message.ends_with(&format!(": {reason}")),
                "{target} must be refused as {reason}, got {message:?}"
            );
        }
    }

    #[tokio::test]
    async fn default_validator_names_the_denial_reason_at_connect_time() {
        // ~keep An IP literal resolves to itself without a DNS query, so the message is exact.
        let validator = DefaultSsrfValidator::with_deny_private(true);
        for (host, reason) in [
            ("::ffff:127.0.0.1", "loopback"),
            ("127.0.0.1", "loopback"),
            ("10.0.0.5", "private_network"),
            ("fd12::1", "unique_local"),
        ] {
            let message = validator.resolve(host).await.expect_err("a denied address");
            assert_eq!(
                message,
                format!("{host} resolves to the private/internal address {host}, which is not allowed: {reason}")
            );
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
    async fn default_validator_does_not_show_a_user_name_parsed_as_the_scheme() {
        for (target, parsed_scheme, secret) in [
            ("user:token@host", "user", "token"),
            ("KEY:@h:1", "key", "key"),
            ("localhost:3128", "localhost", "3128"),
        ] {
            let error = validate(target, true)
                .await
                .expect_err("a scheme other than http or https must be denied");
            assert!(
                error.contains("Forbidden URL scheme"),
                "{target} must be refused for its scheme, got: {error}"
            );
            let lowered = error.to_lowercase();
            for shown in [parsed_scheme, secret, "'"] {
                assert!(
                    !lowered.contains(shown),
                    "the refusal of {target} shows {shown:?}: {error}"
                );
            }
        }
    }

    #[tokio::test]
    async fn default_validator_names_a_known_refused_scheme() {
        for (target, named) in [("ftp://x", "'ftp'"), ("file:///x", "'file'")] {
            let error = validate(target, true)
                .await
                .expect_err("a non-http scheme must be denied");
            assert!(
                error.contains(named),
                "the refusal of {target} must name {named}, got: {error}"
            );
        }
    }

    /// The full [`NAMED_SCHEMES`] list, hardcoded rather than read from the const. See the
    /// core crate's `every_listed_scheme_is_named_in_the_refusal` for why: walking the const
    /// itself would keep passing after an entry is dropped from it, since a dropped scheme
    /// is still refused, just no longer named.
    const EXPECTED_NAMED_SCHEMES: [&str; 19] = [
        "ftp",
        "ftps",
        "sftp",
        "ssh",
        "telnet",
        "smb",
        "file",
        "data",
        "javascript",
        "mailto",
        "ws",
        "wss",
        "blob",
        "gopher",
        "dict",
        "ldap",
        "ldaps",
        "tftp",
        "about",
    ];

    #[tokio::test]
    async fn every_listed_scheme_is_named_in_the_browser_refusal() {
        for scheme in EXPECTED_NAMED_SCHEMES {
            let target = format!("{scheme}://x");
            let error = validate(&target, true)
                .await
                .expect_err("a non-http scheme must be denied");
            assert!(
                error.contains(&format!("'{scheme}'")),
                "{target} must name '{scheme}', got: {error}"
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
    fn exported_deny_net_cidrs_has_a_length_independent_slice_type() {
        trait LengthIndependentDenyList {}
        impl LengthIndependentDenyList for &[&str] {}

        fn assert_length_independent<T: LengthIndependentDenyList>(_: T) {}
        assert_length_independent(DEFAULT_DENY_NET_CIDRS);
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
