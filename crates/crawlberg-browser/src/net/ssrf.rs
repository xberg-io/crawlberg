//! SSRF policy validation for the browser layer.
//!
//! This module duplicates the deny-list and validation logic from the core
//! `crawlberg::net::ssrf` module to avoid a circular dependency (crawlberg
//! optionally depends on crawlberg-browser). The constants are kept in sync
//! by convention.
//!
//! Browser-specific mitigations:
//! - DNS-rebinding defense via hostname string matching (before DNS resolution).
//! - File-scheme bypass for test support.

use std::net::IpAddr;
use std::sync::LazyLock;

use ipnet::IpNet;
use url::Url;

/// Private / metadata / loopback CIDRs that are denied by default.
/// Must be kept in sync with `crawlberg::net::ssrf::DEFAULT_DENY_NETS`.
static DEFAULT_DENY_NETS: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    vec![
        "127.0.0.0/8".parse().unwrap(),
        "10.0.0.0/8".parse().unwrap(),
        "172.16.0.0/12".parse().unwrap(),
        "192.168.0.0/16".parse().unwrap(),
        "169.254.0.0/16".parse().unwrap(),
        "0.0.0.0/8".parse().unwrap(),
        "224.0.0.0/4".parse().unwrap(),
        "::1/128".parse().unwrap(),
        "fe80::/10".parse().unwrap(),
        "fc00::/7".parse().unwrap(),
        "ff00::/8".parse().unwrap(),
    ]
});

/// Validate an IP address against the SSRF policy.
fn is_ip_denied(ip: IpAddr) -> bool {
    for net in DEFAULT_DENY_NETS.iter() {
        if net.contains(&ip) {
            return true;
        }
    }
    false
}

/// Validate a URL for SSRF risks, with browser-specific mitigations.
///
/// Returns `Ok(())` if the URL is safe to fetch, or an error message otherwise.
///
/// # Logic
///
/// 1. Allows `file://` unconditionally (browser-specific for test support).
/// 2. Checks scheme is `http` or `https`.
/// 3. For IP addresses, rejects those in deny-list ranges.
/// 4. For domain names, string-matches `localhost` / `.localhost` (DNS-rebinding mitigation).
/// 5. Respects `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var to bypass checks.
pub fn validate_url(url: &Url) -> Result<(), String> {
    let scheme = url.scheme();

    if scheme == "file" {
        return Ok(());
    }

    let allow_private_network = std::env::var_os("CRAWLBERG_ALLOW_PRIVATE_NETWORK").is_some();
    if allow_private_network {
        return Ok(());
    }

    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "Forbidden URL scheme '{}' - only http, https, and file are allowed",
            scheme
        ));
    }

    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                let ip_addr: IpAddr = ip.into();
                if is_ip_denied(ip_addr) {
                    return Err(format!("Access to private/internal IP address {} is not allowed", ip));
                }
            }
            url::Host::Ipv6(ip) => {
                let ip_addr: IpAddr = ip.into();
                if is_ip_denied(ip_addr) {
                    return Err(format!("Access to private/internal IPv6 address {} is not allowed", ip));
                }
            }
            url::Host::Domain(domain) => {
                let lower_domain = domain.to_lowercase();

                // ~keep Block localhost names before DNS to close rebinding gaps between validation and request time.
                if lower_domain == "localhost" || lower_domain.ends_with(".localhost") {
                    return Err(format!("Localhost rebinding attack blocked: {}", domain));
                }

                // ~keep Literal IP string checks are redundant here because typed IP hosts hit the range checks above.
            }
        }
    }

    Ok(())
}
