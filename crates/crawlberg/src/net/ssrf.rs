//! SSRF policy and validation for outbound HTTP requests.
//!
//! Provides a deny-by-default policy on private IP space, DNS rebinding mitigation,
//! and allowlist matching for URLs in crawl, sitemap, and robots.txt operations.

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::LazyLock;

/// Private / metadata / loopback CIDRs that are denied by default, as source strings.
///
/// `crawlberg-browser` keeps its own copy for standalone use; the parity test in
/// `crate::net::browser_policy` asserts the two have not drifted.
pub(crate) const DEFAULT_DENY_NET_CIDRS: [&str; 11] = [
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "0.0.0.0/8",
    "224.0.0.0/4",
    "::1/128",
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

/// Hostname/IP allowlist matcher for SSRF policy.
///
/// Serializes as an internally-tagged object so each variant is distinguishable on the
/// wire and round-trips losslessly:
///
/// ```json
/// {"type": "exact",  "value": "api.example.com"}
/// {"type": "suffix", "value": ".example.com"}
/// {"type": "cidr",   "value": "10.0.0.0/8"}
/// ```
///
/// A bare JSON string is still accepted on deserialization and resolves to [`Exact`],
/// preserving configs written against the previous untagged representation.
///
/// [`Exact`]: HostMatcher::Exact
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(alef, alef(skip))]
pub enum HostMatcher {
    /// Exact hostname match (case-insensitive).
    Exact {
        /// The hostname to match.
        value: String,
    },
    /// Suffix match: ".xberg.io" matches "api.xberg.io" and "xberg.io".
    Suffix {
        /// The dot-prefixed suffix to match. A leading dot is optional.
        value: String,
    },
    /// CIDR match: "10.0.0.0/8" matches IP addresses in that range.
    Cidr {
        /// The CIDR block. Validated when built through [`HostMatcher::cidr`] or
        /// deserialization.
        value: String,
    },
}

impl HostMatcher {
    /// Build an exact hostname matcher.
    pub fn exact(host: impl Into<String>) -> Self {
        HostMatcher::Exact { value: host.into() }
    }

    /// Build a suffix matcher. A leading dot is optional: `".example.com"` and
    /// `"example.com"` behave identically.
    pub fn suffix(suffix: impl Into<String>) -> Self {
        HostMatcher::Suffix { value: suffix.into() }
    }

    /// Build a CIDR matcher, rejecting a malformed block.
    ///
    /// Validating here rather than at match time keeps a typo from degrading into a
    /// silent "does not match", which for an allowlist would fail in the permissive
    /// direction for every other rule that depends on it.
    ///
    /// # Errors
    ///
    /// Returns [`SsrfError::InvalidCidr`] if `cidr` does not parse as an IPv4 or IPv6
    /// network.
    pub fn cidr(cidr: impl Into<String>) -> Result<Self, SsrfError> {
        let value = cidr.into();
        match value.parse::<IpNet>() {
            Ok(_) => Ok(HostMatcher::Cidr { value }),
            Err(e) => Err(SsrfError::InvalidCidr(format!("{value}: {e}"))),
        }
    }

    /// Test if this matcher matches the given hostname.
    ///
    /// `Cidr` never matches a hostname: it is an IP-space rule, and a hostname is only
    /// resolved to IPs by [`validate_url`].
    pub fn matches_host(&self, host: &str) -> bool {
        match self {
            HostMatcher::Exact { value } => value.eq_ignore_ascii_case(host),
            HostMatcher::Suffix { value } => {
                let suffix_clean = value.trim_start_matches('.').to_ascii_lowercase();
                let host_lower = host.to_ascii_lowercase();
                host_lower == suffix_clean || host_lower.ends_with(&format!(".{suffix_clean}"))
            }
            HostMatcher::Cidr { .. } => false,
        }
    }

    /// Test if this matcher matches the given IP address.
    ///
    /// `Exact` and `Suffix` never match a literal IP; allowlisting an address requires
    /// a `Cidr` matcher.
    pub fn matches_ip(&self, ip: &IpAddr) -> bool {
        match self {
            HostMatcher::Exact { .. } | HostMatcher::Suffix { .. } => false,
            HostMatcher::Cidr { value } => match value.parse::<IpNet>() {
                Ok(net) => net.contains(ip),
                Err(e) => {
                    // ~keep Reachable only via a struct literal bypassing HostMatcher::cidr;
                    // deny, but say so, because a silent false is an allowlist hole.
                    tracing::warn!(cidr = %value, error = %e, "ignoring malformed CIDR allowlist entry");
                    false
                }
            },
        }
    }
}

/// Wire form accepted for [`HostMatcher`]: either the tagged object or a legacy bare
/// string. Kept separate from `HostMatcher` so the tagged arm can run CIDR validation
/// before a value exists.
#[derive(Deserialize)]
#[serde(untagged)]
enum HostMatcherWire {
    Legacy(String),
    Tagged(TaggedHostMatcher),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TaggedHostMatcher {
    Exact { value: String },
    Suffix { value: String },
    Cidr { value: String },
}

impl<'de> Deserialize<'de> for HostMatcher {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match HostMatcherWire::deserialize(deserializer)? {
            HostMatcherWire::Legacy(value) => Ok(HostMatcher::Exact { value }),
            HostMatcherWire::Tagged(TaggedHostMatcher::Exact { value }) => Ok(HostMatcher::Exact { value }),
            HostMatcherWire::Tagged(TaggedHostMatcher::Suffix { value }) => Ok(HostMatcher::Suffix { value }),
            HostMatcherWire::Tagged(TaggedHostMatcher::Cidr { value }) => {
                HostMatcher::cidr(value).map_err(serde::de::Error::custom)
            }
        }
    }
}

/// SSRF validation error.
#[derive(Debug, thiserror::Error)]
pub enum SsrfError {
    /// URL denied by SSRF policy: private IP, metadata IP, etc.
    #[error("denied by SSRF policy: {reason}")]
    DeniedByPolicy {
        /// Stable category of the denial: `"loopback"`, `"private_network"`,
        /// `"link_local"`, `"unique_local"`, `"multicast"`, or `"unspecified"`.
        reason: &'static str,
    },

    /// Host not on allowlist when an allowlist is configured.
    #[error("host not on allowlist")]
    NotOnAllowlist,

    /// Allowlist entry is not a parseable CIDR block.
    #[error("invalid CIDR in SSRF allowlist: {0}")]
    InvalidCidr(String),

    /// DNS resolution failed for hostname.
    #[error("dns resolution failed: {0}")]
    DnsResolutionFailed(String),

    /// Invalid URL format.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// URL scheme not in allowlist (e.g., `ftp://` when only `http`/`https` allowed).
    #[error("disallowed scheme: {0}")]
    DisallowedScheme(String),

    /// Too many HTTP redirects encountered during validation.
    #[error("too many redirects")]
    TooManyRedirects,
}

/// SSRF policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsrfPolicy {
    /// If true, reject URLs that resolve to private/metadata IP ranges.
    #[serde(default = "default_deny_private")]
    pub deny_private: bool,

    /// Hostnames and IP ranges permitted regardless of `deny_private`.
    ///
    /// The allowlist is an *override* of `deny_private`, not an intersection with it.
    /// Precedence, in order:
    ///
    /// 1. `deny_private == false` permits everything; the allowlist is not consulted.
    /// 2. A hostname matching an `Exact` or `Suffix` entry is permitted immediately,
    ///    *before* DNS resolution — so the deny-list is never applied to it. This trusts
    ///    the host string: a name that resolves into private space is still permitted.
    /// 3. A literal or resolved IP inside a `Cidr` entry is permitted even though it is
    ///    in the default deny-list.
    /// 4. Otherwise the default deny-list decides.
    ///
    /// An empty allowlist therefore denies nothing by itself — it simply leaves
    /// `deny_private` and the deny-list in sole control.
    ///
    /// Skipped from language bindings pending the generator rollout; see
    /// <https://github.com/xberg-io/crawlberg/issues/37>.
    #[serde(default)]
    #[cfg_attr(alef, alef(skip))]
    pub allowlist: Vec<HostMatcher>,

    /// Maximum number of HTTP redirects to follow during validation.
    #[serde(default = "default_max_redirects")]
    pub max_redirects: u8,

    /// Allowed URI schemes. Default: `["http", "https"]`. Not serialized (set at runtime).
    /// Skipped from language bindings — `HashSet<&'static str>` is FFI-hostile.
    #[serde(skip, default = "default_scheme_allowlist")]
    #[cfg_attr(alef, alef(skip))]
    pub scheme_allowlist: HashSet<&'static str>,
}

fn default_deny_private() -> bool {
    // ~keep Deny by default; CrawlEngineBuilder applies the env override after JSON/binding construction.
    true
}

fn default_max_redirects() -> u8 {
    5
}

/// Default scheme allowlist used when `SsrfPolicy::scheme_allowlist` is omitted from JSON.
/// Without this, `#[serde(skip)]` would populate the field with `HashSet::default()` (empty)
/// on round-trip deserialize, causing every URL to fail with `disallowed scheme`.
///
/// Also called by `CrawlEngineBuilder::build` to normalize an empty allowlist that arrived
/// through a binding construction path (e.g. `Default::default()` in generated FFI glue).
pub(crate) fn default_scheme_allowlist() -> HashSet<&'static str> {
    let mut set = HashSet::new();
    set.insert("http");
    set.insert("https");
    set
}

impl Default for SsrfPolicy {
    fn default() -> Self {
        let mut scheme_allowlist = HashSet::new();
        scheme_allowlist.insert("http");
        scheme_allowlist.insert("https");

        Self {
            deny_private: true,
            allowlist: Vec::new(),
            max_redirects: 5,
            scheme_allowlist,
        }
    }
}

impl SsrfPolicy {
    /// Create a policy from environment variables.
    ///
    /// On native platforms, reads `CRAWLBERG_ALLOW_PRIVATE_NETWORK` — if set to "1" or "true"
    /// (case-insensitive), sets `deny_private = false`. Otherwise, defaults to `deny_private = true`.
    ///
    /// On wasm32 targets (browser/Node.js), environment variables are not accessible to the
    /// compiled module. Defaults to `deny_private = false` because:
    /// - Outbound requests in a browser go through the fetch API, which enforces its own network policies.
    /// - Rust-side SSRF checking is unenforceable and redundant in a wasm32 context.
    /// - For testing and localhost access, the host's network sandbox is the enforcing boundary.
    pub fn from_env() -> Self {
        #[cfg(target_arch = "wasm32")]
        let allow_private = true;

        #[cfg(not(target_arch = "wasm32"))]
        let allow_private = std::env::var("CRAWLBERG_ALLOW_PRIVATE_NETWORK")
            .map(|v| v.to_lowercase())
            .ok()
            .and_then(|v| {
                if v == "1" || v == "true" {
                    Some(true)
                } else if v == "0" || v == "false" {
                    Some(false)
                } else {
                    None
                }
            })
            .unwrap_or(false);

        let mut scheme_allowlist = std::collections::HashSet::new();
        scheme_allowlist.insert("http");
        scheme_allowlist.insert("https");

        Self {
            deny_private: !allow_private,
            allowlist: Vec::new(),
            max_redirects: 5,
            scheme_allowlist,
        }
    }
}

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
pub async fn validate_url(url: &url::Url, policy: &SsrfPolicy) -> Result<(), SsrfError> {
    let scheme = url.scheme();
    if !policy.scheme_allowlist.contains(scheme) {
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
fn is_ip_permitted(ip: IpAddr, policy: &SsrfPolicy) -> bool {
    if !policy.deny_private {
        return true;
    }

    if policy.allowlist.iter().any(|m| m.matches_ip(&ip)) {
        return true;
    }

    !DEFAULT_DENY_NETS.iter().any(|net| net.contains(&ip))
}

/// Classify a private IP into a category for error messaging.
fn classify_private_ip(ip: IpAddr) -> &'static str {
    match ip {
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
                0xfe80 => "link_local",
                0xfc00 | 0xfd00 => "unique_local",
                0xff00..=0xffff => "multicast",
                _ => "private_network",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = SsrfPolicy::default();
        assert!(policy.deny_private);
        assert!(policy.allowlist.is_empty());
        assert_eq!(policy.max_redirects, 5);
        assert!(policy.scheme_allowlist.contains("http"));
        assert!(policy.scheme_allowlist.contains("https"));
        assert!(!policy.scheme_allowlist.contains("ftp"));
    }

    #[test]
    fn test_host_matcher_exact() {
        let matcher = HostMatcher::exact("example.com");
        assert!(matcher.matches_host("example.com"));
        assert!(matcher.matches_host("EXAMPLE.COM"));
        assert!(!matcher.matches_host("api.example.com"));
    }

    #[test]
    fn test_host_matcher_suffix() {
        let matcher = HostMatcher::suffix(".example.com");
        assert!(matcher.matches_host("api.example.com"));
        assert!(matcher.matches_host("example.com"));
        assert!(matcher.matches_host("API.EXAMPLE.COM"));
        assert!(!matcher.matches_host("notexample.com"));

        let matcher_no_dot = HostMatcher::suffix("example.com");
        assert!(matcher_no_dot.matches_host("example.com"));
        assert!(matcher_no_dot.matches_host("api.example.com"));
        assert!(!matcher_no_dot.matches_host("notexample.com"));
    }

    #[test]
    fn test_host_matcher_cidr_matches_only_ips_in_range() {
        let matcher = HostMatcher::cidr("10.0.0.0/8").expect("literal CIDR is valid");
        assert!(
            matcher.matches_ip(&"10.5.6.7".parse::<IpAddr>().unwrap()),
            "10.5.6.7 is inside 10.0.0.0/8"
        );
        assert!(
            !matcher.matches_ip(&"11.0.0.1".parse::<IpAddr>().unwrap()),
            "11.0.0.1 is outside 10.0.0.0/8"
        );
        assert!(
            !matcher.matches_host("10.0.0.1"),
            "a CIDR matcher is IP-space only and must never match a host string"
        );
    }

    #[test]
    fn host_matcher_cidr_rejects_malformed_block() {
        // ~keep A silent non-match here would be an allowlist hole, not a no-op.
        let err = HostMatcher::cidr("10.0.0.0/99").expect_err("prefix length 99 is not valid");
        assert!(
            matches!(err, SsrfError::InvalidCidr(ref message) if message.contains("10.0.0.0/99")),
            "error must name the offending block, got {err:?}"
        );

        let err = HostMatcher::cidr("not-a-cidr").expect_err("garbage must not parse");
        assert!(
            matches!(err, SsrfError::InvalidCidr(_)),
            "expected InvalidCidr, got {err:?}"
        );
    }

    #[test]
    fn host_matcher_round_trips_through_tagged_json() {
        let cases = [
            (
                HostMatcher::exact("api.example.com"),
                r#"{"type":"exact","value":"api.example.com"}"#,
            ),
            (
                HostMatcher::suffix(".example.com"),
                r#"{"type":"suffix","value":".example.com"}"#,
            ),
            (
                HostMatcher::cidr("10.0.0.0/8").expect("literal CIDR is valid"),
                r#"{"type":"cidr","value":"10.0.0.0/8"}"#,
            ),
        ];

        for (matcher, expected_json) in cases {
            let encoded = serde_json::to_string(&matcher).expect("matcher must serialize");
            assert_eq!(encoded, expected_json, "unexpected wire form for {matcher:?}");

            let decoded: HostMatcher = serde_json::from_str(&encoded).expect("matcher must deserialize");
            assert_eq!(decoded, matcher, "round trip changed the matcher");
        }
    }

    #[test]
    fn host_matcher_accepts_legacy_bare_string_as_exact() {
        // ~keep The pre-tagged representation serialized every variant as a bare string.
        let decoded: HostMatcher = serde_json::from_str(r#""api.example.com""#).expect("legacy form must deserialize");
        assert_eq!(
            decoded,
            HostMatcher::exact("api.example.com"),
            "a bare string must resolve to Exact"
        );
    }

    #[test]
    fn host_matcher_deserialization_rejects_malformed_cidr() {
        let err = serde_json::from_str::<HostMatcher>(r#"{"type":"cidr","value":"10.0.0.0/99"}"#)
            .expect_err("malformed CIDR must not deserialize");
        assert!(
            err.to_string().contains("10.0.0.0/99"),
            "deserialization error must name the offending block, got: {err}"
        );
    }

    #[test]
    fn ssrf_policy_round_trips_a_populated_allowlist() {
        let mut policy = SsrfPolicy::default();
        policy.allowlist.push(HostMatcher::suffix(".internal.example.com"));
        policy
            .allowlist
            .push(HostMatcher::cidr("10.0.0.0/8").expect("literal CIDR is valid"));

        let encoded = serde_json::to_string(&policy).expect("policy must serialize");
        let decoded: SsrfPolicy = serde_json::from_str(&encoded).expect("policy must deserialize");

        assert_eq!(
            decoded.allowlist, policy.allowlist,
            "allowlist must survive a JSON round trip"
        );
    }

    #[test]
    fn test_default_policy_deny_private_true() {
        let policy = SsrfPolicy::default();
        assert!(policy.deny_private);
        assert_eq!(policy.max_redirects, 5);
    }

    #[test]
    fn test_default_policy_scheme_allowlist() {
        let policy = SsrfPolicy::default();
        assert!(policy.scheme_allowlist.contains("http"));
        assert!(policy.scheme_allowlist.contains("https"));
        assert!(!policy.scheme_allowlist.contains("ftp"));
    }

    #[test]
    fn test_classify_ipv4_loopback() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("127.0.0.1".parse().unwrap())),
            "loopback"
        );
    }

    #[test]
    fn test_classify_ipv4_private_10() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("10.0.0.1".parse().unwrap())),
            "private_network"
        );
    }

    #[test]
    fn test_classify_ipv4_private_172() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("172.16.0.1".parse().unwrap())),
            "private_network"
        );
    }

    #[test]
    fn test_classify_ipv4_private_192() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("192.168.0.1".parse().unwrap())),
            "private_network"
        );
    }

    #[test]
    fn test_classify_ipv4_link_local() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("169.254.1.1".parse().unwrap())),
            "link_local"
        );
    }

    #[test]
    fn test_classify_ipv6_loopback() {
        assert_eq!(classify_private_ip(IpAddr::V6("::1".parse().unwrap())), "loopback");
    }

    #[test]
    fn test_classify_ipv6_link_local() {
        assert_eq!(
            classify_private_ip(IpAddr::V6("fe80::1".parse().unwrap())),
            "link_local"
        );
    }

    #[test]
    fn test_classify_ipv6_unique_local() {
        assert_eq!(
            classify_private_ip(IpAddr::V6("fc00::1".parse().unwrap())),
            "unique_local"
        );
    }

    #[test]
    fn test_classify_ipv6_multicast() {
        assert_eq!(classify_private_ip(IpAddr::V6("ff00::1".parse().unwrap())), "multicast");
    }

    #[tokio::test]
    async fn validate_url_rejects_loopback_v4() {
        let policy = SsrfPolicy::default();
        let url = "http://127.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "loopback" }),
            "expected DeniedByPolicy loopback, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_private_10() {
        let policy = SsrfPolicy::default();
        let url = "http://10.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "expected DeniedByPolicy private_network, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_private_172() {
        let policy = SsrfPolicy::default();
        let url = "http://172.16.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "expected DeniedByPolicy private_network, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_private_192() {
        let policy = SsrfPolicy::default();
        let url = "http://192.168.1.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "expected DeniedByPolicy private_network, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_metadata_ip() {
        let policy = SsrfPolicy::default();
        let url = "http://169.254.169.254/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "link_local" }),
            "expected DeniedByPolicy link_local, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_unspecified() {
        let policy = SsrfPolicy::default();
        let url = "http://0.0.0.0/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "unspecified" }),
            "expected DeniedByPolicy unspecified, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_multicast() {
        let policy = SsrfPolicy::default();
        let url = "http://224.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "multicast" }),
            "expected DeniedByPolicy multicast, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_loopback() {
        let policy = SsrfPolicy::default();
        let url = "http://[::1]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "loopback" }),
            "expected DeniedByPolicy loopback, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_link_local() {
        let policy = SsrfPolicy::default();
        let url = "http://[fe80::1]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "link_local" }),
            "expected DeniedByPolicy link_local, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_ula() {
        let policy = SsrfPolicy::default();
        let url = "http://[fc00::1]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "unique_local" }),
            "expected DeniedByPolicy unique_local, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_multicast() {
        let policy = SsrfPolicy::default();
        let url = "http://[ff00::1]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "multicast" }),
            "expected DeniedByPolicy multicast, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_disallowed_scheme_ftp() {
        let policy = SsrfPolicy::default();
        let url = "ftp://example.com/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        match err {
            SsrfError::DisallowedScheme(s) => assert_eq!(s, "ftp"),
            other => panic!("expected DisallowedScheme(\"ftp\"), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_url_rejects_disallowed_scheme_file() {
        let policy = SsrfPolicy::default();
        let url = "file:///etc/passwd".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        match err {
            SsrfError::DisallowedScheme(s) => assert_eq!(s, "file"),
            other => panic!("expected DisallowedScheme(\"file\"), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_url_permits_public_ipv4() {
        let policy = SsrfPolicy::default();
        let url = "http://1.1.1.1/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("public IPv4 should be permitted");
    }

    #[tokio::test]
    async fn validate_url_permits_public_ipv6() {
        let policy = SsrfPolicy::default();
        let url = "http://[2606:4700:4700::1111]/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("public IPv6 should be permitted");
    }

    #[tokio::test]
    async fn validate_url_cidr_allowlist_permits_private() {
        let mut policy = SsrfPolicy::default();
        policy
            .allowlist
            .push(HostMatcher::cidr("10.0.0.0/8").expect("literal CIDR is valid"));
        let url = "http://10.5.6.7/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("10.0.0.0/8 in allowlist should permit 10.5.6.7");
    }

    #[tokio::test]
    async fn validate_url_exact_allowlist_does_not_match_literal_ip() {
        // ~keep Exact matchers are hostname-only; CIDR is required for literal IP allowlisting.
        let mut policy = SsrfPolicy::default();
        policy.allowlist.push(HostMatcher::exact("10.0.0.1"));
        let url = "http://10.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "Exact matcher must not bypass CIDR-based IP denial; got {err:?}"
        );
    }

    #[test]
    fn validate_url_suffix_no_leading_dot_does_not_match_substring() {
        let matcher = HostMatcher::suffix("example.com");
        assert!(
            !matcher.matches_host("notexample.com"),
            "Suffix(\"example.com\") must not match \"notexample.com\""
        );
    }

    #[tokio::test]
    async fn deny_private_false_permits_everything() {
        let policy = SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        };
        let url = "http://127.0.0.1/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("deny_private=false must permit loopback");
    }

    #[allow(unsafe_code)]
    #[tokio::test]
    #[serial_test::serial]
    async fn from_env_honors_crawlberg_allow_private_network() {
        // ~keep SAFETY: #[serial] prevents concurrent environment access in this test process.
        unsafe { std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true") };
        let policy = SsrfPolicy::from_env();
        unsafe { std::env::remove_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK") };
        let url = "http://10.0.0.1/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("CRAWLBERG_ALLOW_PRIVATE_NETWORK=true must permit private IPs");
    }

    #[allow(unsafe_code)]
    #[tokio::test]
    #[serial_test::serial]
    async fn from_env_default_denies() {
        // ~keep SAFETY: #[serial] prevents concurrent environment mutation in this test process.
        unsafe { std::env::remove_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK") };
        let policy = SsrfPolicy::from_env();
        let url = "http://10.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "default from_env policy must deny private IPs; got {err:?}"
        );
    }

    /// Regression test for rc.71: `CrawlConfig` JSON deserialization must honor
    /// `CRAWLBERG_ALLOW_PRIVATE_NETWORK`. Bindings that round-trip CrawlConfig
    /// through `cberg_crawl_config_from_json` (Go/Java/C#/PHP/Ruby/Elixir/Dart/
    /// Swift/Zig/WASM/C/brew) omit the alef-skipped `ssrf` field, so serde
    /// applies the field-level default. Prior to this fix the attribute was
    /// `#[serde(default)]` which called `<SsrfPolicy as Default>::default()`
    /// (deny_private: true) and silently ignored the env var.
    #[allow(unsafe_code)]
    #[test]
    #[serial_test::serial]
    fn crawl_config_json_deserialize_honors_env_var() {
        use crate::types::CrawlConfig;

        // ~keep SAFETY: #[serial] prevents concurrent environment mutation in this test process.
        unsafe { std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true") };
        let cfg: CrawlConfig = serde_json::from_str("{}").expect("empty object must deserialize");
        unsafe { std::env::remove_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK") };
        assert!(
            !cfg.ssrf.deny_private,
            "JSON `{{}}` with CRAWLBERG_ALLOW_PRIVATE_NETWORK=true must produce deny_private=false (got deny_private=true)",
        );

        let cfg_default_env: CrawlConfig = serde_json::from_str("{}").expect("empty object must deserialize");
        assert!(
            cfg_default_env.ssrf.deny_private,
            "JSON `{{}}` without env var must produce deny_private=true (got deny_private=false)",
        );
    }

    /// Regression test for rc.77: `SsrfPolicy` JSON deserialization must
    /// tolerate partial input. Bindings whose CrawlConfig DTO emits
    /// `"ssrf": {}` (e.g. Go's `Ssrf SsrfPolicy `json:"ssrf"`` with all
    /// fields `*pointer + omitempty`) previously failed with
    /// `missing field deny_private`. Field-level `#[serde(default)]` lets the
    /// policy fall back to safe defaults when fields are omitted.
    #[test]
    fn ssrf_policy_deserializes_from_empty_object() {
        let policy: SsrfPolicy = serde_json::from_str("{}").expect("empty object must deserialize");
        assert!(policy.deny_private, "deny_private must default to true");
        assert_eq!(policy.max_redirects, 5, "max_redirects must default to 5");
        assert!(policy.allowlist.is_empty(), "allowlist must default to empty");
        assert!(
            policy.scheme_allowlist.contains("http") && policy.scheme_allowlist.contains("https"),
            "scheme_allowlist must default to http/https"
        );
    }

    /// Companion to `ssrf_policy_deserializes_from_empty_object`: partial input
    /// where some fields are present and others omitted must keep provided
    /// values and default the rest.
    #[test]
    #[serial_test::serial]
    fn ssrf_policy_deserializes_from_partial_object() {
        let policy: SsrfPolicy =
            serde_json::from_str(r#"{"max_redirects": 3}"#).expect("partial object must deserialize");
        assert!(policy.deny_private, "deny_private must default to true");
        assert_eq!(policy.max_redirects, 3, "explicit max_redirects must be honored");

        let policy: SsrfPolicy =
            serde_json::from_str(r#"{"deny_private": false}"#).expect("partial object must deserialize");
        assert!(!policy.deny_private, "explicit deny_private must be honored");
        assert_eq!(policy.max_redirects, 5, "max_redirects must default to 5");
    }

    /// Regression test for the rolling rc.71 fix: `SsrfPolicy` JSON round-trip
    /// (serialize → deserialize) must preserve a populated `scheme_allowlist`.
    /// `#[serde(skip)]` alone would call `HashSet::default()` (empty) on
    /// deserialize, producing a policy that rejects EVERY URL with
    /// `DisallowedScheme(http)`. Reproduces the brew CLI failure mode where
    /// `merge_json_config` round-trips the live CrawlConfig and silently
    /// drops the scheme allowlist.
    #[test]
    fn ssrf_policy_json_round_trip_preserves_scheme_allowlist() {
        let policy = SsrfPolicy::default();
        let json = serde_json::to_string(&policy).expect("serialize");
        let restored: SsrfPolicy = serde_json::from_str(&json).expect("deserialize");
        assert!(
            restored.scheme_allowlist.contains("http"),
            "http scheme must survive JSON round-trip; got {:?}",
            restored.scheme_allowlist
        );
        assert!(
            restored.scheme_allowlist.contains("https"),
            "https scheme must survive JSON round-trip; got {:?}",
            restored.scheme_allowlist
        );
    }
}
