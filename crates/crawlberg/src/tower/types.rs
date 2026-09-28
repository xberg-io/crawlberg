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
}

#[cfg(not(target_arch = "wasm32"))]
impl CrawlRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: HashMap::new(),
            tier: None,
        }
    }

    pub fn domain(&self) -> Option<String> {
        Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_owned()))
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
    /// Where the content came from, when the fetcher followed redirects itself (the
    /// browser tier). `None` when the response belongs to the requested URL.
    ///
    /// ~keep Read by the native redirect chain and by the page results; wasm has no browser
    /// ~keep tier to set it.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub landed: Option<Landing>,
}

/// The URL a self-redirecting fetcher landed on, and the HTTP redirects it followed.
///
/// ~keep Only the browser tier builds one, so without the `browser` feature nothing does.
#[derive(Debug, Clone)]
#[cfg_attr(any(target_arch = "wasm32", not(feature = "browser")), allow(dead_code))]
pub struct Landing {
    pub url: String,
    pub redirects: usize,
    /// The URLs the browser's SSRF check refused for requests the page sent, credential-redacted.
    pub refused: Vec<String>,
}
