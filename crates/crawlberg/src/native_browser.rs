//! Native browser backend adapter — standalone module so it can be used both
//! when only `browser-native` is active and when the full `browser` feature is on.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Duration;

use crawlberg_browser::adapter::{NativeBrowserExecutor, NativeCookie as NBCookie, ProxyCredentials, UpstreamProxy};
use tracing::Instrument as _;

use crate::error::CrawlError;
use crate::http::{BrowserExtras, HttpResponse};
use crate::telemetry::attributes::{CRAWL_BROWSER_BACKEND, CRAWL_BROWSER_SESSION_ID, CRAWL_PAGES_RENDERED};
use crate::telemetry::metrics::registry;
use crate::types::{BrowserWait, CookieInfo, CrawlConfig, ResponseMeta};

/// Process-wide monotonic session counter for `crawl.browser.session_id`.
static NATIVE_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Render `url` with the native backend. Returns the page, the URLs the SSRF policy refused for
/// requests the page sent, and the redirects the backend followed within `max_redirects`.
/// ~keep Only the browser tier in `browser.rs` calls it; a native scrape renders each
/// ~keep hop with `native_browser_render`.
#[cfg(any(feature = "browser", test))]
pub(crate) async fn native_browser_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    native_executor: &NativeBrowserExecutor,
) -> Result<(HttpResponse, Vec<String>, usize), CrawlError> {
    let mut jar = to_native_cookies(prior_cookies);
    native_browser_render(url, config, &mut jar, native_executor).await
}

/// Render `url` with the native backend, starting from the cookies in `jar` and leaving in it the
/// jar the render ended with. The records keep their `secure` and `http_only` flags, so a chain
/// of renders carries its cookies from one render to the next as one browser would.
pub(crate) async fn native_browser_render(
    url: &str,
    config: &CrawlConfig,
    jar: &mut Vec<NBCookie>,
    native_executor: &NativeBrowserExecutor,
) -> Result<(HttpResponse, Vec<String>, usize), CrawlError> {
    let session_id = NATIVE_SESSION_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let session_id_str = session_id.to_string();

    let span = tracing::info_span!(
        "crawl.browser.session",
        { CRAWL_BROWSER_BACKEND } = "native",
        { CRAWL_BROWSER_SESSION_ID } = %session_id_str,
        { CRAWL_PAGES_RENDERED } = 1_i64,
    );

    registry().browser_sessions_active.add(1, &[]);
    struct SessionGuard;
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            registry().browser_sessions_active.add(-1, &[]);
        }
    }
    let _guard = SessionGuard;

    native_browser_fetch_inner(url, config, jar, native_executor)
        .instrument(span)
        .await
}

async fn native_browser_fetch_inner(
    url: &str,
    config: &CrawlConfig,
    jar: &mut Vec<NBCookie>,
    native_executor: &NativeBrowserExecutor,
) -> Result<(HttpResponse, Vec<String>, usize), CrawlError> {
    if config.browser.endpoint.is_some() {
        return Err(CrawlError::invalid_config(
            "browser.endpoint is only supported by the chromiumoxide backend",
        ));
    }

    crate::types::warn_ignored_launch_options(
        &config.browser,
        "the native browser backend is selected; it runs no Chrome process",
    );
    if config.browser_profile.is_some() {
        // ~keep The native backend runs deno_core/V8 in-process and spawns no Chrome
        // ~keep subprocess, so there is no `--user-data-dir` for a profile to configure.
        tracing::warn!(
            profile = config.browser_profile.as_deref().unwrap_or_default(),
            "browser_profile is ignored by the native browser backend; it has no Chrome \
             process or user-data-dir, so persistent profiles are chromiumoxide-only"
        );
    }

    let (ssrf, refused) = crate::net::browser_policy::recording_validator_for(&config.ssrf);
    let native_config = build_native_config(config, url, jar.clone(), ssrf)?;

    let timeout = config.browser.timeout;
    let rendered = native_executor.render_url(url, &native_config).await.map_err(|e| {
        let message = e.to_string();
        if message.contains("timed out") {
            CrawlError::browser_timeout(format!("browser timed out after {timeout:?}"))
        } else {
            CrawlError::browser_error(format!("native browser render failed: {message}"))
        }
    })?;

    if config.browser.wait == BrowserWait::Fixed {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if let Some(extra) = config.browser.extra_wait {
        tokio::time::sleep(extra).await;
    }

    let status = rendered.status.unwrap_or(DEFAULT_RENDERED_STATUS);
    // ~keep The native backend parses even an empty body into a skeleton document. A status
    // ~keep that carries no document reports the empty body and the real content type, as the
    // ~keep HTTP fetch does.
    let no_document = crate::http::NO_DOCUMENT_STATUSES.contains(&status);
    let content_type = rendered.headers.get("content-type").cloned().unwrap_or_else(|| {
        if no_document {
            String::new()
        } else {
            DEFAULT_CONTENT_TYPE.to_owned()
        }
    });
    let body = if no_document { String::new() } else { rendered.html };
    let body_bytes = body.as_bytes().to_vec();

    let extras = BrowserExtras {
        eval_result: rendered.eval_result,
        network_events: rendered
            .network_events
            .into_iter()
            .map(response_meta_from_event)
            .collect(),
        cookies: rendered.cookies.iter().cloned().map(cookie_info_from_native).collect(),
    };
    *jar = rendered.cookies;

    let refused = crate::net::browser_policy::take_refused(&refused);
    let response = HttpResponse {
        status,
        content_type,
        body,
        body_bytes,
        headers: rendered.headers.into_iter().map(|(k, v)| (k, vec![v])).collect(),
        browser_extras: Some(extras),
        final_url: if rendered.final_url.is_empty() {
            url.to_owned()
        } else {
            rendered.final_url
        },
        // ~keep Native-backend screenshot capture lives in the off-limits crawlberg-browser
        // ~keep crate and is out of scope here; `browser::browser_fetch` warns the caller
        // ~keep when `capture_screenshot` is set with this backend.
        screenshot: None,
    };
    Ok((response, refused, rendered.redirects))
}

/// Content type assumed when the render reports none.
const DEFAULT_CONTENT_TYPE: &str = "text/html";

/// Status reported for a rendered page when the backend surfaces none.
const DEFAULT_RENDERED_STATUS: u16 = 200;

/// The proxy a render of `url` goes through, from [`crate::proxy::native_render_proxy`]. Its
/// credentials stay apart from its address; the native clients join them only to connect.
///
/// ~keep The render picks once: the page load and every request the page makes (module
/// ~keep imports, `fetch`, XHR) go through this one proxy.
pub(crate) fn native_proxy(config: &CrawlConfig, url: &str) -> Result<Option<UpstreamProxy>, CrawlError> {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default();
    let Some(proxy) = crate::proxy::native_render_proxy(config, &host)? else {
        return Ok(None);
    };
    let (address, credentials) = proxy.into_parts();
    let credentials = credentials.map(|credentials| ProxyCredentials {
        username: credentials.username,
        password: credentials.password,
    });
    UpstreamProxy::new(address, credentials)
        .map(Some)
        .map_err(|e| CrawlError::invalid_config(e.to_string()))
}

/// Translate the crawl-level wait strategy into the native backend's own.
fn native_wait_until(wait: &BrowserWait) -> crawlberg_browser::adapter::NativeBrowserWait {
    match wait {
        BrowserWait::NetworkIdle => crawlberg_browser::adapter::NativeBrowserWait::NetworkIdle,
        BrowserWait::Selector => crawlberg_browser::adapter::NativeBrowserWait::Selector,
        BrowserWait::Fixed => crawlberg_browser::adapter::NativeBrowserWait::Load,
    }
}

/// Carry cookies from a previous fetch into the render.
///
/// `secure` and `http_only` are not tracked by [`CookieInfo`], so they are sent
/// as `false`; the render only needs name/value/domain/path to replay a session.
#[cfg(any(feature = "browser", test))]
fn to_native_cookies(prior_cookies: Option<&[CookieInfo]>) -> Vec<NBCookie> {
    prior_cookies
        .unwrap_or(&[])
        .iter()
        .map(|c| NBCookie {
            name: c.name.clone(),
            value: c.value.clone(),
            domain: c.domain.clone(),
            path: c.path.clone(),
            secure: false,
            http_only: false,
            host_only: false,
        })
        .collect()
}

/// Assemble the native backend's render configuration from the crawl config.
#[allow(deprecated)]
pub(crate) fn build_native_config(
    config: &CrawlConfig,
    url: &str,
    prior_cookies: Vec<NBCookie>,
    ssrf: std::sync::Arc<dyn crawlberg_browser::adapter::SsrfValidator>,
) -> Result<crawlberg_browser::adapter::NativeBrowserConfig, CrawlError> {
    Ok(crawlberg_browser::adapter::NativeBrowserConfig {
        user_agent: config.user_agent.clone(),
        timeout: config.browser.timeout,
        wait_until: native_wait_until(&config.browser.wait),
        extra_headers: std::collections::HashMap::new(),
        respect_robots_txt: config.respect_robots_txt,
        stealth: matches!(config.browser.mode, crate::types::BrowserMode::Stealth),
        proxy: native_proxy(config, url)?,
        proxy_url: None,
        prior_cookies,
        block_url_patterns: config.browser.block_url_patterns.clone(),
        eval_script: config.browser.eval_script.clone(),
        wait_selector: config.browser.wait_selector.clone(),
        robots_user_agent: config.browser.robots_user_agent.clone(),
        capture_network_events: config.browser.capture_network_events,
        ssrf: Some(ssrf),
        allow_file_access: false,
        origin_headers: crate::net::credentials::origin_headers(config),
        max_redirects: Some(config.max_redirects),
    })
}

/// Project the response headers of one captured network event into [`ResponseMeta`].
fn response_meta_from_event(event: crawlberg_browser::adapter::NativeNetworkEvent) -> ResponseMeta {
    let headers = event.response_headers;
    ResponseMeta {
        server: headers.get("server").cloned(),
        etag: headers.get("etag").cloned(),
        last_modified: headers.get("last-modified").cloned(),
        cache_control: headers.get("cache-control").cloned(),
        x_powered_by: headers.get("x-powered-by").cloned(),
        content_language: headers.get("content-language").cloned(),
        content_encoding: headers.get("content-encoding").cloned(),
    }
}

fn cookie_info_from_native(cookie: NBCookie) -> CookieInfo {
    CookieInfo {
        name: cookie.name,
        value: cookie.value,
        domain: cookie.domain,
        path: cookie.path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AuthConfig;
    use crate::types::{BrowserConfig, ProxyConfig};

    /// The page every render in these tests is for.
    const PAGE: &str = "http://example.com/";

    fn proxy(url: &str, username: Option<&str>, password: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            url: url.to_owned(),
            username: username.map(str::to_owned),
            password: password.map(str::to_owned),
        }
    }

    /// A config admitted for `http://example.com/`, carrying `auth`.
    fn admitted_config(auth: AuthConfig, custom_headers: std::collections::HashMap<String, String>) -> CrawlConfig {
        let seed = url::Url::parse("http://example.com/").expect("test URL must parse");
        CrawlConfig {
            custom_headers,
            auth: Some(auth),
            credential_scope: crate::net::CredentialScope::for_seed(&seed, None),
            ..CrawlConfig::default()
        }
    }

    fn test_validator(config: &CrawlConfig) -> std::sync::Arc<dyn crawlberg_browser::adapter::SsrfValidator> {
        crate::net::browser_policy::recording_validator_for(&config.ssrf).0
    }

    #[test]
    fn a_bearer_token_and_the_custom_headers_are_scoped_to_the_seed_host() {
        let custom_headers = std::collections::HashMap::from([("x-custom".to_owned(), "value".to_owned())]);
        let config = admitted_config(
            AuthConfig::Bearer {
                token: "secret-token".to_owned(),
            },
            custom_headers,
        );

        let native = build_native_config(&config, PAGE, Vec::new(), test_validator(&config))
            .expect("an admitted config must build");

        assert!(
            native.extra_headers.is_empty(),
            "every host receives extra_headers, so nothing may be there: {:?}",
            native.extra_headers
        );
        let scoped = native
            .origin_headers
            .expect("the headers must be scoped to the seed host");
        assert_eq!(scoped.host, "example.com");
        assert_eq!(
            scoped.headers,
            [
                ("x-custom".to_owned(), "value".to_owned()),
                ("Authorization".to_owned(), "Bearer secret-token".to_owned()),
            ]
        );
    }

    #[test]
    fn an_explicit_auth_header_keeps_its_name() {
        let config = admitted_config(
            AuthConfig::Header {
                name: "X-Api-Key".to_owned(),
                value: "k".to_owned(),
            },
            std::collections::HashMap::new(),
        );

        let scoped = build_native_config(&config, PAGE, Vec::new(), test_validator(&config))
            .expect("an admitted config must build")
            .origin_headers
            .expect("the header must be scoped to the seed host");
        assert_eq!(scoped.headers, [("X-Api-Key".to_owned(), "k".to_owned())]);
    }

    #[test]
    fn no_scoped_headers_when_there_is_nothing_to_send() {
        let seed = url::Url::parse("http://example.com/").expect("test URL must parse");
        let config = CrawlConfig {
            credential_scope: crate::net::CredentialScope::for_seed(&seed, None),
            ..CrawlConfig::default()
        };

        let native = build_native_config(&config, PAGE, Vec::new(), test_validator(&config))
            .expect("an admitted config must build");
        assert_eq!(native.origin_headers, None);
    }

    /// The address and the credentials `config` renders through.
    fn resolved(config: &CrawlConfig) -> Option<(String, Option<(String, String)>)> {
        native_proxy(config, PAGE)
            .expect("the proxy must resolve")
            .map(|proxy| {
                let credentials = proxy
                    .credentials()
                    .map(|credentials| (credentials.username.clone(), credentials.password.clone()));
                (proxy.address().to_string(), credentials)
            })
    }

    fn credentials(user: &str, password: &str) -> Option<(String, String)> {
        Some((user.to_owned(), password.to_owned()))
    }

    #[test]
    fn proxy_credentials_stay_out_of_http_and_https_addresses() {
        for (url, address) in [
            ("http://proxy:8080", "http://proxy:8080/"),
            ("https://proxy:8443", "https://proxy:8443/"),
        ] {
            let config = CrawlConfig {
                proxy: Some(proxy(url, Some("u"), Some("p"))),
                ..CrawlConfig::default()
            };
            assert_eq!(resolved(&config), Some((address.to_owned(), credentials("u", "p"))));
        }
    }

    #[test]
    fn a_credential_free_proxy_is_returned_as_parsed() {
        let plain = CrawlConfig {
            proxy: Some(proxy("http://proxy:8080", None, None)),
            ..CrawlConfig::default()
        };
        assert_eq!(resolved(&plain), Some(("http://proxy:8080/".to_owned(), None)));
        assert_eq!(resolved(&CrawlConfig::default()), None);
    }

    #[test]
    fn an_unparseable_proxy_is_a_config_error() {
        let config = CrawlConfig {
            proxy: Some(proxy("not a url", Some("alice"), Some("s3cr3t"))),
            ..CrawlConfig::default()
        };
        assert!(
            matches!(native_proxy(&config, PAGE), Err(CrawlError::InvalidConfig { .. })),
            "a malformed proxy address must be a config error"
        );
    }

    #[test]
    fn a_socks_proxy_is_refused_for_the_native_backend() {
        let socks = CrawlConfig {
            proxy: Some(proxy("socks5://proxy:1080", Some("u"), Some("p"))),
            ..CrawlConfig::default()
        };
        let err = native_proxy(&socks, PAGE).expect_err("the native clients cannot speak SOCKS");
        assert!(err.to_string().contains("'socks5'"), "{err}");
    }

    #[test]
    fn a_password_with_special_characters_keeps_its_value_and_the_host() {
        // ~keep A `:`/`@`/`/` in a credential must not be able to end the userinfo early and
        // ~keep smuggle in a different host.
        let config = CrawlConfig {
            proxy: Some(proxy(
                "http://proxy.test:8080",
                Some("weird:user@name"),
                Some("p@ss:w/ord"),
            )),
            ..CrawlConfig::default()
        };
        assert_eq!(
            resolved(&config),
            Some((
                "http://proxy.test:8080/".to_owned(),
                credentials("weird:user@name", "p@ss:w/ord")
            ))
        );
    }

    #[test]
    fn the_browser_proxy_overrides_the_crawl_wide_proxy() {
        let config = CrawlConfig {
            proxy: Some(proxy("http://crawl-proxy:1", None, None)),
            browser: BrowserConfig {
                proxy: Some(proxy("http://browser-proxy:2", None, None)),
                ..BrowserConfig::default()
            },
            ..CrawlConfig::default()
        };
        assert_eq!(resolved(&config), Some(("http://browser-proxy:2/".to_owned(), None)));
    }

    /// A provider that hands out `proxy` (or none) and records the hosts it is asked for.
    #[derive(Debug)]
    struct Provider {
        proxy: Option<ProxyConfig>,
        hosts: std::sync::Mutex<Vec<String>>,
    }

    impl crate::ProxyProvider for Provider {
        fn next_proxy(&self, host: &str) -> Option<ProxyConfig> {
            self.hosts.lock().expect("hosts lock").push(host.to_owned());
            self.proxy.clone()
        }
    }

    fn with_provider(config: CrawlConfig, proxy: Option<ProxyConfig>) -> (CrawlConfig, std::sync::Arc<Provider>) {
        let provider = std::sync::Arc::new(Provider {
            proxy,
            hosts: std::sync::Mutex::new(Vec::new()),
        });
        let config = CrawlConfig {
            proxy_provider: Some(provider.clone()),
            ..config
        };
        (config, provider)
    }

    #[test]
    fn a_render_goes_through_the_proxy_the_provider_picks_for_the_page_host() {
        let crawl_wide = CrawlConfig {
            proxy: Some(proxy("http://crawl-proxy:1", None, None)),
            ..CrawlConfig::default()
        };
        let (config, provider) = with_provider(crawl_wide, Some(proxy("http://picked:3", Some("u"), Some("p"))));
        assert_eq!(
            resolved(&config),
            Some(("http://picked:3/".to_owned(), credentials("u", "p")))
        );
        assert_eq!(*provider.hosts.lock().expect("hosts lock"), ["example.com"]);
    }

    #[test]
    fn a_render_goes_direct_when_the_provider_picks_no_proxy() {
        let crawl_wide = CrawlConfig {
            proxy: Some(proxy("http://crawl-proxy:1", None, None)),
            ..CrawlConfig::default()
        };
        let (config, _) = with_provider(crawl_wide, None);
        assert_eq!(resolved(&config), None);
    }

    #[test]
    fn a_render_refuses_an_invalid_proxy_from_the_provider() {
        let (config, _) = with_provider(
            CrawlConfig::default(),
            Some(proxy("http://operator:4242#tail@proxy:1", None, None)),
        );
        let error =
            native_proxy(&config, PAGE).expect_err("an invalid provider proxy must fail instead of sending direct");
        assert!(error.to_string().contains("percent-encode"), "{error}");
    }

    #[test]
    fn the_browser_proxy_wins_over_the_provider() {
        let browser = CrawlConfig {
            browser: BrowserConfig {
                proxy: Some(proxy("http://browser-proxy:2", None, None)),
                ..BrowserConfig::default()
            },
            ..CrawlConfig::default()
        };
        let (config, provider) = with_provider(browser, Some(proxy("http://picked:3", None, None)));
        assert_eq!(resolved(&config), Some(("http://browser-proxy:2/".to_owned(), None)));
        assert!(
            provider.hosts.lock().expect("hosts lock").is_empty(),
            "the provider is not asked"
        );
    }

    #[test]
    fn a_socks_proxy_from_the_provider_fails_the_render() {
        let (config, _) = with_provider(CrawlConfig::default(), Some(proxy("socks5://picked:1080", None, None)));
        let err = native_proxy(&config, PAGE).expect_err("the native clients cannot speak SOCKS");
        assert!(err.to_string().contains("'socks5'"), "{err}");
    }

    #[test]
    fn a_fixed_wait_maps_to_the_native_load_strategy() {
        assert!(matches!(
            native_wait_until(&BrowserWait::Fixed),
            crawlberg_browser::adapter::NativeBrowserWait::Load
        ));
        assert!(matches!(
            native_wait_until(&BrowserWait::NetworkIdle),
            crawlberg_browser::adapter::NativeBrowserWait::NetworkIdle
        ));
        assert!(matches!(
            native_wait_until(&BrowserWait::Selector),
            crawlberg_browser::adapter::NativeBrowserWait::Selector
        ));
    }

    #[test]
    fn prior_cookies_are_forwarded_with_name_value_domain_and_path() {
        let cookies = vec![CookieInfo {
            name: "sid".to_owned(),
            value: "abc".to_owned(),
            domain: Some("example.com".to_owned()),
            path: Some("/".to_owned()),
        }];

        let native = to_native_cookies(Some(&cookies));

        assert_eq!(native.len(), 1);
        assert_eq!(native[0].name, "sid");
        assert_eq!(native[0].value, "abc");
        assert_eq!(native[0].domain.as_deref(), Some("example.com"));
        assert_eq!(native[0].path.as_deref(), Some("/"));
        assert!(to_native_cookies(None).is_empty(), "no prior cookies means none sent");
    }

    #[test]
    fn a_network_event_projects_only_the_documented_response_headers() {
        let mut response_headers = std::collections::HashMap::new();
        response_headers.insert("server".to_owned(), "nginx".to_owned());
        response_headers.insert("etag".to_owned(), "\"abc\"".to_owned());
        response_headers.insert("x-ignored".to_owned(), "nope".to_owned());

        let meta = response_meta_from_event(crawlberg_browser::adapter::NativeNetworkEvent {
            url: "https://example.com/".to_owned(),
            method: "GET".to_owned(),
            resource_type: "document".to_owned(),
            status: 200,
            request_headers: std::collections::HashMap::new(),
            response_headers,
            body_size: 0,
            timestamp_ms: 0,
        });

        assert_eq!(meta.server.as_deref(), Some("nginx"));
        assert_eq!(meta.etag.as_deref(), Some("\"abc\""));
        assert_eq!(meta.last_modified, None);
        assert_eq!(meta.cache_control, None);
    }

    /// Render `http://<host>:<port>/` with one prior cookie for `domain`; return the Cookie
    /// headers the page request carried.
    async fn cookies_sent_with_a_prior_cookie(host: &str, domain: &str) -> Vec<String> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>page</body></html>", "text/html"))
            .mount(&site)
            .await;
        let executor =
            NativeBrowserExecutor::new(crawlberg_browser::adapter::NativeBrowserExecutorConfig::with_workers(1))
                .expect("a single-worker executor must start");
        let config = CrawlConfig {
            browser: BrowserConfig {
                backend: crate::types::BrowserBackend::Native,
                mode: crate::types::BrowserMode::Always,
                timeout: Duration::from_secs(10),
                ..BrowserConfig::default()
            },
            ..CrawlConfig::builder().allow_private_networks(true).build()
        };
        let prior = [CookieInfo {
            name: "session".to_owned(),
            value: "abc".to_owned(),
            domain: Some(domain.to_owned()),
            path: Some("/".to_owned()),
        }];

        let url = format!("http://{host}:{}/", site.address().port());
        native_browser_fetch(&url, &config, Some(&prior), &executor)
            .await
            .expect("the render must succeed");

        let requests = site.received_requests().await.expect("request recording is on");
        requests
            .iter()
            .flat_map(|r| r.headers.get_all("cookie").iter())
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect()
    }

    /// The caller's prior cookies start the render's jar: the page request carries them.
    #[tokio::test]
    async fn a_native_fetch_sends_the_prior_cookies_it_is_given() {
        let sent = cookies_sent_with_a_prior_cookie("127.0.0.1", "127.0.0.1").await;
        assert_eq!(sent, ["session=abc"], "the page request must carry the prior cookie");
    }

    /// A prior cookie names its domain, so it also goes to that domain's subdomains.
    #[tokio::test]
    async fn a_native_fetch_sends_a_prior_cookie_to_a_subdomain_of_its_domain() {
        let sent = cookies_sent_with_a_prior_cookie("a.localhost", "localhost").await;
        assert_eq!(
            sent,
            ["session=abc"],
            "a.localhost must get the prior cookie for localhost"
        );
    }
}
