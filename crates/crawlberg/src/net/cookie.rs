//! `Set-Cookie` `Domain=` attribute validation.
//!
//! Enforces the [RFC 6265 §5.3](https://datatracker.ietf.org/doc/html/rfc6265#section-5.3)
//! domain-match rule (a response may only set a cookie scoped to its own host or a
//! parent of it) plus a public-suffix guard on top, so a response cannot plant a
//! cookie shared by every unrelated tenant of a multi-tenant parent domain (e.g.
//! `herokuapp.com`, `github.io`) or a bare TLD.
//!
//! [`PolicyCookieStore`] wraps [`reqwest::cookie::Jar`] and applies this check to every
//! `Set-Cookie` header before it reaches the jar, so `http::build_client` can hand it to
//! `ClientBuilder::cookie_provider` in place of the unvalidated `cookie_store(true)`.

use std::collections::HashSet;
use std::sync::LazyLock;

use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;

/// Cookie `Domain` attributes that name a public suffix rather than a specific
/// registrable domain.
///
/// This is a curated, non-exhaustive subset of the Mozilla Public Suffix List: common
/// gTLDs, the most-traveled ccTLDs, and the multi-tenant hosting suffixes (`*.github.io`,
/// `*.herokuapp.com`, ...) most likely to appear as a crawl target. It is deliberately
/// small and hand-verified rather than a generated copy of the full list, so it trades
/// completeness for auditability. A response whose host does not share a listed suffix
/// with an unrelated tenant is still fully protected by the domain-match check below,
/// which is exact and complete. Integrating the `publicsuffix`/`psl` crate with the full
/// list is the natural follow-up for complete coverage.
#[rustfmt::skip]
static KNOWN_PUBLIC_SUFFIXES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // Common gTLDs.
        "com", "net", "org", "edu", "gov", "mil", "int", "io", "dev", "app", "co", "info",
        "biz", "name", "pro", "xyz", "online", "site", "tech", "store", "cloud", "page",
        "id", "me", "tv", "cc", "ai", "to", "ly",
        // High-traffic ccTLDs.
        "uk", "us", "de", "fr", "jp", "cn", "ru", "br", "in", "au", "ca", "nl", "es", "it",
        "se", "no", "dk", "fi", "pl", "ch", "at", "be", "ie", "nz", "za", "mx", "kr", "tw",
        "hk", "sg", "il",
        // Well-known second-level ccTLD suffixes.
        "co.uk", "org.uk", "gov.uk", "ac.uk", "co.jp", "ne.jp", "or.jp", "co.nz", "co.za",
        "co.in", "co.kr", "co.id", "com.au", "com.br", "com.cn", "com.mx", "com.tw",
        // Multi-tenant hosting suffixes.
        "github.io", "gitlab.io", "herokuapp.com", "netlify.app", "vercel.app", "pages.dev",
        "workers.dev", "s3.amazonaws.com", "blogspot.com", "appspot.com",
        "azurewebsites.net", "cloudfront.net", "firebaseapp.com", "web.app",
        "ondigitalocean.app",
    ]
    .into_iter()
    .collect()
});

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

/// Validate a cookie's `Domain` attribute against the host that sent it.
///
/// `domain_attr` is the raw `Domain=` value from a `Set-Cookie` header (a leading dot,
/// if present, is stripped per RFC 6265 §5.2.3 before comparison). An empty attribute —
/// including one that was only a leading dot — is treated as absent (a host-only cookie)
/// and always passes: there is nothing to validate.
///
/// # Errors
///
/// Returns [`CookieDomainError::NotDomainMatch`] if `host` is not `domain_attr` and not a
/// subdomain of it, or [`CookieDomainError::PublicSuffix`] if `domain_attr` names a known
/// public suffix (see [`KNOWN_PUBLIC_SUFFIXES`]).
pub fn validate_cookie_domain(host: &str, domain_attr: &str) -> Result<(), CookieDomainError> {
    let domain = domain_attr.trim().trim_start_matches('.');
    if domain.is_empty() {
        return Ok(());
    }

    if !is_domain_match(host, domain) {
        return Err(CookieDomainError::NotDomainMatch {
            host: host.to_owned(),
            domain: domain.to_owned(),
        });
    }

    if KNOWN_PUBLIC_SUFFIXES.contains(domain.to_ascii_lowercase().as_str()) {
        return Err(CookieDomainError::PublicSuffix(domain.to_owned()));
    }

    Ok(())
}

/// Extract the `Domain=` attribute value from a raw `Set-Cookie` header string, if
/// present. Attribute matching is case-insensitive per RFC 6265 §4.1.1.
fn domain_attribute(raw_set_cookie: &str) -> Option<String> {
    raw_set_cookie.split(';').skip(1).find_map(|attr| {
        let attr = attr.trim();
        if attr.len() > 7 && attr[..7].eq_ignore_ascii_case("domain=") {
            Some(attr[7..].to_owned())
        } else {
            None
        }
    })
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
            .filter(|header| {
                let Ok(raw) = header.to_str() else { return false };
                match domain_attribute(raw) {
                    None => true,
                    Some(domain) => match validate_cookie_domain(host, &domain) {
                        Ok(()) => true,
                        Err(e) => {
                            // ~keep Dropping, not just logging: a public-suffix or spoofed
                            // Domain must never reach the jar, or it would be replayed to
                            // every host sharing that suffix.
                            tracing::warn!(host = %host, error = %e, "dropping cookie with invalid domain");
                            false
                        }
                    },
                }
            })
            .cloned()
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
}
