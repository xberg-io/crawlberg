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
    /// The URL the content came from, when the fetcher followed redirects itself (the
    /// browser tier). `None` when the response belongs to the requested URL.
    ///
    /// ~keep Read only by the native redirect chain; wasm has no browser tier to set it.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub landed_url: Option<String>,
    /// The `User-Agent` header value this response's request actually sent, when known.
    ///
    /// ~keep Only the native HTTP tier (`tower::service::do_fetch`) sets this, because it is
    /// the only tier the UA rotation layer reaches. `None` means the caller should fall back
    /// to the configured default -- true for the browser tier, wasm, the bypass tier, and any
    /// synthetic response, none of which rotate. Robots decisions (group selection, meta and
    /// header directive matching) must read this instead of recomputing the engine's default,
    /// or they judge a page against an agent a rotating crawl never sent (crawlberg#423).
    pub sent_user_agent: Option<String>,
}
