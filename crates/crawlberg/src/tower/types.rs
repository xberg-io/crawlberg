//! Request and response types flowing through the Tower service stack.

use std::collections::HashMap;

#[cfg(not(target_arch = "wasm32"))]
use url::Url;

/// HTTP request flowing through the Tower service stack.
///
/// Not available on `wasm32` targets — the Tower stack is native-only.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
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
impl std::fmt::Debug for CrawlRequest {
    /// Redacted: `headers` can hold an `Authorization` or cookie header set on this request.
    /// Header names stay visible; sensitive values print as `***`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            url,
            headers,
            tier,
            origin_host,
        } = self;
        f.debug_struct("CrawlRequest")
            .field("url", url)
            .field("headers", &crate::net::redact::RedactedHeaders(headers))
            .field("tier", tier)
            .field("origin_host", origin_host)
            .finish()
    }
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
    fn request_and_response_debug_hide_sensitive_header_values() {
        const SECRET: &str = "sk-live-9f8e7d6c5b4a";
        let mut request = CrawlRequest::new("https://example.com/");
        request.headers = HashMap::from([
            ("authorization".to_owned(), format!("Bearer {SECRET}")),
            ("Cookie".to_owned(), format!("sid={SECRET}")),
            ("accept".to_owned(), "text/html".to_owned()),
        ]);
        let response = CrawlResponse {
            status: 200,
            content_type: "text/html".into(),
            body: String::new(),
            body_bytes: Vec::new(),
            headers: HashMap::from([
                ("set-cookie".to_owned(), vec![format!("sid={SECRET}")]),
                ("proxy-authorization".to_owned(), vec![format!("Basic {SECRET}")]),
                ("server".to_owned(), vec!["nginx".to_owned()]),
            ]),
        };
        for text in [format!("{request:?}"), format!("{response:#?}")] {
            assert!(!text.contains(SECRET), "secret printed: {text}");
            assert!(text.contains("***"), "placeholder missing: {text}");
        }
        assert!(format!("{request:?}").contains("text/html"));
        assert!(format!("{response:?}").contains("nginx"));
    }

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
#[derive(Clone)]
pub struct CrawlResponse {
    pub status: u16,
    pub content_type: String,
    pub body: String,
    pub body_bytes: Vec<u8>,
    pub headers: HashMap<String, Vec<String>>,
}

impl std::fmt::Debug for CrawlResponse {
    /// Redacted: `headers` can carry `Set-Cookie`. Header names stay visible; sensitive
    /// values print as `***`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            status,
            content_type,
            body,
            body_bytes,
            headers,
        } = self;
        f.debug_struct("CrawlResponse")
            .field("status", status)
            .field("content_type", content_type)
            .field("body", body)
            .field("body_bytes", body_bytes)
            .field("headers", &crate::net::redact::RedactedHeaders(headers))
            .finish()
    }
}
