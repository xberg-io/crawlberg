//! Native browser backend adapter — standalone module so it can be used both
//! when only `browser-native` is active and when the full `browser` feature is on.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Duration;

use crawlberg_browser::adapter::{NativeBrowserExecutor, NativeCookie as NBCookie};
use tracing::Instrument as _;

use crate::error::CrawlError;
use crate::http::{BrowserExtras, HttpResponse};
use crate::telemetry::attributes::{CRAWL_BROWSER_BACKEND, CRAWL_BROWSER_SESSION_ID, CRAWL_PAGES_RENDERED};
use crate::telemetry::metrics::registry;
use crate::types::{AuthConfig, BrowserWait, CookieInfo, CrawlConfig, ResponseMeta};

/// Process-wide monotonic session counter for `crawl.browser.session_id`.
static NATIVE_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) async fn native_browser_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    native_executor: &NativeBrowserExecutor,
) -> Result<HttpResponse, CrawlError> {
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

    native_browser_fetch_inner(url, config, prior_cookies, native_executor)
        .instrument(span)
        .await
}

async fn native_browser_fetch_inner(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    native_executor: &NativeBrowserExecutor,
) -> Result<HttpResponse, CrawlError> {
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

    let native_config = build_native_config(config, prior_cookies);

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

    let content_type = rendered
        .headers
        .get("content-type")
        .cloned()
        .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_owned());
    let body_bytes = rendered.html.as_bytes().to_vec();

    let extras = BrowserExtras {
        eval_result: rendered.eval_result,
        network_events: rendered
            .network_events
            .into_iter()
            .map(response_meta_from_event)
            .collect(),
        cookies: rendered.cookies.into_iter().map(cookie_info_from_native).collect(),
    };

    Ok(HttpResponse {
        status: rendered.status.unwrap_or(DEFAULT_RENDERED_STATUS),
        content_type,
        body: rendered.html,
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
    })
}

/// Content type assumed when the render reports none.
const DEFAULT_CONTENT_TYPE: &str = "text/html";

/// Status reported for a rendered page when the backend surfaces none.
const DEFAULT_RENDERED_STATUS: u16 = 200;

/// Request headers to send with the render: the configured custom headers, plus
/// whatever `auth` translates into.
fn build_extra_headers(config: &CrawlConfig) -> std::collections::HashMap<String, String> {
    let mut extra_headers = config.custom_headers.clone();
    match config.auth {
        Some(AuthConfig::Bearer { ref token }) => {
            extra_headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
        }
        Some(AuthConfig::Header { ref name, ref value }) => {
            extra_headers.insert(name.clone(), value.clone());
        }
        _ => {}
    }
    extra_headers
}

/// The proxy URL to render through: the browser-specific proxy if set, else the
/// crawl-wide one, with any configured credentials inlined into the URL.
fn resolve_proxy_url(config: &CrawlConfig) -> Option<String> {
    config.browser.proxy.as_ref().or(config.proxy.as_ref()).map(|p| {
        if p.username.is_some() || p.password.is_some() {
            let user = p.username.as_deref().unwrap_or("");
            let pass = p.password.as_deref().unwrap_or("");
            if let Some(rest) = p.url.strip_prefix("http://") {
                format!("http://{user}:{pass}@{rest}")
            } else if let Some(rest) = p.url.strip_prefix("https://") {
                format!("https://{user}:{pass}@{rest}")
            } else {
                p.url.clone()
            }
        } else {
            p.url.clone()
        }
    })
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
        })
        .collect()
}

/// Assemble the native backend's render configuration from the crawl config.
fn build_native_config(
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
) -> crawlberg_browser::adapter::NativeBrowserConfig {
    crawlberg_browser::adapter::NativeBrowserConfig {
        user_agent: config.user_agent.clone(),
        timeout: config.browser.timeout,
        wait_until: native_wait_until(&config.browser.wait),
        extra_headers: build_extra_headers(config),
        respect_robots_txt: config.respect_robots_txt,
        stealth: matches!(config.browser.mode, crate::types::BrowserMode::Stealth),
        proxy_url: resolve_proxy_url(config),
        prior_cookies: to_native_cookies(prior_cookies),
        block_url_patterns: config.browser.block_url_patterns.clone(),
        eval_script: config.browser.eval_script.clone(),
        wait_selector: config.browser.wait_selector.clone(),
        robots_user_agent: config.browser.robots_user_agent.clone(),
        capture_network_events: config.browser.capture_network_events,
        ssrf: Some(crate::net::browser_policy::validator_for(&config.ssrf)),
        allow_file_access: false,
    }
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
    use crate::types::{BrowserConfig, ProxyConfig};

    fn proxy(url: &str, username: Option<&str>, password: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            url: url.to_owned(),
            username: username.map(str::to_owned),
            password: password.map(str::to_owned),
        }
    }

    #[test]
    fn extra_headers_carry_custom_headers_and_a_bearer_token() {
        let mut custom_headers = std::collections::HashMap::new();
        custom_headers.insert("x-custom".to_owned(), "value".to_owned());
        let config = CrawlConfig {
            custom_headers,
            auth: Some(AuthConfig::Bearer {
                token: "secret-token".to_owned(),
            }),
            ..CrawlConfig::default()
        };

        let headers = build_extra_headers(&config);

        assert_eq!(headers.get("x-custom").map(String::as_str), Some("value"));
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer secret-token"),
            "a Bearer auth config must become an Authorization header"
        );
    }

    #[test]
    fn extra_headers_carry_an_explicit_auth_header() {
        let config = CrawlConfig {
            auth: Some(AuthConfig::Header {
                name: "X-Api-Key".to_owned(),
                value: "k".to_owned(),
            }),
            ..CrawlConfig::default()
        };

        assert_eq!(
            build_extra_headers(&config).get("X-Api-Key").map(String::as_str),
            Some("k")
        );
    }

    #[test]
    fn proxy_credentials_are_inlined_into_http_and_https_urls() {
        let http = CrawlConfig {
            proxy: Some(proxy("http://proxy:8080", Some("u"), Some("p"))),
            ..CrawlConfig::default()
        };
        assert_eq!(resolve_proxy_url(&http).as_deref(), Some("http://u:p@proxy:8080"));

        let https = CrawlConfig {
            proxy: Some(proxy("https://proxy:8443", Some("u"), Some("p"))),
            ..CrawlConfig::default()
        };
        assert_eq!(resolve_proxy_url(&https).as_deref(), Some("https://u:p@proxy:8443"));
    }

    #[test]
    fn a_proxy_without_credentials_or_a_known_scheme_is_passed_through_unchanged() {
        let plain = CrawlConfig {
            proxy: Some(proxy("http://proxy:8080", None, None)),
            ..CrawlConfig::default()
        };
        assert_eq!(resolve_proxy_url(&plain).as_deref(), Some("http://proxy:8080"));

        let socks = CrawlConfig {
            proxy: Some(proxy("socks5://proxy:1080", Some("u"), Some("p"))),
            ..CrawlConfig::default()
        };
        assert_eq!(
            resolve_proxy_url(&socks).as_deref(),
            Some("socks5://proxy:1080"),
            "credentials cannot be inlined into a non-http(s) proxy URL"
        );

        assert_eq!(resolve_proxy_url(&CrawlConfig::default()), None);
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

        assert_eq!(resolve_proxy_url(&config).as_deref(), Some("http://browser-proxy:2"));
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
}
