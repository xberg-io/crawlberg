//! Network utilities: SSRF policy, validation, and security.

#[cfg(feature = "browser-native")]
pub(crate) mod browser_policy;
pub mod ssrf;

pub use ssrf::{HostMatcher, SsrfError, SsrfPolicy, validate_url};
