//! Request and response types flowing through the Tower service stack.

use std::collections::HashMap;

#[cfg(not(target_arch = "wasm32"))]
use url::Url;

/// HTTP request flowing through the Tower service stack.
///
/// Not available on `wasm32` targets — the Tower stack is native-only.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct CrawlRequest {
    pub url: String,
    pub headers: HashMap<String, String>,
    /// Dispatch tier that initiated this request — used by `CrawlTracingLayer`
    /// to record `crawl.tier` on the `crawl.page.fetch` span without having to
    /// thread the value through a separate channel.  `None` for direct (non-dispatch)
    /// calls that bypass the tier loop.
    pub tier: Option<&'static str>,
    /// Host of the URL that started this redirect chain, when this request is a
    /// later hop in one. `None` means the request *is* the origin.
    ///
    /// ~keep Redirects are followed manually under `Policy::none()`, so reqwest never
    /// ~keep applies its own cross-host credential stripping. This field is what lets
    /// ~keep `apply_headers` withhold configured credentials once a chain leaves its origin.
    pub origin_host: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl CrawlRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: HashMap::new(),
            tier: None,
            origin_host: None,
        }
    }

    /// Mark this request as a redirect hop originating from `origin_host`.
    ///
    /// Configured credentials are only sent when the hop is still on that host.
    pub fn with_origin_host(mut self, origin_host: Option<String>) -> Self {
        self.origin_host = origin_host;
        self
    }

    pub fn domain(&self) -> Option<String> {
        Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_owned()))
    }

    /// Whether this request is still on the host that started its redirect chain.
    ///
    /// `true` when no origin was recorded (the request *is* the origin). An
    /// unparseable URL or one with no host returns `false`, so credentials are
    /// withheld rather than sent to something we could not identify.
    pub fn is_on_origin_host(&self) -> bool {
        let Some(ref origin) = self.origin_host else {
            return true;
        };
        Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(|host| host.eq_ignore_ascii_case(origin)))
            .unwrap_or(false)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn a_request_with_no_recorded_origin_is_treated_as_the_origin() {
        assert!(CrawlRequest::new("https://example.com/a").is_on_origin_host());
    }

    #[test]
    fn a_hop_on_the_origin_host_keeps_credentials() {
        let req = CrawlRequest::new("https://example.com/b").with_origin_host(Some("example.com".to_owned()));
        assert!(req.is_on_origin_host());
    }

    #[test]
    fn a_hop_to_a_different_host_withholds_credentials() {
        let req = CrawlRequest::new("https://attacker.test/collect").with_origin_host(Some("example.com".to_owned()));
        assert!(!req.is_on_origin_host());
    }

    #[test]
    fn a_hop_to_a_subdomain_of_the_origin_withholds_credentials() {
        let req =
            CrawlRequest::new("https://evil.example.com/collect").with_origin_host(Some("example.com".to_owned()));
        assert!(!req.is_on_origin_host());
    }

    #[test]
    fn a_hop_to_a_host_that_merely_starts_with_the_origin_withholds_credentials() {
        let req = CrawlRequest::new("https://example.com.attacker.test/collect")
            .with_origin_host(Some("example.com".to_owned()));
        assert!(!req.is_on_origin_host());
    }

    #[test]
    fn host_comparison_ignores_case() {
        let req = CrawlRequest::new("https://EXAMPLE.com/b").with_origin_host(Some("example.com".to_owned()));
        assert!(req.is_on_origin_host());
    }

    #[test]
    fn an_unparseable_hop_url_withholds_credentials() {
        let req = CrawlRequest::new("not a url").with_origin_host(Some("example.com".to_owned()));
        assert!(!req.is_on_origin_host());
    }
}

/// HTTP response from the Tower service stack.
#[derive(Debug, Clone)]
pub struct CrawlResponse {
    pub status: u16,
    pub content_type: String,
    pub body: String,
    pub body_bytes: Vec<u8>,
    pub headers: HashMap<String, Vec<String>>,
}
