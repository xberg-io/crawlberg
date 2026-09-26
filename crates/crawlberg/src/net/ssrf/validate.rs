//! URL and IP validation against an [`SsrfPolicy`].

use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use super::policy::is_supported_scheme;
use super::{HostMatcher, SsrfError, SsrfPolicy};

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
        return Err(SsrfError::DisallowedScheme(scheme.to_string()));
    }

    let host = url
        .host()
        .ok_or_else(|| SsrfError::InvalidUrl(format!("missing hostname: {url}")))?;

    let host_str = match host {
        url::Host::Domain(d) => d,
        url::Host::Ipv4(ip) => {
            let ip_addr: IpAddr = ip.into();
            if is_ip_permitted(ip_addr, policy) {
                return Ok(());
            } else {
                let reason = classify_private_ip(ip_addr);
                return Err(SsrfError::DeniedByPolicy { reason });
            }
        }
        url::Host::Ipv6(ip) => {
            let ip_addr: IpAddr = ip.into();
            if is_ip_permitted(ip_addr, policy) {
                return Ok(());
            } else {
                let reason = classify_private_ip(ip_addr);
                return Err(SsrfError::DeniedByPolicy { reason });
            }
        }
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
            if !is_ip_permitted(*ip, policy) {
                let reason = classify_private_ip(*ip);
                return Err(SsrfError::DeniedByPolicy { reason });
            }
        }

        Ok(())
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
/// address in bits 16 to 47 (RFC 3056 section 2), and an ISATAP interface identifier
/// `0000:5efe` or `0200:5efe` under any prefix (RFC 5214 section 6.1).
///
/// The local-use NAT64 prefix `64:ff9b:1::/48` (RFC 8215) fixes no position for the
/// address: a network may use the whole /48 or a /56, /64 or /96 inside it, and RFC 6052
/// section 2.2 places the address differently for each. Every one of the four positions
/// is returned, except one that reads as `0.0.0.0/8`, multicast or `240.0.0.0/4`: the
/// zero bits of a valid address read as `0.0.0.0/8` at the positions its network does not
/// use, and the shifted bytes of a public address often read as multicast or `240.0.0.0/4`.
/// When every position is skipped, all four are returned, so the address is refused: no
/// real destination encodes that way, and a stateful NAT64 translator such as Jool forwards
/// `0.0.0.0` to its own host. Teredo `2001::/32` is not unwrapped: RFC 4380 section 5.2.4 requires
/// every Teredo node to drop a packet whose embedded address is not global.
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
            [0x2002, ..] => Some(at(2, 3, 4, 5)),
            _ => None,
        })
    };
    let isatap = matches!(segments, [_, _, _, _, 0 | 0x0200, 0x5efe, _, _]).then(|| at(12, 13, 14, 15));
    let local_nat64 = matches!(segments, [0x0064, 0xff9b, 0x0001, ..]).then(|| {
        let positions = [at(6, 7, 9, 10), at(7, 9, 10, 11), at(9, 10, 11, 12), at(12, 13, 14, 15)];
        let skipped = |v4: &Ipv4Addr| v4.octets()[0] == 0 || v4.octets()[0] >= 224;
        let none_left = positions.iter().all(skipped);
        positions.into_iter().filter(move |v4| none_left || !skipped(v4))
    });

    fixed.into_iter().chain(isatap).chain(local_nat64.into_iter().flatten())
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

/// Classify a private IP into a category for error messaging.
pub(crate) fn classify_private_ip(ip: IpAddr) -> &'static str {
    // ~keep Classify the address actually routed to, so ::ffff:127.0.0.1 reports
    // "loopback" rather than falling through to the generic IPv6 arm.
    match denied_address(ip, &[]).unwrap_or(ip) {
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
                0xfe80 => "link_local",
                0xfc00 | 0xfd00 => "unique_local",
                0xff00..=0xffff => "multicast",
                _ => "private_network",
            }
        }
    }
}
