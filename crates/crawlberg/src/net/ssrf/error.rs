//! The error type returned by SSRF validation.

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
