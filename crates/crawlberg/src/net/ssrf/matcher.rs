//! Allowlist matching primitives: [`HostMatcher`] and its memoized CIDR parser.

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{LazyLock, RwLock};

use super::SsrfError;

/// Content-addressed cache of parsed [`IpNet`] values, keyed by the source CIDR string.
///
/// `HostMatcher::Cidr`'s `value` field is a `String`, not an `IpNet`, because it is a
/// binding-generator source type: `crates/crawlberg-ffi` and friends cannot bind an
/// `IpNet` field, so the wire/enum shape must stay string-based. Without this cache,
/// [`HostMatcher::matches_ip`] would reparse the same CIDR string on every IP it is
/// asked about — once per resolved address, on every request an allowlisted CIDR could
/// apply to.
///
/// Keyed by string content rather than embedded in `HostMatcher` or `SsrfPolicy`
/// themselves, so cloning a policy or mutating its `allowlist` in place (both public
/// operations — `SsrfPolicy::allowlist` is a plain `pub Vec<HostMatcher>`) can never
/// desynchronize a matcher from a stale, positionally-cached entry.
type CidrParseResult = Result<IpNet, String>;
type CidrParseCache = RwLock<HashMap<Box<str>, CidrParseResult>>;

pub(super) static CIDR_PARSE_CACHE: LazyLock<CidrParseCache> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Parse `cidr`, memoizing the result — success or failure — in [`CIDR_PARSE_CACHE`] so
/// repeated calls with the same string never re-invoke [`IpNet`]'s parser.
fn parse_cidr_cached(cidr: &str) -> CidrParseResult {
    if let Some(cached) = CIDR_PARSE_CACHE.read().expect("CIDR_PARSE_CACHE poisoned").get(cidr) {
        return cached.clone();
    }
    let parsed = cidr.parse::<IpNet>().map_err(|e| e.to_string());
    CIDR_PARSE_CACHE
        .write()
        .expect("CIDR_PARSE_CACHE poisoned")
        .insert(cidr.into(), parsed.clone());
    parsed
}

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
        match parse_cidr_cached(&value) {
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
            HostMatcher::Cidr { value } => match parse_cidr_cached(value) {
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
