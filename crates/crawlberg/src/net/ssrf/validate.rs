//! URL and IP validation against an [`SsrfPolicy`].

use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use super::policy::is_supported_scheme;
use super::{HostMatcher, SsrfError, SsrfPolicy};

/// Refused schemes a [`SsrfError::DisallowedScheme`] names. Any other scheme is reported as
/// [`UNNAMED_SCHEME`]: an address written without a scheme, such as `user:token@host`, parses
/// with its user name as the scheme.
///
/// Kept in sync with `crawlberg_browser::net::ssrf::NAMED_SCHEMES` apart from `http` and
/// `https` (a configured `scheme_allowlist` can refuse either; the browser layer never does)
/// by the parity test in `crate::net::browser_policy`.
pub(crate) const NAMED_SCHEMES: [&str; 21] = [
    "http",
    "https",
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

/// What a [`SsrfError::DisallowedScheme`] carries for a scheme not in [`NAMED_SCHEMES`].
const UNNAMED_SCHEME: &str = "unrecognized";

/// Private / metadata / loopback CIDRs that are denied by default, as source strings.
///
/// `crawlberg-browser` keeps its own copy for standalone use; the parity test in
/// `crate::net::browser_policy` asserts the two have not drifted.
pub(crate) const DEFAULT_DENY_NET_CIDRS: [&str; 14] = [
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

/// Private / metadata / loopback CIDRs that are denied by default.
static DEFAULT_DENY_NETS: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    DEFAULT_DENY_NET_CIDRS
        .iter()
        .map(|cidr| cidr.parse().expect("literal CIDR"))
        .collect()
});

/// Validate a URL against the SSRF policy.
///
/// 1. Validates the scheme against `policy.scheme_allowlist`.
/// 2. If the host is a literal IP, decides on that IP alone and returns.
/// 3. Otherwise, if the hostname matches an `Exact`/`Suffix` allowlist entry, permits it
///    without resolving — see [`SsrfPolicy::allowlist`] for why that shortcut exists.
/// 4. Otherwise resolves the hostname and requires *every* resolved IP to be permitted.
///
/// An allowlist is permissive only: a host that matches nothing is not rejected for that
/// reason, it simply falls through to the `deny_private` deny-list.
///
/// DNS rebinding mitigation: all resolved IPs are validated; if ANY resolved IP violates
/// the policy, the URL is rejected.
///
/// **wasm32 targets only check literal IP hosts.** There is no DNS resolution on `wasm32`
/// (no `tokio::net`), so step 4 never runs: a hostname that survives the scheme and allowlist
/// checks above is permitted unconditionally, regardless of `policy.deny_private`. See the
/// `wasm32`-specific note on [`SsrfPolicy::from_env`] for why this matters under Node.js.
pub async fn validate_url(url: &url::Url, policy: &SsrfPolicy) -> Result<(), SsrfError> {
    let scheme = url.scheme();
    if !is_supported_scheme(scheme)
        || !policy
            .scheme_allowlist
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(scheme))
    {
        let shown = if NAMED_SCHEMES.contains(&scheme) {
            scheme
        } else {
            UNNAMED_SCHEME
        };
        return Err(SsrfError::DisallowedScheme(shown.to_string()));
    }

    let host = url
        .host()
        .ok_or_else(|| SsrfError::InvalidUrl(format!("missing hostname: {url}")))?;

    let host_str = match host {
        url::Host::Domain(d) => d,
        url::Host::Ipv4(ip) => return check_ip(ip.into(), policy),
        url::Host::Ipv6(ip) => return check_ip(ip.into(), policy),
    };

    for matcher in &policy.allowlist {
        if matcher.matches_host(host_str) {
            return Ok(());
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        // ~keep wasm32 has no tokio::net; browser/edge same-origin and CORS policy gate non-allowlisted domains.
        let _ = port_for_url(scheme, url);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let port = port_for_url(scheme, url);
        let lookup_addr = format!("{host_str}:{port}");
        let addresses: Vec<IpAddr> = tokio::net::lookup_host(&lookup_addr)
            .await
            .map_err(|e| SsrfError::DnsResolutionFailed(format!("{host_str}: {e}")))?
            .map(|addr| addr.ip())
            .collect();

        if addresses.is_empty() {
            return Err(SsrfError::DnsResolutionFailed(format!(
                "no addresses resolved for {host_str}"
            )));
        }

        // ~keep DNS rebinding mitigation: every resolved IP must satisfy policy.
        for ip in &addresses {
            check_ip(*ip, policy)?;
        }

        Ok(())
    }
}

/// Decide one address, naming the reason when the policy refuses it.
fn check_ip(ip: IpAddr, policy: &SsrfPolicy) -> Result<(), SsrfError> {
    if is_ip_permitted(ip, policy) {
        Ok(())
    } else {
        let reason = classify_private_ip(ip, &policy.allowlist);
        Err(SsrfError::DeniedByPolicy { reason })
    }
}

fn port_for_url(scheme: &str, url: &url::Url) -> u16 {
    url.port().unwrap_or(match scheme {
        "https" => 443,
        _ => 80,
    })
}

/// The IPv4 addresses an IPv6 address embeds, for each form that is routed to that IPv4 host.
///
/// `ipnet`'s `contains` only matches within an address family, so `::ffff:127.0.0.1`
/// would be tested against the IPv6 deny-nets only and sail past `127.0.0.0/8`. A host or
/// network that carries such an address reaches the IPv4 destination, so without this the
/// deny-list is bypassable by writing the literal in IPv6 form.
///
/// Covers the IPv4-mapped and IPv4-compatible forms (RFC 4291 section 2.5.5), the
/// IPv4-translated form `::ffff:0:0:0/96` (RFC 2765 section 2.1), the NAT64 well-known
/// prefix `64:ff9b::/96` (RFC 6052 section 2.1), 6to4 `2002::/16`, which carries the
/// address in bits 16 to 47 (RFC 3056 section 2), Teredo `2001:0::/32`, which carries the
/// client address inverted in the last 32 bits (RFC 4380 section 4), and an ISATAP
/// interface identifier `0000:5efe` or `0200:5efe` under any prefix (RFC 5214 section 6.1).
///
/// Each of those forms fixes one position, so its one reading is taken as it is, with no
/// skip rule. An address only shaped like one is therefore refused for what that position
/// reads: `2001:db8::5efe:1:1` reads as `0.1.0.1` and is refused. That space carries no
/// legitimate traffic, so the refusal is deliberate.
///
/// The local-use NAT64 prefix `64:ff9b:1::/48` (RFC 8215) is read at the /96 position only,
/// the last 32 bits. A /48, /56 or /64 network inside it places the address elsewhere (RFC
/// 6052 section 2.2), and its unused low bits then read at the /96 position as `0.0.0.0` or,
/// on a /64 network, as the destination's last octet followed by three zero octets. So a
/// /96 reading whose last three octets are zero is skipped when any of bytes 6 to 11 is set:
/// that is the only shape where the /96 reading can be padding. When bytes 6 to 11 are all
/// zero, every prefix length reads the same address, and the reading stands.
fn embedded_ipv4s(v6: Ipv6Addr) -> impl Iterator<Item = Ipv4Addr> {
    let octets = v6.octets();
    let at = |a: usize, b: usize, c: usize, d: usize| Ipv4Addr::new(octets[a], octets[b], octets[c], octets[d]);
    let segments = v6.segments();

    // ~keep `::` and `::1` fall inside `::/96` but are the IPv6 unspecified and loopback
    // ~keep addresses; the IPv6 deny-nets already cover and classify both.
    let fixed = if v6.is_unspecified() || v6.is_loopback() {
        None
    } else {
        v6.to_ipv4().or(match segments {
            [0, 0, 0, 0, 0xffff, 0, _, _] | [0x0064, 0xff9b, 0, 0, 0, 0, _, _] => Some(at(12, 13, 14, 15)),
            [0x0064, 0xff9b, 0x0001, ..] => {
                let v4 = at(12, 13, 14, 15);
                let padding = octets[6..12].iter().any(|&b| b != 0) && v4.octets()[1..] == [0, 0, 0];
                (!padding).then_some(v4)
            }
            [0x2002, ..] => Some(at(2, 3, 4, 5)),
            [0x2001, 0, ..] => Some(Ipv4Addr::from(!u32::from(at(12, 13, 14, 15)))),
            _ => None,
        })
    };
    let isatap = matches!(segments, [_, _, _, _, 0 | 0x0200, 0x5efe, _, _]).then(|| at(12, 13, 14, 15));

    fixed.into_iter().chain(isatap)
}

/// The first address a connection to `ip` can reach that the default deny-list covers and
/// `allowlist` does not permit: `ip` itself, then each IPv4 address it embeds.
fn denied_address(ip: IpAddr, allowlist: &[HostMatcher]) -> Option<IpAddr> {
    let embedded = match ip {
        IpAddr::V6(v6) => Some(embedded_ipv4s(v6).map(IpAddr::V4)),
        IpAddr::V4(_) => None,
    };
    std::iter::once(ip)
        .chain(embedded.into_iter().flatten())
        .find(|candidate| {
            !allowlist.iter().any(|m| m.matches_ip(candidate))
                && DEFAULT_DENY_NETS.iter().any(|net| net.contains(candidate))
        })
}

/// Test if an IP address is permitted by the SSRF policy.
///
/// Returns true if the IP is allowed, false if it should be rejected.
pub(crate) fn is_ip_permitted(ip: IpAddr, policy: &SsrfPolicy) -> bool {
    !policy.deny_private || denied_address(ip, &policy.allowlist).is_none()
}

/// Classify a denied IP into a category for error messaging.
///
/// `allowlist` must be the one the deny decision used. Classifying against an empty
/// allowlist instead names the first deny-listed candidate, which the policy may permit:
/// with `fe80::/10` allowlisted, `fe80::5efe:10.0.0.5` is denied for the `10.0.0.5` it
/// carries, not for being link-local.
pub(crate) fn classify_private_ip(ip: IpAddr, allowlist: &[HostMatcher]) -> &'static str {
    // ~keep Classify the address actually routed to, so ::ffff:127.0.0.1 reports
    // "loopback" rather than falling through to the generic IPv6 arm.
    match denied_address(ip, allowlist).unwrap_or(ip) {
        IpAddr::V4(ipv4) => {
            let octets = ipv4.octets();
            match octets[0] {
                127 => "loopback",
                10 => "private_network",
                172 if octets[1] >= 16 && octets[1] <= 31 => "private_network",
                192 if octets[1] == 168 => "private_network",
                169 if octets[1] == 254 => "link_local",
                0 => "unspecified",
                224..=239 => "multicast",
                _ => "private_network",
            }
        }
        IpAddr::V6(ipv6) => {
            let segments = ipv6.segments();
            match segments[0] {
                0x0000
                    if segments[1] == 0
                        && segments[2] == 0
                        && segments[3] == 0
                        && segments[4] == 0
                        && segments[5] == 0
                        && segments[6] == 0
                        && segments[7] == 1 =>
                {
                    "loopback"
                }
                0x0000 if ipv6.segments() == [0; 8] => "unspecified",
                0xfe80..=0xfebf => "link_local",
                0xfc00..=0xfdff => "unique_local",
                0xff00..=0xffff => "multicast",
                _ => "private_network",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The non-`http`/`https` half of [`NAMED_SCHEMES`], hardcoded rather than read from
    /// the const. Walking `NAMED_SCHEMES` itself would make this test a no-op against the
    /// bug it guards: dropping an entry from the production list still refuses that scheme
    /// (it is simply unsupported), it just stops naming it, and a test that iterates the
    /// same list that shrank would shrink with it instead of turning red.
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
    async fn every_listed_scheme_is_named_in_the_refusal() {
        let policy = SsrfPolicy::default();
        for scheme in EXPECTED_NAMED_SCHEMES {
            let url = format!("{scheme}://example.com/")
                .parse::<url::Url>()
                .expect("valid URL");
            let err = validate_url(&url, &policy)
                .await
                .expect_err("an unsupported scheme must be refused");
            assert!(
                matches!(&err, SsrfError::DisallowedScheme(shown) if shown == scheme),
                "{scheme} must be named in the refusal, got {err:?}"
            );
        }
    }
}
