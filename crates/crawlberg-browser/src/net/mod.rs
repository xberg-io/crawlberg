pub mod client;
pub mod cookies;
pub mod credential;
pub mod interceptor;
pub mod resolver;
pub mod robots;
pub mod ssrf;
#[cfg(feature = "stealth")]
pub mod wreq_client;

pub use client::{HttpClient, NetError, Response};

/// `error` followed by each of its causes, joined with ": ".
///
/// reqwest shows only "error sending request" at the top; a refusal from the DNS resolver, such
/// as the SSRF policy's reason, is in the causes.
pub(crate) fn error_with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(current) = cause {
        text.push_str(": ");
        text.push_str(&current.to_string());
        cause = current.source();
    }
    text
}
pub use cookies::CookieJar;
pub use credential::OriginHeaders;
pub use robots::RobotsCache;
#[cfg(feature = "stealth")]
pub use wreq_client::{STEALTH_USER_AGENT, StealthHttpClient};
