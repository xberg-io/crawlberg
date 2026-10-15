//! `Set-Cookie` `Domain=` attribute validation.
//!
//! Enforces the [RFC 6265 §5.3](https://datatracker.ietf.org/doc/html/rfc6265#section-5.3)
//! domain-match rule (a response may only set a cookie scoped to its own host or a
//! parent of it) plus the public-suffix rule of step 5, so a response cannot plant a
//! cookie shared by every unrelated site under a public suffix (a hosting suffix such as
//! `onrender.com` or `github.io`, a country suffix such as `co.il`, or a bare TLD). The
//! suffixes come from the Mozilla Public Suffix List that the [`psl`] crate compiles in.
//!
//! [`PolicyCookieStore`] wraps [`reqwest::cookie::Jar`] and applies this check to every
//! `Set-Cookie` header before it reaches the jar, so `http::build_client` can hand it to
//! `ClientBuilder::cookie_provider` in place of the unvalidated `cookie_store(true)`.

use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;

/// Error rejecting a cookie's `Domain` attribute.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CookieDomainError {
    /// The `Domain` attribute is neither the response host nor a parent of it. Accepting
    /// it would let any origin set a cookie scoped to an unrelated host.
    #[error("cookie domain '{domain}' does not domain-match response host '{host}'")]
    NotDomainMatch {
        /// The response host that sent the `Set-Cookie` header.
        host: String,
        /// The offending `Domain` attribute value.
        domain: String,
    },
    /// The `Domain` attribute, though it domain-matches, names a public suffix. Accepting
    /// it would create a cookie shared by every unrelated site under that suffix.
    #[error("cookie domain '{0}' is a public suffix")]
    PublicSuffix(String),
}

/// RFC 6265 §5.1.3 domain-match: `domain` and `host` are identical, or `host` is a
/// subdomain of `domain` (with a `.` boundary, not merely a matching substring). Both
/// arguments are compared case-insensitively; `domain` is used without a leading dot.
fn is_domain_match(host: &str, domain: &str) -> bool {
    host.eq_ignore_ascii_case(domain) || {
        let domain_lower = domain.to_ascii_lowercase();
        host.to_ascii_lowercase().ends_with(&format!(".{domain_lower}"))
    }
}

/// Whether `domain` (lower-case, no leading dot) is a public suffix of the Mozilla Public
/// Suffix List, including an unlisted top-level label, which the list's implicit `*` rule
/// treats as a suffix.
fn is_public_suffix(domain: &str) -> bool {
    psl::suffix(domain.as_bytes()).is_some_and(|suffix| suffix.as_bytes() == domain.as_bytes())
}

/// Validate a cookie's `Domain` attribute against the host that sent it and give the scope
/// the cookie gets: `Ok(None)` for a host-only cookie, `Ok(Some(domain))` for a domain
/// cookie, with `domain` in lower case and without its leading dot.
///
/// `domain_attr` is the raw `Domain=` value from a `Set-Cookie` header (a leading dot,
/// if present, is stripped per RFC 6265 §5.2.3 before comparison). An empty attribute —
/// including one that was only a leading dot — is treated as absent (a host-only cookie)
/// and always passes: there is nothing to validate. A `Domain` that is a public suffix and
/// is `host` itself gives a host-only cookie as well (RFC 6265 §5.3 step 5), so `onrender.com`
/// keeps a cookie it sets with `Domain=onrender.com` and no other site under the suffix
/// receives it.
///
/// # Errors
///
/// Returns [`CookieDomainError::NotDomainMatch`] if `host` is not `domain_attr` and not a
/// subdomain of it, or [`CookieDomainError::PublicSuffix`] if `domain_attr` names a public
/// suffix of the Public Suffix List that is not `host` itself.
pub fn validate_cookie_domain(host: &str, domain_attr: &str) -> Result<Option<String>, CookieDomainError> {
    let domain = domain_attr.trim().trim_start_matches('.');
    if domain.is_empty() {
        return Ok(None);
    }

    if !is_domain_match(host, domain) {
        return Err(CookieDomainError::NotDomainMatch {
            host: host.to_owned(),
            domain: domain.to_owned(),
        });
    }

    let domain = domain.to_ascii_lowercase();
    if is_public_suffix(&domain) {
        if host.eq_ignore_ascii_case(&domain) {
            return Ok(None);
        }
        return Err(CookieDomainError::PublicSuffix(domain));
    }

    Ok(Some(domain))
}

/// The value of an attribute that the cookie parser of reqwest's jar reads as `Domain`: the
/// key before the first `=` matches case-insensitively once trimmed, and the value, trimmed,
/// must be non-empty. ~keep The same reading as `cookie` 0.18's `parse_inner`, so admission
/// sees the attribute the jar stores.
fn domain_attribute_value(attr: &str) -> Option<&str> {
    let (key, value) = attr.split_once('=')?;
    let value = value.trim();
    (key.trim().eq_ignore_ascii_case("domain") && !value.is_empty()).then_some(value)
}

/// The `Domain` the jar takes from a raw `Set-Cookie` header: the last non-empty one, since
/// each later attribute replaces the earlier (RFC 6265 section 5.2.3 and the `cookie` crate).
fn domain_attribute(raw_set_cookie: &str) -> Option<String> {
    raw_set_cookie
        .split(';')
        .skip(1)
        .filter_map(domain_attribute_value)
        .last()
        .map(str::to_owned)
}

/// The raw `Set-Cookie` header without its `Domain=` attributes: the header of the host-only
/// cookie that RFC 6265 §5.3 step 5 stores when `Domain` is the host's own public suffix.
fn without_domain_attribute(raw_set_cookie: &str) -> String {
    let mut parts = raw_set_cookie.split(';');
    let mut header = parts.next().unwrap_or_default().to_owned();
    for attr in parts.filter(|attr| domain_attribute_value(attr).is_none()) {
        header.push(';');
        header.push_str(attr);
    }
    header
}

/// [`reqwest::cookie::CookieStore`] that enforces [`validate_cookie_domain`] on every
/// `Set-Cookie` header before delegating to an inner [`Jar`].
///
/// Wire in place of `ClientBuilder::cookie_store(true)`:
///
/// ```
/// use std::sync::Arc;
/// use crawlberg::net::cookie::PolicyCookieStore;
///
/// let builder = reqwest::Client::builder()
///     .cookie_provider(Arc::new(PolicyCookieStore::default()));
/// let _ = builder;
/// ```
#[derive(Debug, Default)]
pub struct PolicyCookieStore {
    inner: Jar,
}

impl CookieStore for PolicyCookieStore {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &url::Url) {
        let host = url.host_str().unwrap_or_default();
        let permitted: Vec<HeaderValue> = cookie_headers
            .filter_map(|header| {
                let raw = header.to_str().ok()?;
                let Some(domain) = domain_attribute(raw) else {
                    return Some(header.clone());
                };
                match validate_cookie_domain(host, &domain) {
                    Ok(Some(_)) => Some(header.clone()),
                    // ~keep The jar has no public suffix list: handed `Domain=onrender.com` from
                    // onrender.com it would store a domain cookie and send it to every site
                    // under the suffix. The host-only cookie of RFC 6265 §5.3 step 5 is the
                    // header without the attribute.
                    Ok(None) => HeaderValue::from_str(&without_domain_attribute(raw)).ok(),
                    Err(e) => {
                        // ~keep Dropping, not just logging: a public-suffix or spoofed
                        // Domain must never reach the jar, or it would be replayed to
                        // every host sharing that suffix.
                        tracing::warn!(host = %host, error = %e, "dropping cookie with invalid domain");
                        None
                    }
                }
            })
            .collect();
        self.inner.set_cookies(&mut permitted.iter(), url);
    }

    fn cookies(&self, url: &url::Url) -> Option<HeaderValue> {
        self.inner.cookies(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_domain_match_accepts_identical_host() {
        assert!(is_domain_match("example.com", "example.com"));
    }

    #[test]
    fn is_domain_match_accepts_parent_domain() {
        assert!(
            is_domain_match("api.example.com", "example.com"),
            "a subdomain's response may widen its cookie to the registrable parent"
        );
    }

    #[test]
    fn is_domain_match_rejects_unrelated_domain() {
        assert!(
            !is_domain_match("evil.example", "victim.example"),
            "an unrelated domain must not domain-match"
        );
    }

    #[test]
    fn is_domain_match_rejects_substring_that_is_not_a_label_boundary() {
        assert!(
            !is_domain_match("notexample.com", "example.com"),
            "'notexample.com' shares a suffix with 'example.com' but is not a subdomain of it"
        );
    }

    #[test]
    fn validate_cookie_domain_rejects_cross_origin_spoof() {
        // ~keep The literal attack: a response from evil.example claims Domain=victim.example.
        let err = validate_cookie_domain("evil.example", "victim.example")
            .expect_err("evil.example must not be able to scope a cookie to victim.example");
        assert_eq!(
            err,
            CookieDomainError::NotDomainMatch {
                host: "evil.example".to_owned(),
                domain: "victim.example".to_owned(),
            },
            "unexpected error variant: {err:?}"
        );
    }

    #[test]
    fn validate_cookie_domain_permits_legitimate_parent_widening() {
        validate_cookie_domain("api.example.com", "example.com")
            .expect("a subdomain must be able to widen a cookie to its own registrable parent");
    }

    #[test]
    fn validate_cookie_domain_permits_host_only_cookie() {
        validate_cookie_domain("example.com", "").expect("an absent/empty Domain is host-only and always valid");
    }

    #[test]
    fn validate_cookie_domain_strips_leading_dot() {
        validate_cookie_domain("api.example.com", ".example.com")
            .expect("a leading dot on the Domain attribute must not change the outcome");
    }

    #[test]
    fn validate_cookie_domain_rejects_known_public_suffix() {
        // ~keep A tenant of a shared hosting domain must not be able to plant a cookie for
        // every other tenant sharing that suffix.
        let err = validate_cookie_domain("evil-tenant.herokuapp.com", "herokuapp.com")
            .expect_err("herokuapp.com is a known public suffix and must be rejected");
        assert_eq!(
            err,
            CookieDomainError::PublicSuffix("herokuapp.com".to_owned()),
            "unexpected error variant: {err:?}"
        );
    }

    #[test]
    fn validate_cookie_domain_rejects_bare_tld() {
        let err =
            validate_cookie_domain("evil.com", "com").expect_err("a bare TLD must be rejected as a public suffix");
        assert_eq!(err, CookieDomainError::PublicSuffix("com".to_owned()));
    }

    #[test]
    fn validate_cookie_domain_rejects_a_suffix_of_the_public_suffix_list() {
        for (host, domain) in [("a.onrender.com", "onrender.com"), ("a.co.il", "co.il")] {
            assert_eq!(
                validate_cookie_domain(host, domain),
                Err(CookieDomainError::PublicSuffix(domain.to_owned())),
                "{domain} is a public suffix for {host}"
            );
        }
    }

    #[test]
    fn validate_cookie_domain_gives_the_suffix_host_itself_a_host_only_cookie() {
        assert_eq!(
            validate_cookie_domain("onrender.com", ".OnRender.com"),
            Ok(None),
            "RFC 6265 5.3 step 5: a Domain that is the host's own public suffix is host-only"
        );
        assert_eq!(
            validate_cookie_domain("api.example.com", ".Example.com"),
            Ok(Some("example.com".to_owned())),
            "a registrable domain gives a domain cookie in lower case without its dot"
        );
    }

    #[test]
    fn without_domain_attribute_keeps_every_other_attribute() {
        assert_eq!(
            without_domain_attribute("own=1; Path=/; DOMAIN=onrender.com; Secure; domain=.onrender.com"),
            "own=1; Path=/; Secure"
        );
        assert_eq!(without_domain_attribute("own=1"), "own=1");
    }

    #[test]
    fn domain_attribute_extracts_case_insensitively() {
        assert_eq!(
            domain_attribute("session=abc; Domain=victim.example; Path=/"),
            Some("victim.example".to_owned())
        );
        assert_eq!(
            domain_attribute("session=abc; DOMAIN=victim.example"),
            Some("victim.example".to_owned())
        );
    }

    #[test]
    fn domain_attribute_returns_none_when_absent() {
        assert_eq!(domain_attribute("session=abc; Path=/; HttpOnly"), None);
    }

    #[test]
    fn bare_reqwest_jar_already_rejects_the_literal_cross_domain_spoof() {
        // ~keep Documents that reqwest's own cookie_store crate already enforces
        // domain-match at insertion (Error::DomainMismatch), independent of this module.
        // The PolicyCookieStore tests below exercise the gap that remains: no
        // public-suffix check, which the literal single-domain attack does not cover.
        let jar = Jar::default();
        let evil_url = "http://evil.example/".parse::<url::Url>().unwrap();
        jar.add_cookie_str("session=stolen; Domain=victim.example", &evil_url);

        let victim_url = "http://victim.example/".parse::<url::Url>().unwrap();
        assert_eq!(
            jar.cookies(&victim_url),
            None,
            "reqwest's bare Jar must not replay a cross-origin Domain-spoofed cookie"
        );
    }

    #[test]
    fn bare_reqwest_jar_leaks_a_public_suffix_cookie_across_tenants() {
        // ~keep Proves the live gap PolicyCookieStore closes: reqwest's Jar performs no
        // public-suffix check, so a cookie scoped to a shared hosting suffix set by one
        // tenant IS replayed to a different, unrelated tenant of that same suffix.
        let jar = Jar::default();
        let evil_tenant = "http://evil-tenant.herokuapp.com/".parse::<url::Url>().unwrap();
        jar.add_cookie_str("session=stolen; Domain=herokuapp.com", &evil_tenant);

        let victim_tenant = "http://victim-tenant.herokuapp.com/".parse::<url::Url>().unwrap();
        assert!(
            jar.cookies(&victim_tenant).is_some(),
            "expected the unguarded reqwest Jar to leak the public-suffix cookie across tenants"
        );
    }

    #[test]
    fn policy_cookie_store_rejects_public_suffix_cookie_across_tenants() {
        let store = PolicyCookieStore::default();
        let evil_tenant = "http://evil-tenant.herokuapp.com/".parse::<url::Url>().unwrap();
        let header = HeaderValue::from_static("session=stolen; Domain=herokuapp.com");
        store.set_cookies(&mut std::iter::once(&header), &evil_tenant);

        let victim_tenant = "http://victim-tenant.herokuapp.com/".parse::<url::Url>().unwrap();
        assert_eq!(
            store.cookies(&victim_tenant),
            None,
            "PolicyCookieStore must not replay a public-suffix cookie to an unrelated tenant"
        );
    }

    #[test]
    fn policy_cookie_store_rejects_cross_origin_domain_spoof() {
        let store = PolicyCookieStore::default();
        let evil_url = "http://evil.example/".parse::<url::Url>().unwrap();
        let header = HeaderValue::from_static("session=stolen; Domain=victim.example");
        store.set_cookies(&mut std::iter::once(&header), &evil_url);

        let victim_url = "http://victim.example/".parse::<url::Url>().unwrap();
        assert_eq!(
            store.cookies(&victim_url),
            None,
            "PolicyCookieStore must not replay a Domain-spoofed cookie to the claimed host"
        );
    }

    #[test]
    fn policy_cookie_store_still_stores_legitimate_cookies() {
        let store = PolicyCookieStore::default();
        let url = "http://example.com/".parse::<url::Url>().unwrap();
        let header = HeaderValue::from_static("session=abc123; Path=/");
        store.set_cookies(&mut std::iter::once(&header), &url);

        let cookies = store
            .cookies(&url)
            .expect("a valid host-only cookie must be stored and returned");
        assert!(
            cookies.to_str().unwrap().contains("session=abc123"),
            "expected session=abc123 in {cookies:?}"
        );
    }

    #[test]
    fn policy_cookie_store_permits_legitimate_parent_widening() {
        let store = PolicyCookieStore::default();
        let sub_url = "http://api.example.com/".parse::<url::Url>().unwrap();
        let header = HeaderValue::from_static("session=abc123; Domain=example.com");
        store.set_cookies(&mut std::iter::once(&header), &sub_url);

        let parent_url = "http://example.com/".parse::<url::Url>().unwrap();
        let cookies = store
            .cookies(&parent_url)
            .expect("a cookie legitimately widened to its own registrable parent must be sent there");
        assert!(cookies.to_str().unwrap().contains("session=abc123"));
    }

    #[test]
    fn policy_cookie_store_never_sends_a_public_suffix_cookie_to_another_site() {
        // ~keep The rows of #762: a suffix the public suffix list knows never crosses sites,
        // ~keep whatever any fixed list says; a registrable domain under a suffix still does.
        for (setter, raw, other, sent) in [
            (
                "http://a.onrender.com/",
                "psl=1; Domain=onrender.com",
                "http://b.onrender.com/",
                false,
            ),
            ("http://a.co.il/", "psl=2; Domain=co.il", "http://b.co.il/", false),
            (
                "http://a.github.io/",
                "listed=3; Domain=github.io",
                "http://b.github.io/",
                false,
            ),
            (
                "http://a.example.org/",
                "control=4; Domain=example.org",
                "http://b.example.org/",
                true,
            ),
        ] {
            let store = PolicyCookieStore::default();
            let header = HeaderValue::from_str(raw).unwrap();
            store.set_cookies(&mut std::iter::once(&header), &setter.parse::<url::Url>().unwrap());
            let cookies = store.cookies(&other.parse::<url::Url>().unwrap());
            assert_eq!(
                cookies.is_some(),
                sent,
                "{raw} set from {setter}, then a request to {other} carries {cookies:?}"
            );
        }
    }

    #[test]
    fn policy_cookie_store_admits_the_domain_the_jar_would_use() {
        // ~keep reqwest 0.13.5 / cookie 0.18.2 take the last non-empty Domain and trim the
        // ~keep attribute key; admission must validate that one, not the first or an unspaced match.
        for (setter, raw, other) in [
            (
                "http://a.onrender.com/",
                "s=1; Domain=a.onrender.com; Domain=onrender.com",
                "http://b.onrender.com/",
            ),
            (
                "http://a.onrender.com/",
                "s=2; Domain = onrender.com",
                "http://b.onrender.com/",
            ),
            (
                "http://a.onrender.com/",
                "s=3; Domain=onrender.com; Domain=a.onrender.com; Domain=onrender.com",
                "http://b.onrender.com/",
            ),
            (
                "http://a.onrender.com/",
                "s=4; Domain=evil.test; Domain=onrender.com",
                "http://b.onrender.com/",
            ),
        ] {
            let store = PolicyCookieStore::default();
            let header = HeaderValue::from_str(raw).unwrap();
            store.set_cookies(&mut std::iter::once(&header), &setter.parse::<url::Url>().unwrap());
            assert_eq!(
                store.cookies(&other.parse::<url::Url>().unwrap()),
                None,
                "{raw} set from {setter} reached {other}"
            );
        }
    }

    #[test]
    fn policy_cookie_store_stores_the_last_domain_in_either_order() {
        let store = PolicyCookieStore::default();
        let setter = "http://a.example.org/".parse::<url::Url>().unwrap();
        let header = HeaderValue::from_static("s=1; Domain=evil.test; Domain=example.org");
        store.set_cookies(&mut std::iter::once(&header), &setter);
        let sent = store.cookies(&"http://b.example.org/".parse::<url::Url>().unwrap());
        assert!(
            sent.is_some_and(|value| value.to_str().is_ok_and(|v| v.contains("s=1"))),
            "the last Domain, example.org, is the one the jar stores and sends"
        );
        let store = PolicyCookieStore::default();
        let header = HeaderValue::from_static("s=2; Domain=example.org; Domain=evil.test");
        store.set_cookies(&mut std::iter::once(&header), &setter);
        assert_eq!(
            store.cookies(&setter),
            None,
            "the last Domain, evil.test, does not match and the cookie is dropped"
        );
    }

    #[test]
    fn policy_cookie_store_agrees_with_the_result_metadata_on_a_repeated_domain() {
        let host = "http://127.0.0.1/".parse::<url::Url>().unwrap();
        let raw = "lastwins=16; Domain=other.test; Domain=127.0.0.1";
        let store = PolicyCookieStore::default();
        let header = HeaderValue::from_static(raw);
        store.set_cookies(&mut std::iter::once(&header), &host);
        let sent = store
            .cookies(&host)
            .expect("the jar stores the last Domain, the host itself");
        assert!(sent.to_str().unwrap().contains("lastwins=16"), "sent {sent:?}");
        let headers = std::collections::HashMap::from([("set-cookie".to_owned(), vec![raw.to_owned()])]);
        let listed = crate::http::result_cookies_from_hashmap(host.as_str(), &headers);
        assert_eq!(listed.len(), 1, "result metadata lists what the jar stored");
    }

    #[test]
    fn policy_cookie_store_keeps_the_suffix_hosts_own_cookie_host_only() {
        // ~keep RFC 6265 5.3 step 5: a Domain that is the request host's own public suffix
        // ~keep gives a host-only cookie: sent back to the host, never to another site under it.
        let store = PolicyCookieStore::default();
        let host = "http://onrender.com/".parse::<url::Url>().unwrap();
        let header = HeaderValue::from_static("own=1; Domain=onrender.com");
        store.set_cookies(&mut std::iter::once(&header), &host);

        let sent = store.cookies(&host).expect("the suffix host keeps its own cookie");
        assert!(sent.to_str().unwrap().contains("own=1"), "expected own=1 in {sent:?}");
        let other = "http://b.onrender.com/".parse::<url::Url>().unwrap();
        assert_eq!(
            store.cookies(&other),
            None,
            "a host-only cookie of the suffix host must not reach another site under it"
        );
    }
}
