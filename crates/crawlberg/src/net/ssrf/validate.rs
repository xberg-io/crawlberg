//! URL and IP validation against an [`SsrfPolicy`].

use ipnet::IpNet;
use std::net::IpAddr;
use std::sync::LazyLock;

use super::policy::is_supported_scheme;
use super::{SsrfError, SsrfPolicy};

/// Private / metadata / loopback CIDRs that are denied by default, as source strings.
///
/// `crawlberg-browser` keeps its own copy for standalone use; the parity test in
/// `crate::net::browser_policy` asserts the two have not drifted.
pub(crate) const DEFAULT_DENY_NET_CIDRS: [&str; 13] = [
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

/// Test if an IP address is permitted by the SSRF policy.
///
/// Returns true if the IP is allowed, false if it should be rejected.
/// Collapse an IPv6 address that actually addresses IPv4 space into that IPv4 address.
///
/// `ipnet`'s `contains` only matches within an address family, so `::ffff:127.0.0.1`
/// would be tested against the IPv6 deny-nets only and sail past `127.0.0.0/8`. On a
/// dual-stack host the kernel routes such an address to the IPv4 destination, so
/// without this the deny-list is bypassable by writing the literal in IPv6 form.
///
/// Covers the IPv4-mapped form (`::ffff:a.b.c.d`) and the NAT64 well-known prefix
/// (`64:ff9b::/96`, RFC 6052), which embeds an IPv4 address the same way.
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

pub(crate) fn is_ip_permitted(ip: IpAddr, policy: &SsrfPolicy) -> bool {
    if !policy.deny_private {
        return true;
    }

    let ip = canonicalize_ip(ip);

    if policy.allowlist.iter().any(|m| m.matches_ip(&ip)) {
        return true;
    }

    !DEFAULT_DENY_NETS.iter().any(|net| net.contains(&ip))
}

/// Classify a private IP into a category for error messaging.
pub(crate) fn classify_private_ip(ip: IpAddr) -> &'static str {
    // ~keep Classify the address actually routed to, so ::ffff:127.0.0.1 reports
    // "loopback" rather than falling through to the generic IPv6 arm.
    match canonicalize_ip(ip) {
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
