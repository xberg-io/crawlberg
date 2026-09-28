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

use std::net::IpAddr;
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
    /// [`ValidatorResolver`](crate::net::resolver::ValidatorResolver)), so the policy decision and
    /// the connection share one lookup. `validate` alone cannot give that: its lookup is gone by
    /// the time the client resolves the host again, and a rebinding DNS answer differs.
    ///
    /// The default resolves with the system resolver and passes each address through
    /// [`validate`](Self::validate) as a literal-address URL. Override it when the policy also
    /// decides by host name.
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let addresses: Vec<IpAddr> = tokio::net::lookup_host((host, 0))
            .await
            .map_err(|e| format!("dns resolution failed: {host}: {e}"))?
            .map(|address| address.ip())
            .collect();
        if addresses.is_empty() {
            return Err(format!("dns resolution failed: no addresses resolved for {host}"));
        }
        for ip in &addresses {
            let mut literal = Url::parse("http://0.0.0.0/").expect("literal URL");
            literal.set_ip_host(*ip).expect("an http URL accepts an IP host");
            self.validate(&literal).await?;
        }
        Ok(addresses)
    }
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

/// Collapse an IPv6 address that actually addresses IPv4 space into that IPv4 address.
///
/// Mirrors `crawlberg::net::ssrf::canonicalize_ip`. Without it, `::ffff:127.0.0.1` is
/// only tested against the IPv6 deny-nets and slips past `127.0.0.0/8`, while a
/// dual-stack host routes it straight to loopback.
fn canonicalize_ip(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else { return ip };

    if let Some(v4) = v6.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }

    let segments = v6.segments();
    if segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2..6] == [0, 0, 0, 0] {
        let octets = v6.octets();
        return IpAddr::V4(std::net::Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]));
    }

    ip
}

fn is_ip_denied(ip: IpAddr) -> bool {
    let ip = canonicalize_ip(ip);
    DEFAULT_DENY_NETS.iter().any(|net| net.contains(&ip))
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
        ] {
            assert!(
                validate(denied, true).await.is_err(),
                "{denied} must be denied by the default validator"
            );
        }
    }

    #[tokio::test]
    async fn default_validator_permits_public_addresses() {
        validate("http://1.1.1.1/", true)
            .await
            .expect("a public address must be permitted");
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
    fn deny_net_cidrs_all_parse() {
        assert_eq!(
            DEFAULT_DENY_NETS.len(),
            DEFAULT_DENY_NET_CIDRS.len(),
            "every exported CIDR string must parse into the deny-list"
        );
    }
}
