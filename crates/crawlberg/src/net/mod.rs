//! Network utilities: SSRF policy, validation, and security.

#[cfg(feature = "browser-native")]
pub(crate) mod browser_policy;
// ~keep `reqwest::cookie` (and thus PolicyCookieStore's `Jar` wrapper) only exists under
// reqwest's hyper backend; wasm32 uses the browser's own fetch/cookie handling instead.
#[cfg(not(target_arch = "wasm32"))]
pub mod cookie;
pub(crate) mod origin;
pub mod redact;
pub mod ssrf;

pub use redact::redact_url_credentials;
pub use ssrf::{HostMatcher, SsrfError, SsrfPolicy, validate_url};
