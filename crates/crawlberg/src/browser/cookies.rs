use std::time::{SystemTime, UNIX_EPOCH};

use chromiumoxide::cdp::browser_protocol::network::{Cookie, CookieParam, CookieSourceScheme, TimeSinceEpoch};
use chromiumoxide::cdp::browser_protocol::storage::{
    GetCookiesParams as StorageGetCookiesParams, SetCookiesParams as StorageSetCookiesParams,
};

use crate::error::CrawlError;
use crate::ssrf_intercept::Watch;
use crate::types::BrowserCookie;

/// Seed the page with cookies carried over from a previous fetch.
///
/// ~keep Replay is one browser-scoped operation against the watched page's exact context. A
/// ~keep failure aborts the hop because continuing would silently turn an authenticated chain
/// ~keep into an unauthenticated request.
pub(super) async fn apply_prior_cookies(
    watch: &Watch,
    prior_cookies: Option<&[BrowserCookie]>,
) -> Result<(), CrawlError> {
    let Some(cookies) = prior_cookies else {
        return Ok(());
    };
    if cookies.is_empty() {
        return Ok(());
    }
    let (browser, browser_context_id) = watch.cookie_store()?;
    browser
        .execute(StorageSetCookiesParams {
            cookies: cookies.iter().map(|cookie| cookie.params.clone()).collect(),
            browser_context_id,
        })
        .await
        .map_err(|error| CrawlError::browser_error(format!("failed to seed browser cookies: {error}")))?;
    Ok(())
}

/// Read the whole browser context's jar before its page is released. A URL-scoped
/// `Network.getCookies` would omit a cookie whose `Path` matches only the next hop. ~keep
pub(super) async fn page_cookies(watch: &Watch) -> Result<Vec<BrowserCookie>, CrawlError> {
    let (browser, browser_context_id) = watch.cookie_store()?;
    let response = browser
        .execute(StorageGetCookiesParams { browser_context_id })
        .await
        .map_err(|error| CrawlError::browser_error(format!("failed to read browser cookies: {error}")))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CrawlError::browser_error(format!("system clock precedes Unix epoch: {error}")))?
        .as_secs_f64();
    Ok(response
        .result
        .cookies
        .into_iter()
        .filter_map(|cookie| browser_cookie(cookie, now))
        .collect())
}

/// Preserve a stored cookie as a CDP set-cookie parameter without reviving an expired cookie or
/// turning a host-only cookie into a domain cookie. ~keep
fn browser_cookie(cookie: Cookie, now: f64) -> Option<BrowserCookie> {
    if (!cookie.session && (!cookie.expires.is_finite() || cookie.expires <= now))
        || cookie.partition_key_opaque == Some(true)
    {
        return None;
    }
    let (url, domain) = if cookie.domain.starts_with('.') {
        (None, Some(cookie.domain.clone()))
    } else {
        (Some(host_only_cookie_url(&cookie)?), None)
    };
    Some(BrowserCookie {
        params: CookieParam {
            name: cookie.name,
            value: cookie.value,
            url,
            domain,
            path: Some(cookie.path),
            secure: Some(cookie.secure),
            http_only: Some(cookie.http_only),
            same_site: cookie.same_site,
            expires: (!cookie.session).then(|| TimeSinceEpoch::new(cookie.expires)),
            priority: Some(cookie.priority),
            same_party: None,
            source_scheme: Some(cookie.source_scheme),
            source_port: Some(cookie.source_port),
            partition_key: cookie.partition_key,
        },
    })
}

fn host_only_cookie_url(cookie: &Cookie) -> Option<String> {
    let scheme = match cookie.source_scheme {
        CookieSourceScheme::Secure => "https",
        CookieSourceScheme::NonSecure => "http",
        CookieSourceScheme::Unset if cookie.secure => "https",
        CookieSourceScheme::Unset => "http",
    };
    let mut url = url::Url::parse(&format!("{scheme}://cookie.invalid/")).ok()?;
    url.set_host(Some(&cookie.domain)).ok()?;
    if let Ok(port) = u16::try_from(cookie.source_port)
        && port != 0
    {
        url.set_port(Some(port)).ok()?;
    }
    url.set_path(&cookie.path);
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use chromiumoxide::cdp::browser_protocol::network::{CookiePartitionKey, CookiePriority, CookieSameSite};

    use super::*;

    #[test]
    fn a_carried_cookie_keeps_all_replayable_cdp_metadata() {
        let partition = CookiePartitionKey::new("https://top.example", true);
        let cookie = cookie(".example.com", false, 200.0, Some(partition.clone()));

        let carried = browser_cookie(cookie, 100.0).expect("the unexpired cookie must be carried");
        let params = carried.params;

        assert_eq!(params.url, None);
        assert_eq!(params.domain.as_deref(), Some(".example.com"));
        assert_eq!(params.path.as_deref(), Some("/private"));
        assert_eq!(params.secure, Some(true));
        assert_eq!(params.http_only, Some(true));
        assert_eq!(params.same_site, Some(CookieSameSite::Strict));
        assert_eq!(params.expires.as_ref().map(TimeSinceEpoch::inner), Some(&200.0));
        assert_eq!(params.priority, Some(CookiePriority::High));
        assert_eq!(params.source_scheme, Some(CookieSourceScheme::Secure));
        assert_eq!(params.source_port, Some(8443));
        assert_eq!(params.partition_key, Some(partition));
    }

    #[test]
    fn a_host_only_cookie_stays_host_only_when_carried() {
        let carried =
            browser_cookie(cookie("example.com", true, -1.0, None), 100.0).expect("the session cookie must be carried");

        assert_eq!(carried.params.domain, None);
        assert_eq!(carried.params.url.as_deref(), Some("https://example.com:8443/private"));
        assert_eq!(carried.params.expires, None);
    }

    #[test]
    fn an_expired_cookie_is_not_carried() {
        assert!(
            browser_cookie(cookie("example.com", false, 99.0, None), 100.0).is_none(),
            "a cookie that expired before capture must not be resurrected"
        );
    }

    #[test]
    fn an_opaque_partition_cookie_is_not_replayed_without_its_key() {
        let mut opaque = cookie("example.com", true, -1.0, None);
        opaque.partition_key_opaque = Some(true);

        assert!(
            browser_cookie(opaque, 100.0).is_none(),
            "an opaque partition must not be replayed as an unpartitioned cookie"
        );
    }

    fn cookie(domain: &str, session: bool, expires: f64, partition_key: Option<CookiePartitionKey>) -> Cookie {
        Cookie {
            name: "session".to_owned(),
            value: "secret".to_owned(),
            domain: domain.to_owned(),
            path: "/private".to_owned(),
            expires,
            size: 13,
            http_only: true,
            secure: true,
            session,
            same_site: Some(CookieSameSite::Strict),
            priority: CookiePriority::High,
            source_scheme: CookieSourceScheme::Secure,
            source_port: 8443,
            partition_key,
            partition_key_opaque: Some(false),
        }
    }
}
