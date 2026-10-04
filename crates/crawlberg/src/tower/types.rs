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
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for CrawlRequest {
    /// Redacted: every header value is hidden, not only the credential denylist's. The engine
    /// sends `CrawlConfig.custom_headers` on the HTTP client directly, so nothing fills this
    /// map today; a caller who fills it can put an API key under any name the vendor asks for
    /// (`X-Api-Key`, `apikey`, ...), which no name denylist covers. Names stay visible.
    /// Response headers keep the denylist, where `content-type` and `server` are the
    /// debugging value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { url, headers, tier } = self;
        f.debug_struct("CrawlRequest")
            .field("url", url)
            .field("headers", &crate::net::redact::RedactedValues(headers))
            .field("tier", tier)
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
        }
    }

    pub fn domain(&self) -> Option<String> {
        Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_owned()))
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
            landed: None,
            sent_user_agent: None,
            soft_error: false,
        };
        for text in [format!("{request:?}"), format!("{response:#?}")] {
            assert!(!text.contains(SECRET), "secret printed: {text}");
            assert!(text.contains("***"), "placeholder missing: {text}");
        }
        // ~keep A request header's value is hidden whatever its name is, so `accept` does not
        // ~keep keep its value here; only the name stays. A response header keeps a
        // ~keep non-sensitive value, which is why `server: nginx` is still readable.
        let request_debug = format!("{request:?}");
        assert!(
            !request_debug.contains("text/html"),
            "no request header value may print: {request_debug}"
        );
        assert!(
            request_debug.contains("accept"),
            "header names must stay: {request_debug}"
        );
        assert!(format!("{response:?}").contains("nginx"));
    }

    /// The scenario that made the name denylist and `CrawlConfig`'s hide-everything rule
    /// disagree on the same data: a vendor key under a name no denylist can predict.
    #[test]
    fn a_request_header_with_an_unguessable_credential_name_is_hidden() {
        const SECRET: &str = "sk-live-9f8e7d6c5b4a";
        let mut request = CrawlRequest::new("https://example.com/");
        request.headers = HashMap::from([("X-Api-Key".to_owned(), SECRET.to_owned())]);

        let config = crate::CrawlConfig {
            custom_headers: HashMap::from([("X-Api-Key".to_owned(), SECRET.to_owned())]),
            ..crate::CrawlConfig::default()
        };

        for text in [format!("{request:?}"), format!("{config:?}")] {
            assert!(!text.contains(SECRET), "the vendor key printed: {text}");
            assert!(
                text.contains(r#""X-Api-Key": "***""#),
                "the name must stay and the value must be the placeholder: {text}"
            );
        }
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
    /// Where the content came from, when the fetcher followed redirects itself (the
    /// browser tier). `None` when the response belongs to the requested URL.
    ///
    /// ~keep Read by the native redirect chain and by the page results; wasm has no browser
    /// ~keep tier to set it. Boxed because only the browser tier sets it, so the other tiers'
    /// ~keep responses do not carry its size.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub landed: Option<Box<Landing>>,
    /// The `User-Agent` header value this response's request actually sent, when known.
    ///
    /// ~keep Only the native HTTP tier (`tower::service::do_fetch`) sets this, because it is
    /// the only tier the UA rotation layer reaches. `None` means the caller should fall back
    /// to the configured default -- true for the browser tier, wasm, the bypass tier, and any
    /// synthetic response, none of which rotate. Robots decisions (group selection, meta and
    /// header directive matching) must read this instead of recomputing the engine's default,
    /// or they judge a page against an agent a rotating crawl never sent (crawlberg#423).
    pub sent_user_agent: Option<String>,
    /// Whether `soft_http_errors` built this response in place of an error. Only the engine's
    /// soft error path sets it, so a scrape can tell its page from a real response with the same status.
    pub soft_error: bool,
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
            landed,
            sent_user_agent,
            soft_error,
        } = self;
        f.debug_struct("CrawlResponse")
            .field("status", status)
            .field("content_type", content_type)
            .field("body", body)
            .field("body_bytes", body_bytes)
            .field("headers", &crate::net::redact::RedactedHeaders(headers))
            .field("landed", landed)
            .field("sent_user_agent", sent_user_agent)
            .field("soft_error", soft_error)
            .finish()
    }
}

/// The URL a self-redirecting fetcher landed on, and the redirects it followed: HTTP redirects,
/// and the navigations the page started, one each.
///
/// ~keep Only a browser tier builds one, so without a browser feature nothing does.
#[derive(Debug, Clone)]
#[cfg_attr(
    any(target_arch = "wasm32", not(any(feature = "browser", feature = "browser-native"))),
    allow(dead_code)
)]
pub struct Landing {
    pub url: String,
    pub redirects: usize,
    /// The URLs the browser's SSRF check refused for requests the page sent, credential-redacted.
    pub refused: Vec<String>,
    /// The eval result, network events and cookies of the page. Only a scrape on the native
    /// backend carries them.
    pub extras: Option<crate::http::BrowserExtras>,
    /// Cookies retained only long enough to seed the next Chromiumoxide redirect hop. ~keep
    pub(crate) cookies: Vec<crate::types::BrowserCookie>,
}
