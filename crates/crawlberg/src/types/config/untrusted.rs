//! Trust-boundary helpers for server applications accepting caller-supplied configuration.

use serde_json::Value;

use super::CrawlConfig;
use crate::error::CrawlError;

/// JSON pointers, relative to a [`CrawlConfig`], that an untrusted caller may not set.
///
/// Inspect the raw JSON with [`reject_untrusted_fields`] before deserializing it, then call
/// [`CrawlConfig::adopt_operator_egress`] after deserialization so the server-owned policy is
/// authoritative even when a caller config came from a non-JSON binding.
#[cfg_attr(alef, alef(skip))]
pub const UNTRUSTED_CALLER_FORBIDDEN_FIELDS: &[&str] = &[
    "/ssrf",
    "/ssrf_deny_private_explicit",
    "/max_redirects",
    "/proxy",
    "/browser/proxy",
    "/browser/endpoint",
    "/browser/chrome_path",
    "/browser/chrome_args",
    "/browser/eval_script",
    "/browser/session_affinity",
    "/browser_profile",
    "/save_browser_profile",
    "/document_output_dir",
    "/warc_output",
];

/// Reject a raw caller JSON value that sets an operator-owned field to a non-null value.
///
/// The returned error identifies the forbidden JSON pointer but never formats its value, since
/// proxy URLs, browser endpoints, scripts, Chrome arguments, and filesystem paths can contain secrets.
#[cfg_attr(alef, alef(skip))]
pub fn reject_untrusted_fields(value: &Value) -> Result<(), CrawlError> {
    for &pointer in UNTRUSTED_CALLER_FORBIDDEN_FIELDS {
        if value.pointer(pointer).is_some_and(|field| !field.is_null()) {
            return Err(CrawlError::invalid_config(format!(
                "untrusted caller may not set {pointer}"
            )));
        }
    }
    Ok(())
}

impl CrawlConfig {
    /// Replace every egress, browser-launch, and filesystem-output setting with operator policy.
    ///
    /// Caller-owned request credentials (`auth` and `custom_headers`) are intentionally retained:
    /// they authenticate to the selected target but do not select a target, proxy, executable, or
    /// output path. Call this after deserializing caller input and before constructing an engine.
    #[cfg_attr(alef, alef(skip))]
    pub fn adopt_operator_egress(&mut self, operator: &Self) {
        self.ssrf = operator.ssrf.clone();
        self.ssrf_deny_private_explicit = operator.ssrf_deny_private_explicit;
        self.max_redirects = operator.max_redirects;
        self.proxy = operator.proxy.clone();
        self.browser.proxy = operator.browser.proxy.clone();
        self.browser.endpoint = operator.browser.endpoint.clone();
        self.browser.chrome_path = operator.browser.chrome_path.clone();
        self.browser.chrome_args = operator.browser.chrome_args.clone();
        self.browser.eval_script = operator.browser.eval_script.clone();
        self.browser.session_affinity = operator.browser.session_affinity;
        self.browser_profile = operator.browser_profile.clone();
        self.save_browser_profile = operator.save_browser_profile;
        self.document_output_dir = operator.document_output_dir.clone();
        self.warc_output = operator.warc_output.clone();
        self.dispatch = operator.dispatch.clone();
        self.proxy_provider = operator.proxy_provider.clone();
        #[cfg(feature = "browser")]
        {
            self.browser_pool = operator.browser_pool.clone();
            self.browser_session_pool = operator.browser_session_pool.clone();
        }
    }
}

#[cfg(test)]
#[path = "untrusted_tests.rs"]
mod tests;
