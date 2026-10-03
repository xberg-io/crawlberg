use std::sync::Arc;

#[cfg(feature = "stealth")]
use crate::net::StealthHttpClient;
use crate::net::proxy::UpstreamProxy;
use crate::net::ssrf::{DefaultSsrfValidator, SsrfValidator};
use crate::net::{CookieJar, HttpClient, NetError, RobotsCache};

pub struct BrowserContext {
    pub id: String,
    pub cookie_jar: Arc<CookieJar>,
    pub http_client: Arc<HttpClient>,
    pub user_agent: String,
    pub proxy: Option<UpstreamProxy>,
    pub robots_cache: Arc<RobotsCache>,
    pub obey_robots: bool,
    pub stealth: bool,
    /// When true, CDP-driven navigation to file:// URLs is permitted.
    /// Default is false: a remote CDP client cannot point the browser
    /// at /etc/shadow even if the native browser is running as a privileged
    /// user. The direct Crawlberg adapter leaves this disabled.
    pub allow_file_access: bool,
    /// The Chrome-fingerprinted client, built with the context when `stealth` is set.
    #[cfg(feature = "stealth")]
    pub stealth_client: Option<Arc<StealthHttpClient>>,
}

impl BrowserContext {
    pub fn new(id: String) -> Self {
        let cookie_jar = Arc::new(CookieJar::new());
        let http_client = Arc::new(HttpClient::with_cookie_jar(cookie_jar.clone()));
        BrowserContext {
            id,
            cookie_jar,
            http_client,
            user_agent:
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
                    .to_string(),
            proxy: None,
            robots_cache: Arc::new(RobotsCache::new()),
            obey_robots: false,
            stealth: false,
            allow_file_access: false,
            #[cfg(feature = "stealth")]
            stealth_client: None,
        }
    }

    pub fn with_options(id: String, proxy: Option<UpstreamProxy>, stealth: bool) -> Result<Self, NetError> {
        Self::with_full_options(id, proxy, stealth, None)
    }

    pub fn with_full_options(
        id: String,
        proxy: Option<UpstreamProxy>,
        stealth: bool,
        user_agent: Option<String>,
    ) -> Result<Self, NetError> {
        Self::with_ssrf(
            id,
            proxy,
            stealth,
            user_agent,
            Arc::new(DefaultSsrfValidator::from_env()),
            false,
        )
    }

    /// Build a context whose HTTP client, stealth client and JS realm all share `ssrf`.
    ///
    /// Fails with [`NetError::InvalidProxy`] when `proxy` cannot be used, so no request
    /// from this context can connect directly in its place.
    pub fn with_ssrf(
        id: String,
        proxy: Option<UpstreamProxy>,
        stealth: bool,
        user_agent: Option<String>,
        ssrf: Arc<dyn SsrfValidator>,
        allow_file_access: bool,
    ) -> Result<Self, NetError> {
        Self::with_ssrf_and_cookie_jar(
            id,
            proxy,
            stealth,
            user_agent,
            ssrf,
            allow_file_access,
            Arc::new(CookieJar::new()),
        )
    }

    pub(crate) fn with_ssrf_and_cookie_jar(
        id: String,
        proxy: Option<UpstreamProxy>,
        stealth: bool,
        user_agent: Option<String>,
        ssrf: Arc<dyn SsrfValidator>,
        allow_file_access: bool,
        cookie_jar: Arc<CookieJar>,
    ) -> Result<Self, NetError> {
        let client = HttpClient::with_ssrf(cookie_jar.clone(), proxy.as_ref(), ssrf, allow_file_access)?;
        // ~keep Share the plain client's SSRF policy: the stealth path is an
        // alternate transport, not an alternate policy.
        #[cfg(feature = "stealth")]
        let stealth_client = if stealth {
            Some(Arc::new(StealthHttpClient::with_ssrf(
                cookie_jar.clone(),
                proxy.as_ref(),
                client.ssrf.clone(),
            )?))
        } else {
            None
        };
        let resolved_ua = user_agent.unwrap_or_else(|| {
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
                .to_string()
        });
        // ~keep Set the HTTP user agent before async setup; no other task holds this lock during construction.
        if let Ok(mut guard) = client.user_agent.try_write() {
            *guard = resolved_ua.clone();
        }
        let http_client = Arc::new(client);
        Ok(BrowserContext {
            id,
            cookie_jar,
            http_client,
            user_agent: resolved_ua,
            proxy,
            robots_cache: Arc::new(RobotsCache::new()),
            obey_robots: false,
            stealth,
            allow_file_access,
            #[cfg(feature = "stealth")]
            stealth_client,
        })
    }

    pub fn with_proxy(id: String, proxy: Option<UpstreamProxy>) -> Result<Self, NetError> {
        Self::with_options(id, proxy, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn with_full_options_propagates_user_agent_to_http_client() {
        let ctx = BrowserContext::with_full_options("test".to_string(), None, false, Some("Custom-UA/1.0".to_string()))
            .expect("no proxy, so the context must build");
        assert_eq!(ctx.user_agent, "Custom-UA/1.0");
        let client_ua = ctx.http_client.user_agent.read().await.clone();
        assert_eq!(client_ua, "Custom-UA/1.0");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn with_full_options_falls_back_to_chrome_default() {
        let ctx = BrowserContext::with_full_options("test".to_string(), None, false, None)
            .expect("no proxy, so the context must build");
        assert!(ctx.user_agent.contains("Chrome"));
        let client_ua = ctx.http_client.user_agent.read().await.clone();
        assert!(client_ua.contains("Chrome"));
        assert_eq!(ctx.user_agent, client_ua);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn with_options_keeps_default_user_agent() {
        let ctx =
            BrowserContext::with_options("test".to_string(), None, false).expect("no proxy, so the context must build");
        assert!(ctx.user_agent.contains("Chrome"));
    }

    #[test]
    fn a_proxy_the_clients_cannot_use_fails_context_construction() {
        for stealth in [false, true] {
            // ~keep A context takes its proxy as an `UpstreamProxy`, so a socks5 proxy is refused
            // ~keep before a context that would connect directly can be built.
            let result = crate::net::proxy::test_proxy("socks5://proxy.test:1080")
                .map(|proxy| BrowserContext::with_options("test".to_string(), Some(proxy), stealth));
            assert!(
                matches!(
                    result,
                    Err(crate::net::proxy::ProxyError::UnsupportedScheme(ref scheme)) if scheme == "socks5"
                ),
                "stealth={stealth}: a socks5 proxy must refuse the context, not build one that connects directly"
            );
        }
    }
}
