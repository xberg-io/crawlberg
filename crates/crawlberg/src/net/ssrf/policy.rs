//! [`SsrfPolicy`]: the configurable half of SSRF validation.

use serde::{Deserialize, Serialize};

use super::HostMatcher;

/// SSRF policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsrfPolicy {
    /// If true, reject URLs that resolve to private/metadata IP ranges.
    #[serde(default = "default_deny_private")]
    pub deny_private: bool,

    /// Hostnames and IP ranges permitted regardless of `deny_private`, unless a matching
    /// address is in `denylist`.
    ///
    /// The allowlist is an *override* of `deny_private`, not an intersection with it.
    /// Precedence, in order:
    ///
    /// 1. A configured `denylist` CIDR always refuses a matching address.
    /// 2. `deny_private == false` permits addresses outside `denylist`; the allowlist is not consulted.
    /// 3. A hostname matching an `Exact` or `Suffix` entry is permitted before applying
    ///    the built-in deny-list. When `denylist` is non-empty, it is still resolved so
    ///    custom network denials can be enforced.
    /// 4. A literal or resolved IP inside a `Cidr` entry is permitted even though it is
    ///    in the default deny-list.
    /// 5. Otherwise the default deny-list decides.
    ///
    /// An empty allowlist therefore denies nothing by itself — it simply leaves
    /// `deny_private` and the deny-list in sole control.
    #[serde(default)]
    pub allowlist: Vec<HostMatcher>,

    /// IP ranges refused regardless of `deny_private` and `allowlist`.
    ///
    /// ~keep Only [`HostMatcher::Cidr`] entries are valid. Configured denials are checked
    /// before permissive settings, including against IPv4 addresses embedded in IPv6 and,
    /// on native targets, every address returned by DNS. They extend the built-in deny-list
    /// and cannot weaken it. A hostname is refused when an upstream proxy or remote browser
    /// performs the connection lookup, because that lookup cannot be bound to these checks.
    #[serde(default)]
    pub denylist: Vec<HostMatcher>,

    /// Maximum number of HTTP redirects to follow during validation.
    #[serde(default = "default_max_redirects")]
    pub max_redirects: u8,

    /// Allowed URI schemes. Default: `["http", "https"]`.
    ///
    /// Only `http` and `https` are supported. An empty list denies every URL.
    #[serde(
        default = "default_scheme_allowlist",
        skip_serializing_if = "is_default_scheme_allowlist"
    )]
    pub scheme_allowlist: Vec<String>,
}

fn default_deny_private() -> bool {
    // ~keep Deny by default; CrawlEngineBuilder applies the env override after JSON/binding construction.
    true
}

fn default_max_redirects() -> u8 {
    5
}

fn default_scheme_allowlist() -> Vec<String> {
    vec!["http".to_owned(), "https".to_owned()]
}

fn is_default_scheme_allowlist(schemes: &[String]) -> bool {
    schemes == default_scheme_allowlist()
}

pub(super) fn is_supported_scheme(scheme: &str) -> bool {
    scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
}

impl Default for SsrfPolicy {
    fn default() -> Self {
        Self {
            deny_private: true,
            allowlist: Vec::new(),
            denylist: Vec::new(),
            max_redirects: 5,
            scheme_allowlist: default_scheme_allowlist(),
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
    ///
    /// **Node.js caveat:** `deny_private` and `denylist` have no effect on hostname-based requests
    /// under `wasm32`. There is no DNS resolution on this target, so [`validate_url`] only ever
    /// checks a literal IP host; a domain name falls straight through to `Ok(())`. In a browser
    /// this is covered by same-origin/CORS. Node's `fetch` enforces no CORS, so a Node service
    /// embedding this wasm module can be driven to internal hosts by domain name despite the
    /// policy. Enforce egress restrictions (network policy, firewall, proxy allowlist) outside
    /// the process.
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

        Self {
            deny_private: !allow_private,
            allowlist: Vec::new(),
            denylist: Vec::new(),
            max_redirects: 5,
            scheme_allowlist: default_scheme_allowlist(),
        }
    }

    pub(crate) fn validate_scheme_allowlist(&self) -> Result<(), String> {
        for (index, scheme) in self.scheme_allowlist.iter().enumerate() {
            if !is_supported_scheme(scheme) {
                return Err(format!(
                    "ssrf.scheme_allowlist contains unsupported scheme '{scheme}' (expected http or https)"
                ));
            }
            if self.scheme_allowlist[..index]
                .iter()
                .any(|previous| previous.eq_ignore_ascii_case(scheme))
            {
                return Err(format!("ssrf.scheme_allowlist contains duplicate scheme '{scheme}'"));
            }
        }
        Ok(())
    }

    pub(crate) fn validate_denylist(&self) -> Result<(), String> {
        for matcher in &self.denylist {
            match matcher {
                HostMatcher::Cidr { value } => {
                    HostMatcher::cidr(value.clone())
                        .map_err(|_| format!("ssrf.denylist contains invalid CIDR '{value}'"))?;
                }
                HostMatcher::Exact { .. } | HostMatcher::Suffix { .. } => {
                    return Err("ssrf.denylist entries must be CIDR matchers".to_owned());
                }
            }
        }
        Ok(())
    }

    /// ~keep Whether IP-level enforcement must remain active for this policy.
    pub(crate) fn enforces_ip_denials(&self) -> bool {
        self.deny_private || !self.denylist.is_empty()
    }
}
