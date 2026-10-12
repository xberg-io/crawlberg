use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use chromiumoxide::cdp::browser_protocol::network::{Cookie, CookieParam, CookieSourceScheme, TimeSinceEpoch};
use chromiumoxide::cdp::browser_protocol::storage::{
    GetCookiesParams as StorageGetCookiesParams, SetCookiesParams as StorageSetCookiesParams,
};

use crate::error::CrawlError;
use crate::ssrf_intercept::Watch;
use crate::types::BrowserCookie;

/// The cookies the Chrome pages of one crawl have in common.
///
/// ~keep Each page has a browser context of its own, which the SSRF check needs: the context is
/// ~keep what ties a popup to its page's policy, and its disposal is what ends the page's pending
/// ~keep requests (xberg-io/crawlberg#506). So the pages cannot share Chrome's store. Each page
/// ~keep starts with a copy of these cookies in its own store, and what it changed is recorded
/// ~keep here when it ends. Chrome still decides which cookie a request gets: every attribute
/// ~keep travels with the cookie, and nothing here reads one.
#[derive(Default)]
pub(crate) struct CrawlCookies {
    held: Mutex<Held>,
}

/// The cookies of a crawl, each with its place in the order Chrome made them.
///
/// ~keep Chrome sends the cookies of one path length oldest first, and a script reads them in
/// ~keep that order. A page's store makes the cookies it is given in the order of the list, so
/// ~keep the list keeps the order of the crawl's pages' stores.
#[derive(Default)]
struct Held {
    cookies: HashMap<Slot, (u64, BrowserCookie)>,
    made: u64,
}

impl Held {
    /// Record `cookie` as a page left it. Chrome makes a cookie anew when its value changes,
    /// and keeps its age when only an attribute does.
    fn put(&mut self, slot: Slot, cookie: BrowserCookie) {
        let kept_age = self
            .cookies
            .get(&slot)
            .filter(|(_, held)| held.params.value == cookie.params.value)
            .map(|(age, _)| *age);
        let age = kept_age.unwrap_or_else(|| {
            self.made += 1;
            self.made
        });
        self.cookies.insert(slot, (age, cookie));
    }
}

/// What makes two cookies one cookie in Chrome's store. A cookie for one host only has that
/// host as its `site`; a cookie with a `Domain` has the domain with its leading dot.
#[derive(PartialEq, Eq, Hash)]
struct Slot {
    name: String,
    site: Option<String>,
    path: Option<String>,
    partition: Option<(String, bool)>,
}

impl Slot {
    fn of(cookie: &BrowserCookie) -> Self {
        let cookie = &cookie.params;
        let host = || {
            let url = url::Url::parse(cookie.url.as_deref()?).ok()?;
            url.host_str().map(str::to_owned)
        };
        Self {
            name: cookie.name.clone(),
            site: cookie.domain.clone().or_else(host),
            path: cookie.path.clone(),
            partition: cookie
                .partition_key
                .as_ref()
                .map(|key| (key.top_level_site.clone(), key.has_cross_site_ancestor)),
        }
    }
}

impl CrawlCookies {
    /// The cookies a page that starts now is given.
    pub(crate) fn given(&self) -> Vec<BrowserCookie> {
        let mut cookies: Vec<(u64, BrowserCookie)> = self.lock().cookies.values().cloned().collect();
        cookies.sort_unstable_by_key(|(age, _)| *age);
        cookies.into_iter().map(|(_, cookie)| cookie).collect()
    }

    /// Record what a page did to the cookies it was `given`, from the cookies it `left`.
    ///
    /// ~keep Only the page's own changes are recorded. Pages of one crawl load at the same time,
    /// ~keep so a page ends with a copy that can be older than the crawl's: a cookie it left as
    /// ~keep it was given says nothing, and a cookie it no longer has is removed only while the
    /// ~keep crawl still holds the one the page was given.
    pub(crate) fn absorb(&self, given: &[BrowserCookie], left: &[BrowserCookie]) {
        let mut given: HashMap<Slot, &BrowserCookie> = given.iter().map(|cookie| (Slot::of(cookie), cookie)).collect();
        let mut held = self.lock();
        // ~keep `left` is in the order of the page's store: new cookies keep that order.
        for after in left {
            let slot = Slot::of(after);
            if given.remove(&slot).is_none_or(|before| before.params != after.params) {
                held.put(slot, after.clone());
            }
        }
        for (slot, before) in given {
            if held
                .cookies
                .get(&slot)
                .is_some_and(|(_, held)| held.params == before.params)
            {
                held.cookies.remove(&slot);
            }
        }
    }

    /// Forget each cookie that a response from `host`, fetched without the browser, set.
    ///
    /// ~keep That fetch has a cookie store of its own, so its `Set-Cookie` deleted or replaced
    /// ~keep the cookie there and the copy here is the old one. The cookie leaves by its name,
    /// ~keep whatever its path, for `host` and every domain above or below it: a later page gets
    /// ~keep no cookie in place of the old one. Nothing is added, so Chrome never gets a cookie
    /// ~keep it did not make.
    pub(crate) fn forget_set_without_browser(&self, host: &str, headers: &HashMap<String, Vec<String>>) {
        let Some(set) = headers.get("set-cookie") else {
            return;
        };
        let names: Vec<&str> = set
            .iter()
            .filter_map(|raw| raw.split(';').next()?.split_once('='))
            .map(|(name, _)| name.trim())
            .collect();
        let within = |inner: &str, outer: &str| {
            inner == outer || inner.strip_suffix(outer).is_some_and(|rest| rest.ends_with('.'))
        };
        self.lock().cookies.retain(|slot, _| {
            let site = slot.site.as_deref().unwrap_or_default().trim_start_matches('.');
            !(names.contains(&slot.name.as_str()) && (within(host, site) || within(site, host)))
        });
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

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

    /// The cookie `name` of the host `example.com` with `value`, as a page leaves it.
    fn left(name: &str, value: &str) -> BrowserCookie {
        let mut cookie = cookie("example.com", true, -1.0, None);
        cookie.name = name.to_owned();
        cookie.value = value.to_owned();
        browser_cookie(cookie, 100.0).expect("a session cookie is carried")
    }

    /// The crawl's cookies as `name=value` pairs in name order.
    fn held(crawl: &CrawlCookies) -> Vec<String> {
        let mut pairs: Vec<String> = crawl
            .given()
            .iter()
            .map(|cookie| format!("{}={}", cookie.params.name, cookie.params.value))
            .collect();
        pairs.sort();
        pairs
    }

    #[test]
    fn a_later_page_is_given_the_cookie_an_earlier_page_left() {
        let crawl = CrawlCookies::default();
        assert!(crawl.given().is_empty(), "a crawl starts with no cookie");

        crawl.absorb(&[], &[left("session", "1")]);

        assert_eq!(held(&crawl), ["session=1"]);
    }

    #[test]
    fn a_page_that_changes_a_cookie_changes_it_for_the_crawl() {
        let crawl = CrawlCookies::default();
        crawl.absorb(&[], &[left("session", "1"), left("other", "kept")]);

        let given = crawl.given();
        crawl.absorb(&given, &[left("session", "2"), left("other", "kept")]);

        assert_eq!(held(&crawl), ["other=kept", "session=2"]);
    }

    #[test]
    fn a_page_that_deletes_a_cookie_deletes_it_for_the_crawl() {
        let crawl = CrawlCookies::default();
        crawl.absorb(&[], &[left("session", "1"), left("other", "kept")]);

        let given = crawl.given();
        crawl.absorb(&given, &[left("other", "kept")]);

        assert_eq!(held(&crawl), ["other=kept"]);
    }

    /// Two pages start with the same cookies. The first to end changes one; the second ends
    /// with the copy it was given, which is older than the crawl's by then.
    #[test]
    fn a_page_that_ends_with_an_older_copy_does_not_undo_a_newer_change() {
        let crawl = CrawlCookies::default();
        crawl.absorb(&[], &[left("session", "1")]);
        let first = crawl.given();
        let second = crawl.given();

        crawl.absorb(&first, &[left("session", "2"), left("added", "1")]);
        crawl.absorb(&second, &[left("session", "1")]);

        assert_eq!(held(&crawl), ["added=1", "session=2"]);
    }

    /// Two pages start with the same cookie. One changes it and one deletes it; the deletion
    /// applies to the cookie the page was given, not to the one written since.
    #[test]
    fn a_deletion_does_not_remove_the_value_another_page_wrote_since() {
        let crawl = CrawlCookies::default();
        crawl.absorb(&[], &[left("session", "1")]);
        let writer = crawl.given();
        let deleter = crawl.given();

        crawl.absorb(&writer, &[left("session", "2")]);
        crawl.absorb(&deleter, &[]);

        assert_eq!(held(&crawl), ["session=2"]);
    }

    fn set_cookie(values: &[&str]) -> HashMap<String, Vec<String>> {
        let values = values.iter().map(|value| (*value).to_owned()).collect();
        HashMap::from([("set-cookie".to_owned(), values)])
    }

    /// A deletion and a new value both leave the crawl without the cookie: the old copy is
    /// what must not reach a later page.
    #[test]
    fn a_cookie_set_without_the_browser_leaves_the_crawl() {
        let crawl = CrawlCookies::default();
        crawl.absorb(&[], &[left("gone", "1"), left("new", "1"), left("other", "kept")]);

        crawl.forget_set_without_browser("example.com", &set_cookie(&["gone=; Max-Age=0", " new = 2; Path=/a"]));

        assert_eq!(held(&crawl), ["other=kept"]);
    }

    #[test]
    fn a_cookie_set_without_the_browser_leaves_for_the_domains_above_and_below_its_host() {
        for host in ["example.com", "www.example.com", "com"] {
            let crawl = CrawlCookies::default();
            crawl.absorb(&[], &[left("session", "1")]);

            crawl.forget_set_without_browser(host, &set_cookie(&["session=2"]));

            assert!(held(&crawl).is_empty(), "a response from {host} sets the cookie");
        }
    }

    #[test]
    fn a_response_for_another_host_or_with_no_cookie_changes_nothing() {
        let crawl = CrawlCookies::default();
        crawl.absorb(&[], &[left("session", "1")]);

        crawl.forget_set_without_browser("notexample.com", &set_cookie(&["session=2"]));
        crawl.forget_set_without_browser("other.org", &set_cookie(&["session=2"]));
        crawl.forget_set_without_browser("example.com", &set_cookie(&["no pair", "different=1"]));
        crawl.forget_set_without_browser("example.com", &HashMap::new());

        assert_eq!(held(&crawl), ["session=1"]);
    }

    #[test]
    fn two_pages_that_set_the_same_cookie_leave_one_cookie_with_the_last_value() {
        let crawl = CrawlCookies::default();
        let first = crawl.given();
        let second = crawl.given();

        crawl.absorb(&first, &[left("session", "first")]);
        crawl.absorb(&second, &[left("session", "second")]);

        assert_eq!(held(&crawl), ["session=second"]);
    }

    /// Chrome keeps these as separate cookies, so the crawl does too: the same name on another
    /// host, as a domain cookie, on another path, and in another partition.
    #[test]
    fn cookies_that_chrome_keeps_apart_stay_apart() {
        let host_only = |domain: &str| {
            let mut cookie = cookie(domain, true, -1.0, None);
            cookie.value = domain.to_owned();
            browser_cookie(cookie, 100.0).expect("a session cookie is carried")
        };
        let mut other_path = cookie("example.com", true, -1.0, None);
        other_path.path = "/public".to_owned();
        let partitioned = cookie(
            "example.com",
            true,
            -1.0,
            Some(CookiePartitionKey::new("https://top.example", false)),
        );
        let apart = [
            host_only("example.com"),
            host_only("other.example.com"),
            host_only(".example.com"),
            browser_cookie(other_path, 100.0).expect("a session cookie is carried"),
            browser_cookie(partitioned, 100.0).expect("a session cookie is carried"),
        ];
        let crawl = CrawlCookies::default();

        crawl.absorb(&[], &apart);
        assert_eq!(crawl.given().len(), apart.len());

        // ~keep The same cookie from another port of the host is one cookie in Chrome.
        let mut other_port = cookie("example.com", true, -1.0, None);
        other_port.source_port = 9443;
        other_port.value = "replaced".to_owned();
        crawl.absorb(
            &[],
            &[browser_cookie(other_port, 100.0).expect("a session cookie is carried")],
        );
        assert_eq!(crawl.given().len(), apart.len());
        assert!(crawl.given().iter().any(|cookie| cookie.params.value == "replaced"));
    }

    /// Chrome sends cookies of one path length in the order they were made, and a page's script
    /// reads them in that order.
    #[test]
    fn a_page_is_given_the_cookies_in_the_order_the_crawl_got_them() {
        let names: Vec<String> = (0..12).map(|n| format!("cookie{n}")).collect();
        let crawl = CrawlCookies::default();
        let first: Vec<BrowserCookie> = names[..8].iter().map(|name| left(name, "1")).collect();
        crawl.absorb(&[], &first);
        let given = crawl.given();
        let mut second = given.clone();
        second.extend(names[8..].iter().map(|name| left(name, "1")));
        crawl.absorb(&given, &second);

        let order: Vec<String> = crawl.given().into_iter().map(|cookie| cookie.params.name).collect();
        assert_eq!(order, names);

        // ~keep Chrome makes a cookie anew when its value changes, so it moves behind the others.
        let given = crawl.given();
        let mut changed = given.clone();
        let moved = changed.remove(3);
        changed.push(left(&moved.params.name, "2"));
        crawl.absorb(&given, &changed);

        let order: Vec<String> = crawl.given().into_iter().map(|cookie| cookie.params.name).collect();
        let mut expected = names.clone();
        let moved = expected.remove(3);
        expected.push(moved);
        assert_eq!(order, expected);
    }

    /// What one page of a pair does to the cookie `session`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Act {
        Nothing,
        Set(&'static str),
        Delete,
    }

    impl Act {
        fn on(self, value: Option<&'static str>) -> Option<&'static str> {
            match self {
                Self::Nothing => value,
                Self::Set(new) => Some(new),
                Self::Delete => None,
            }
        }
    }

    /// Run two pages against one crawl. `order` lists the page of each event: the first event
    /// of a page is its start, the second its end. Returns the crawl's `session` value.
    fn two_pages(initial: Option<&'static str>, acts: [Act; 2], order: [usize; 4]) -> Option<String> {
        let crawl = CrawlCookies::default();
        let mut start = vec![left("other", "kept")];
        start.extend(initial.map(|value| left("session", value)));
        crawl.absorb(&[], &start);

        let mut given: [Option<Vec<BrowserCookie>>; 2] = [None, None];
        for page in order {
            let Some(given) = given[page].take() else {
                given[page] = Some(crawl.given());
                continue;
            };
            let mut left_by_page: Vec<BrowserCookie> = given
                .iter()
                .filter(|cookie| cookie.params.name != "session" || acts[page] == Act::Nothing)
                .cloned()
                .collect();
            if let Act::Set(value) = acts[page] {
                left_by_page.push(left("session", value));
            }
            crawl.absorb(&given, &left_by_page);
        }

        let held = held(&crawl);
        assert!(
            held.contains(&"other=kept".to_owned()),
            "an untouched cookie stays: {held:?}"
        );
        assert!(held.len() <= 2, "one cookie for one name: {held:?}");
        held.iter()
            .find_map(|pair| pair.strip_prefix("session="))
            .map(str::to_owned)
    }

    /// Every order of the starts and ends of two pages, for every pair of actions, with and
    /// without the cookie at the start.
    #[test]
    fn two_pages_in_every_order_leave_what_one_cookie_store_would() {
        const ORDERS: [[usize; 4]; 6] = [
            [0, 0, 1, 1],
            [1, 1, 0, 0],
            [0, 1, 0, 1],
            [0, 1, 1, 0],
            [1, 0, 1, 0],
            [1, 0, 0, 1],
        ];
        let actions = |first: &'static str| [Act::Nothing, Act::Set(first), Act::Delete];
        let mut cases = 0;
        for initial in [None, Some("start")] {
            for first in actions("first") {
                for second in actions("second") {
                    for order in ORDERS {
                        let acts = [first, second];
                        let last_to_end = order[3];
                        let first_to_end = 1 - last_to_end;
                        let one_after_the_other = order[0] == order[1];
                        let expected = if one_after_the_other {
                            // ~keep The second page starts with what the first one left.
                            acts[last_to_end].on(acts[first_to_end].on(initial))
                        } else {
                            match (acts[first_to_end], acts[last_to_end]) {
                                // ~keep A page that did nothing changes nothing, whenever it ends.
                                (act, Act::Nothing) | (Act::Nothing, act) => act.on(initial),
                                // ~keep Of two values the one of the page that ends last stays.
                                (Act::Set(_) | Act::Delete, Act::Set(value)) => Some(value),
                                // ~keep A deletion does not remove a value another page wrote.
                                (Act::Set(value), Act::Delete) => Some(value),
                                (Act::Delete, Act::Delete) => None,
                            }
                        };
                        assert_eq!(
                            two_pages(initial, acts, order).as_deref(),
                            expected,
                            "initial {initial:?}, pages {acts:?}, events {order:?}"
                        );
                        cases += 1;
                    }
                }
            }
        }
        assert_eq!(cases, 108);
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
