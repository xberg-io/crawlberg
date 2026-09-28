//! Network utilities: SSRF policy, validation, and security.

#[cfg(feature = "browser-native")]
pub(crate) mod browser_policy;
// ~keep `reqwest::cookie` (and thus PolicyCookieStore's `Jar` wrapper) only exists under
// reqwest's hyper backend; wasm32 uses the browser's own fetch/cookie handling instead.
#[cfg(not(target_arch = "wasm32"))]
pub mod cookie;
pub(crate) mod origin;
pub mod redact;
// ~keep `reqwest::dns::Resolve` only exists under reqwest's hyper backend; wasm32 has no
// DNS surface at all (see the wasm32 note on `ssrf::validate_url`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod resolver;
pub mod ssrf;

pub use redact::redact_url_credentials;
pub use ssrf::{HostMatcher, SsrfError, SsrfPolicy, validate_url};

/// How many refusals of one browser page or session are logged one by one. The rest are
/// counted, and one warning reports the count when the page or session ends.
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
pub(crate) const LOGGED_REFUSALS: usize = 5;
