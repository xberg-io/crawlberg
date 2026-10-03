use std::collections::HashMap;
use std::sync::RwLock;
use url::Url;

use crate::adapter::NativeCookie;

pub struct CookieJar {
    cookies: RwLock<HashMap<String, HashMap<CookieKey, CookieEntry>>>,
}

/// A cookie's identity within its domain: the name, the path and the host-only flag. A cookie
/// replaces or deletes another only when all three match, with the domain (RFC 6265bis section
/// 5.7 step 23).
type CookieKey = (String, String, bool);

#[derive(Debug, Clone)]
struct CookieEntry {
    name: String,
    value: String,
    path: String,
    domain: String,
    secure: bool,
    http_only: bool,
    same_site: SameSite,
    /// Set without a `Domain` attribute, so sent to its own host only (RFC 6265 section 5.3
    /// step 6), not to the host's subdomains.
    host_only: bool,
    expires: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SameSite {
    Default,
    Strict,
    Lax,
    None,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CookieRequestContext<'a> {
    site_for_cookies: Option<&'a Url>,
    safe_method: bool,
    top_level: bool,
}

impl<'a> CookieRequestContext<'a> {
    pub(crate) fn top_level(site_for_cookies: Option<&'a Url>, safe_method: bool) -> Self {
        Self {
            site_for_cookies,
            safe_method,
            top_level: true,
        }
    }

    pub(crate) fn subresource(site_for_cookies: Option<&'a Url>) -> Self {
        Self {
            site_for_cookies,
            safe_method: true,
            top_level: false,
        }
    }

    fn is_cross_site(self, request_url: &Url) -> bool {
        self.site_for_cookies
            .is_some_and(|site| !schemeful_same_site(site, request_url))
    }

    fn allows_storage(self, policy: SameSite, request_url: &Url) -> bool {
        policy == SameSite::None || !self.is_cross_site(request_url) || self.top_level
    }
}

impl CookieEntry {
    fn key(&self) -> CookieKey {
        (self.name.clone(), self.path.clone(), self.host_only)
    }

    fn is_sent_to(&self, host: &str) -> bool {
        if self.host_only {
            host.eq_ignore_ascii_case(&self.domain)
        } else {
            domain_matches(host, &self.domain)
        }
    }

    fn same_site_allows(&self, request_url: &Url, context: CookieRequestContext<'_>) -> bool {
        if !context.is_cross_site(request_url) {
            return true;
        }
        match self.same_site {
            SameSite::Strict => false,
            SameSite::Default | SameSite::Lax => context.top_level && context.safe_method,
            SameSite::None => true,
        }
    }
}

impl CookieJar {
    pub fn new() -> Self {
        CookieJar {
            cookies: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_cookie(&self, set_cookie_str: &str, url: &Url) {
        self.set_cookie_for_request(set_cookie_str, url, CookieRequestContext::top_level(None, true));
    }

    pub(crate) fn set_cookie_for_request(&self, set_cookie_str: &str, url: &Url, context: CookieRequestContext<'_>) {
        let Some((name, value, attribute_list)) = split_cookie_string(set_cookie_str) else {
            return;
        };
        let mut attributes = CookieAttributes::defaults_for(url);
        attributes.apply_all(attribute_list);
        if !attributes.may_be_set_by(&name, url) || !context.allows_storage(attributes.same_site, url) {
            return;
        }
        self.commit(name, value, attributes, url);
    }

    /// Store a parsed cookie, honouring the delete sentinel and dropping already-expired cookies.
    /// A page over http cannot replace or delete a Secure cookie of the same name whose domain
    /// and path cover the new cookie (RFC 6265bis section 5.7 step 16).
    fn commit(&self, name: String, value: String, attributes: CookieAttributes, url: &Url) {
        if url.scheme() != "https" && self.shadows_a_secure_cookie(&name, &attributes) {
            return;
        }
        let host_only = attributes.domain_attribute.is_none();
        if let Some(expiry) = attributes.expires {
            if expiry == EXPIRY_DELETE_SENTINEL {
                let mut cookies = self.cookies.write().unwrap();
                if let Some(domain_cookies) = cookies.get_mut(&attributes.domain) {
                    domain_cookies.remove(&(name, attributes.path, host_only));
                }
                return;
            }
            if expiry < unix_now_seconds() {
                return;
            }
        }

        let entry = CookieEntry {
            name: name.clone(),
            value,
            path: attributes.path,
            domain: attributes.domain.clone(),
            secure: attributes.secure,
            http_only: attributes.http_only,
            same_site: attributes.same_site,
            host_only,
            expires: attributes.expires,
        };

        let mut cookies = self.cookies.write().unwrap();
        cookies.entry(attributes.domain).or_default().insert(entry.key(), entry);
    }

    fn shadows_a_secure_cookie(&self, name: &str, attributes: &CookieAttributes) -> bool {
        let cookies = self.cookies.read().unwrap();
        cookies.values().flat_map(HashMap::values).any(|existing| {
            existing.secure
                && existing.name == name
                && (domain_matches(&existing.domain, &attributes.domain)
                    || domain_matches(&attributes.domain, &existing.domain))
                && path_matches(&attributes.path, &existing.path)
        })
    }

    pub fn get_cookie_header(&self, url: &Url) -> String {
        self.get_cookie_header_for_request(url, CookieRequestContext::top_level(None, true))
    }

    pub(crate) fn get_cookie_header_for_request(&self, url: &Url, context: CookieRequestContext<'_>) -> String {
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let is_secure = url.scheme() == "https";
        let cookies = self.cookies.read().unwrap();

        let mut matching: Vec<String> = Vec::new();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        for (domain, domain_cookies) in cookies.iter() {
            if !domain_matches(host, domain) {
                continue;
            }
            for entry in domain_cookies.values().filter(|entry| entry.is_sent_to(host)) {
                if let Some(exp) = entry.expires
                    && exp < now
                {
                    continue;
                }
                if entry.secure && !is_secure {
                    continue;
                }
                if !path_matches(path, &entry.path) {
                    continue;
                }
                if !entry.same_site_allows(url, context) {
                    continue;
                }
                matching.push(format!("{}={}", entry.name, entry.value));
            }
        }

        matching.join("; ")
    }

    pub fn get_all_cookies(&self) -> Vec<CookieInfo> {
        let cookies = self.cookies.read().unwrap();
        let mut result = Vec::new();
        for domain_cookies in cookies.values() {
            for entry in domain_cookies.values() {
                result.push(CookieInfo {
                    name: entry.name.clone(),
                    value: entry.value.clone(),
                    domain: entry.domain.clone(),
                    path: entry.path.clone(),
                    secure: entry.secure,
                    http_only: entry.http_only,
                });
            }
        }
        result
    }

    pub fn set_cookies_from_cdp(&self, cookies: Vec<CookieInfo>) {
        let mut jar = self.cookies.write().unwrap();
        for cookie in cookies {
            let entry = CookieEntry {
                name: cookie.name.clone(),
                value: cookie.value,
                path: cookie.path,
                domain: cookie.domain.clone(),
                secure: cookie.secure,
                http_only: cookie.http_only,
                same_site: SameSite::Default,
                host_only: false,
                expires: None,
            };
            jar.entry(cookie.domain).or_default().insert(entry.key(), entry);
        }
    }

    pub fn get_js_visible_cookies(&self, url: &Url) -> String {
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let is_secure = url.scheme() == "https";
        let cookies = self.cookies.read().unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut matching: Vec<String> = Vec::new();

        for (domain, domain_cookies) in cookies.iter() {
            if !domain_matches(host, domain) {
                continue;
            }
            for entry in domain_cookies.values().filter(|entry| entry.is_sent_to(host)) {
                if entry.http_only {
                    continue;
                }
                if let Some(exp) = entry.expires
                    && exp < now
                {
                    continue;
                }
                if entry.secure && !is_secure {
                    continue;
                }
                if !path_matches(path, &entry.path) {
                    continue;
                }
                matching.push(format!("{}={}", entry.name, entry.value));
            }
        }

        matching.join("; ")
    }

    pub fn set_cookie_from_js(&self, cookie_str: &str, url: &Url) {
        let Some((name, value, attribute_list)) = split_cookie_string(cookie_str) else {
            return;
        };
        let mut attributes = CookieAttributes::defaults_for(url);
        attributes.apply_all(attribute_list);
        // ~keep A `document.cookie` assignment can never mark a cookie HttpOnly: per RFC 6265 the
        // attribute is only meaningful on a Set-Cookie header, and honouring it here would let a
        // page hide a cookie from its own script and from `get_js_visible_cookies`.
        attributes.http_only = false;
        if !attributes.may_be_set_by(&name, url) {
            return;
        }
        self.commit(name, value, attributes, url);
    }

    /// Insert a cookie from pre-parsed fields (not a raw Set-Cookie header).
    pub fn set_parsed_cookie(&self, cookie: &NativeCookie) {
        let domain = cookie
            .domain
            .as_deref()
            .unwrap_or("")
            .trim_start_matches('.')
            .to_lowercase();
        let entry = CookieEntry {
            name: cookie.name.clone(),
            value: cookie.value.clone(),
            path: cookie.path.clone().unwrap_or_else(|| "/".to_string()),
            domain: domain.clone(),
            secure: cookie.secure,
            http_only: cookie.http_only,
            same_site: SameSite::Default,
            host_only: cookie.host_only,
            expires: None,
        };
        let mut cookies = self.cookies.write().unwrap();
        cookies.entry(domain).or_default().insert(entry.key(), entry);
    }

    /// Snapshot all non-expired cookies as flat tuples; the last field is the host-only flag.
    pub fn snapshot(&self) -> Vec<(String, String, String, String, bool, bool, bool)> {
        let cookies = self.cookies.read().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut result = Vec::new();
        for domain_cookies in cookies.values() {
            for entry in domain_cookies.values() {
                if let Some(exp) = entry.expires
                    && exp < now
                {
                    continue;
                }
                result.push((
                    entry.name.clone(),
                    entry.value.clone(),
                    entry.domain.clone(),
                    entry.path.clone(),
                    entry.secure,
                    entry.http_only,
                    entry.host_only,
                ));
            }
        }
        result
    }

    pub fn clear(&self) {
        self.cookies.write().unwrap().clear();
    }
}

impl Default for CookieJar {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CookieInfo {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
}

/// Expiry value standing for "delete this cookie now", produced by `Max-Age` values <= 0.
const EXPIRY_DELETE_SENTINEL: u64 = 0;

fn unix_now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Split `name=value; attr=val; flag` into its name, value and the unparsed attribute list.
///
/// Returns `None` when the leading pair has no `=`, which discards the whole cookie.
fn split_cookie_string(cookie_str: &str) -> Option<(String, String, &str)> {
    let mut parts = cookie_str.splitn(2, ';');
    let (name, value) = parts.next()?.trim().split_once('=')?;
    Some((
        name.trim().to_string(),
        value.trim().to_string(),
        parts.next().unwrap_or(""),
    ))
}

/// Cookie attributes, seeded from the request URL and then overwritten by the cookie string.
struct CookieAttributes {
    domain: String,
    /// The cookie's own `Domain` value, before any check against the request host.
    domain_attribute: Option<String>,
    path: String,
    /// Whether a `Path` attribute that starts with `/` set `path`, rather than the default path.
    path_attribute: bool,
    secure: bool,
    http_only: bool,
    expires: Option<u64>,
    same_site: SameSite,
}

impl CookieAttributes {
    fn defaults_for(url: &Url) -> Self {
        CookieAttributes {
            domain: url.host_str().unwrap_or("").to_lowercase(),
            domain_attribute: None,
            path: default_path(url),
            path_attribute: false,
            secure: false,
            http_only: false,
            expires: None,
            same_site: SameSite::Default,
        }
    }

    /// Apply every attribute in order, so a repeated attribute is won by its last occurrence.
    fn apply_all(&mut self, attribute_list: &str) {
        for attribute in attribute_list.split(';') {
            let attribute = attribute.trim();
            match attribute.split_once('=') {
                Some((key, value)) => self.apply_keyed(key.trim(), value.trim()),
                None => self.apply_flag(attribute),
            }
        }
    }

    fn apply_keyed(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            // ~keep An empty Domain value is ignored, which leaves the cookie on the request host
            // ~keep (RFC 6265 section 5.2.3).
            "domain" if !value.trim_start_matches('.').is_empty() => {
                self.domain = ascii_domain(value.trim_start_matches('.'));
                self.domain_attribute = Some(value.to_string());
            }
            // ~keep A Path that does not start with `/` leaves the default path (RFC 6265bis
            // ~keep section 5.6.4).
            "path" if value.starts_with('/') => {
                self.path = value.to_string();
                self.path_attribute = true;
            }
            "path" => {}
            "expires" => {
                if let Ok(timestamp) = parse_http_date(value) {
                    self.expires = Some(timestamp);
                }
            }
            "max-age" => {
                if let Some(expiry) = max_age_to_expiry(value) {
                    self.expires = Some(expiry);
                }
            }
            "samesite" => {
                self.same_site = match value.to_ascii_lowercase().as_str() {
                    "strict" => SameSite::Strict,
                    "lax" => SameSite::Lax,
                    "none" => SameSite::None,
                    _ => SameSite::Default,
                };
            }
            _ => {}
        }
    }

    /// Whether a page at `url` may set a cookie with these attributes. A `Domain` value must
    /// domain-match the request host, so a host cannot set a cookie for another host (RFC 6265
    /// section 5.3 step 6), and an IP address matches no other name. A `Secure` cookie must come
    /// over https (the RFC 6265bis storage model). A `Domain` value that is a public suffix
    /// (`co.uk`, `github.io`, `localhost`) is refused, unless it equals the request host, which
    /// makes the cookie host-only (RFC 6265bis section 5.7 step 9). A `__Secure-` name needs
    /// `Secure`, and a `__Host-` name also needs no `Domain` and `Path=/` (RFC 6265bis section
    /// 4.1.3), with the prefix matched without regard to case.
    fn may_be_set_by(&mut self, name: &str, url: &Url) -> bool {
        if self.secure && url.scheme() != "https" {
            return false;
        }
        if self.same_site == SameSite::None && !self.secure {
            return false;
        }
        let prefix = name.get(..PREFIX_HOST.len()).unwrap_or("");
        if prefix.eq_ignore_ascii_case(PREFIX_HOST)
            && !(self.secure && self.domain_attribute.is_none() && self.path_attribute && self.path == "/")
        {
            return false;
        }
        let prefix = name.get(..PREFIX_SECURE.len()).unwrap_or("");
        if prefix.eq_ignore_ascii_case(PREFIX_SECURE) && !self.secure {
            return false;
        }
        let Some(value) = self.domain_attribute.as_deref() else {
            return true;
        };
        if psl::suffix(self.domain.as_bytes()).is_some_and(|suffix| suffix.as_bytes() == self.domain.as_bytes()) {
            if !url
                .host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case(&self.domain))
            {
                return false;
            }
            self.domain_attribute = None;
            return true;
        }
        cookie_store::CookieDomain::try_from(value).is_ok_and(|domain| domain.matches(url))
    }

    fn apply_flag(&mut self, flag: &str) {
        match flag.to_lowercase().as_str() {
            "secure" => self.secure = true,
            "httponly" => self.http_only = true,
            _ => {}
        }
    }
}

/// Convert a `Max-Age` value to an absolute expiry, or `None` when it does not parse.
fn max_age_to_expiry(value: &str) -> Option<u64> {
    let seconds = value.parse::<i64>().ok()?;
    if seconds <= 0 {
        return Some(EXPIRY_DELETE_SENTINEL);
    }
    Some(unix_now_seconds() + seconds as u64)
}

fn parse_http_date(s: &str) -> Result<u64, ()> {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];

    let s = s.replace('-', " ");
    let parts: Vec<&str> = s.split_whitespace().collect();

    if parts.len() < 5 {
        return Err(());
    }

    let day: u64 = parts[1].parse().map_err(|_| ())?;
    let month = months
        .iter()
        .position(|m| parts[2].to_lowercase().starts_with(m))
        .ok_or(())? as u64
        + 1;
    let year: u64 = parts[3].parse().map_err(|_| ())?;

    let time_parts: Vec<&str> = parts[4].split(':').collect();
    let hour: u64 = time_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minute: u64 = time_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let second: u64 = time_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    let mut days_total: u64 = 0;
    for y in 1970..year {
        days_total += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
    }
    let days_in_month = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    for m in 1..month {
        days_total += days_in_month[m as usize] + if m == 2 && is_leap { 1 } else { 0 };
    }
    days_total += day - 1;

    Ok(days_total * 86400 + hour * 3600 + minute * 60 + second)
}

const PREFIX_HOST: &str = "__Host-";
const PREFIX_SECURE: &str = "__Secure-";

/// The path a cookie gets without a usable `Path` attribute: the request path up to, but not
/// including, its last `/`, or `/` (RFC 6265bis section 5.1.4).
fn default_path(url: &Url) -> String {
    let path = url.path();
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(last) => path[..last].to_string(),
    }
}

/// Whether `request_path` is `cookie_path` or lies under it at a `/` boundary (RFC 6265bis
/// section 5.1.4), so `/only` covers `/only/x` but not `/onlyfoo`.
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path
        .strip_prefix(cookie_path)
        .is_some_and(|rest| rest.is_empty() || cookie_path.ends_with('/') || rest.starts_with('/'))
}

/// A `Domain` value in the ASCII form a `Url` host has, so `пример.рф` compares equal to the
/// host `xn--e1afmkfd.xn--p1ai` (RFC 6265bis section 5.1.2). A value that is no valid
/// domain name, such as an IP address, is only lowercased.
fn ascii_domain(value: &str) -> String {
    match url::Host::parse(value) {
        Ok(url::Host::Domain(domain)) => domain,
        _ => value.to_lowercase(),
    }
}

fn domain_matches(host: &str, domain: &str) -> bool {
    let host = host.to_lowercase();
    let domain = domain.trim_start_matches('.').to_lowercase();
    host == domain || host.ends_with(&format!(".{}", domain))
}

fn schemeful_same_site(left: &Url, right: &Url) -> bool {
    if left.scheme() != right.scheme() {
        return false;
    }
    let Some(left_host) = left.host_str() else {
        return false;
    };
    let Some(right_host) = right.host_str() else {
        return false;
    };
    let left_site = psl::domain_str(left_host).unwrap_or(left_host);
    let right_site = psl::domain_str(right_host).unwrap_or(right_host);
    left_site.eq_ignore_ascii_case(right_site)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/path").unwrap();
        jar.set_cookie("session=abc123; Path=/; Secure; HttpOnly", &url);

        let header = jar.get_cookie_header(&url);
        assert!(header.contains("session=abc123"));
    }

    #[test]
    fn test_cookie_domain_matching() {
        let jar = CookieJar::new();
        let url = Url::parse("https://www.example.com/").unwrap();
        jar.set_cookie("token=xyz; Domain=example.com", &url);

        let header = jar.get_cookie_header(&url);
        assert!(header.contains("token=xyz"));

        let sub_url = Url::parse("https://api.example.com/").unwrap();
        let header2 = jar.get_cookie_header(&sub_url);
        assert!(header2.contains("token=xyz"));

        let other_url = Url::parse("https://other.com/").unwrap();
        let header3 = jar.get_cookie_header(&other_url);
        assert!(header3.is_empty());
    }

    #[test]
    fn test_cdp_cookie_with_leading_dot_domain_matches_requests() {
        let jar = CookieJar::new();
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "token".to_string(),
            value: "xyz".to_string(),
            domain: ".example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
        }]);

        let apex_url = Url::parse("https://example.com/").unwrap();
        let apex_header = jar.get_cookie_header(&apex_url);
        assert!(apex_header.contains("token=xyz"));

        let subdomain_url = Url::parse("https://api.example.com/").unwrap();
        let subdomain_header = jar.get_cookie_header(&subdomain_url);
        assert!(subdomain_header.contains("token=xyz"));

        let other_url = Url::parse("https://other.com/").unwrap();
        let other_header = jar.get_cookie_header(&other_url);
        assert!(other_header.is_empty());
    }

    #[test]
    fn test_secure_cookie_not_sent_over_http() {
        let jar = CookieJar::new();
        let https_url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("secure_token=secret; Secure", &https_url);

        let http_url = Url::parse("http://example.com/").unwrap();
        let header = jar.get_cookie_header(&http_url);
        assert!(header.is_empty());
    }

    #[test]
    fn test_max_age_zero_deletes_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("session=abc", &url);
        assert!(jar.get_cookie_header(&url).contains("session=abc"));

        jar.set_cookie("session=abc; Max-Age=0", &url);
        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn test_max_age_sets_expiry() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("token=xyz; Max-Age=3600", &url);
        assert!(jar.get_cookie_header(&url).contains("token=xyz"));
    }

    #[test]
    fn test_expired_cookie_not_sent() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("old=gone; Expires=Thu, 01 Jan 2020 00:00:00 GMT", &url);
        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn test_clear_cookies() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("a=1", &url);
        assert!(!jar.get_cookie_header(&url).is_empty());

        jar.clear();
        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn set_cookie_hides_httponly_cookies_from_javascript_but_still_sends_them() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("session=abc; HttpOnly", &url);

        assert_eq!(jar.get_cookie_header(&url), "session=abc");
        assert_eq!(jar.get_js_visible_cookies(&url), "");
    }

    #[test]
    fn set_cookie_from_js_ignores_the_httponly_attribute() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie_from_js("session=abc; HttpOnly", &url);

        assert_eq!(jar.get_js_visible_cookies(&url), "session=abc");
        assert!(!jar.snapshot()[0].5, "document.cookie cannot set HttpOnly");
    }

    #[test]
    fn set_cookie_from_js_applies_domain_path_and_secure_attributes() {
        let jar = CookieJar::new();
        let url = Url::parse("https://www.example.com/app/page").unwrap();
        jar.set_cookie_from_js("token=xyz; Domain=.example.com; Path=/app; Secure", &url);

        let snapshot = jar.snapshot();
        assert_eq!(snapshot.len(), 1);
        let (name, value, domain, path, secure, http_only, host_only) = snapshot[0].clone();
        assert!(!host_only, "a Domain attribute makes a domain cookie");
        assert_eq!(name, "token");
        assert_eq!(value, "xyz");
        assert_eq!(domain, "example.com", "leading dot is stripped");
        assert_eq!(path, "/app");
        assert!(secure);
        assert!(!http_only);

        assert_eq!(jar.get_js_visible_cookies(&url), "token=xyz");
        let http_url = Url::parse("http://www.example.com/app/page").unwrap();
        assert_eq!(jar.get_js_visible_cookies(&http_url), "", "Secure blocks plain http");
        let other_path = Url::parse("https://www.example.com/other").unwrap();
        assert_eq!(jar.get_js_visible_cookies(&other_path), "", "Path prefix must match");
    }

    #[test]
    fn set_cookie_from_js_with_max_age_zero_deletes_the_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie_from_js("session=abc", &url);
        assert_eq!(jar.get_js_visible_cookies(&url), "session=abc");

        jar.set_cookie_from_js("session=abc; Max-Age=0", &url);
        assert_eq!(jar.get_js_visible_cookies(&url), "");
    }

    #[test]
    fn set_cookie_from_js_with_expired_expires_is_dropped_without_storing() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie_from_js("old=gone; Expires=Thu, 01 Jan 2020 00:00:00 GMT", &url);
        assert_eq!(jar.snapshot().len(), 0);
    }

    #[test]
    fn set_cookie_from_js_keeps_a_future_max_age_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie_from_js("token=xyz; Max-Age=3600", &url);
        assert_eq!(jar.get_js_visible_cookies(&url), "token=xyz");
    }

    #[test]
    fn unparsable_max_age_and_expires_leave_the_cookie_session_scoped() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("a=1; Max-Age=not-a-number", &url);
        jar.set_cookie("b=2; Expires=not-a-date", &url);
        jar.set_cookie_from_js("c=3; Max-Age=not-a-number", &url);

        let mut names: Vec<String> = jar.snapshot().into_iter().map(|entry| entry.0).collect();
        names.sort();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_cookie_string_without_an_equals_sign_is_ignored_on_both_paths() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("justaname; Path=/", &url);
        jar.set_cookie_from_js("justaname; Path=/", &url);
        assert_eq!(jar.snapshot().len(), 0);
    }

    #[test]
    fn a_repeated_attribute_is_won_by_the_last_occurrence() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/a/b/c").unwrap();
        jar.set_cookie("a=1; Path=/a; Path=/a/b", &url);
        jar.set_cookie_from_js("b=2; Path=/a; Path=/a/b", &url);

        let mut paths: Vec<String> = jar.snapshot().into_iter().map(|entry| entry.3).collect();
        paths.sort();
        assert_eq!(paths, vec!["/a/b", "/a/b"]);
    }

    #[test]
    fn unknown_attributes_and_valueless_flags_are_skipped_without_affecting_the_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("a=1; Priority=High; Partitioned", &url);
        jar.set_cookie_from_js("b=2; Priority=High; Partitioned", &url);

        let snapshot = jar.snapshot();
        assert_eq!(snapshot.len(), 2);
        for entry in snapshot {
            assert!(!entry.4, "no unknown attribute should set Secure");
            assert!(!entry.5, "no unknown attribute should set HttpOnly");
            assert_eq!(entry.3, "/", "path stays the request path");
        }
    }

    #[test]
    fn a_domain_attribute_that_the_request_host_does_not_match_is_ignored() {
        let jar = CookieJar::new();
        let url = Url::parse("http://www.example.com/").unwrap();
        jar.set_cookie("a=1; Domain=other.com", &url);
        jar.set_cookie("b=2; Domain=api.example.com", &url);
        jar.set_cookie_from_js("c=3; Domain=other.com", &url);
        assert_eq!(jar.snapshot().len(), 0);
        assert_eq!(jar.get_cookie_header(&Url::parse("http://other.com/").unwrap()), "");
    }

    #[test]
    fn an_ip_address_host_sets_no_domain_cookie_for_another_name() {
        let jar = CookieJar::new();
        let url = Url::parse("http://127.0.0.1/").unwrap();
        jar.set_cookie("inj=1; Domain=localhost", &url);
        jar.set_cookie("suffix=1; Domain=0.0.1", &url);
        assert_eq!(jar.snapshot().len(), 0);

        jar.set_cookie("own=1; Domain=127.0.0.1", &url);
        assert_eq!(jar.get_cookie_header(&url), "own=1", "an IP host may name itself");
    }

    #[test]
    fn an_empty_domain_attribute_leaves_the_cookie_on_the_request_host() {
        let jar = CookieJar::new();
        let url = Url::parse("http://example.com/").unwrap();
        jar.set_cookie("a=1; Path=/; Domain=", &url);
        jar.set_cookie("b=2; Path=/; Domain=.", &url);
        let mut stored: Vec<(String, String)> = jar.snapshot().into_iter().map(|entry| (entry.0, entry.2)).collect();
        stored.sort();
        assert_eq!(
            stored,
            [
                ("a".to_owned(), "example.com".to_owned()),
                ("b".to_owned(), "example.com".to_owned())
            ]
        );
    }

    #[test]
    fn a_cookie_without_a_domain_attribute_is_sent_to_its_own_host_only() {
        let jar = CookieJar::new();
        let url = Url::parse("http://a.example.com/").unwrap();
        jar.set_cookie("host=1; Path=/", &url);
        jar.set_cookie("domain=1; Path=/; Domain=a.example.com", &url);
        jar.set_cookie_from_js("js=1; Path=/", &url);
        let sub = Url::parse("http://b.a.example.com/").unwrap();
        assert_eq!(jar.get_cookie_header(&sub), "domain=1");
        assert_eq!(jar.get_js_visible_cookies(&sub), "domain=1");

        let header = jar.get_cookie_header(&url);
        let mut own: Vec<&str> = header.split("; ").collect();
        own.sort_unstable();
        assert_eq!(own, ["domain=1", "host=1", "js=1"], "the host itself gets all three");
    }

    #[test]
    fn a_parsed_host_only_cookie_stays_host_only() {
        let jar = CookieJar::new();
        jar.set_parsed_cookie(&NativeCookie {
            name: "host".into(),
            value: "1".into(),
            domain: Some("a.example.com".into()),
            path: Some("/".into()),
            secure: false,
            http_only: false,
            host_only: true,
        });
        assert_eq!(
            jar.get_cookie_header(&Url::parse("http://b.a.example.com/").unwrap()),
            ""
        );
        assert_eq!(
            jar.get_cookie_header(&Url::parse("http://a.example.com/").unwrap()),
            "host=1"
        );
        assert!(jar.snapshot()[0].6, "the snapshot keeps the host-only flag");
    }

    #[test]
    fn a_domain_attribute_that_is_a_public_suffix_is_ignored() {
        let jar = CookieJar::new();
        for (cookie, from, victim) in [
            (
                "psl=1; Path=/; Domain=co.uk",
                "http://www.example.co.uk/",
                "http://victim.co.uk/",
            ),
            (
                "gh=1; Path=/; Domain=github.io",
                "http://evil.github.io/",
                "http://victim.github.io/",
            ),
            ("tld=1; Path=/; Domain=com", "http://evil.com/", "http://victim.com/"),
            (
                "lh=1; Path=/; Domain=localhost",
                "http://a.localhost/",
                "http://b.localhost/",
            ),
        ] {
            jar.set_cookie(cookie, &Url::parse(from).unwrap());
            jar.set_cookie_from_js(cookie, &Url::parse(from).unwrap());
            assert_eq!(
                jar.get_cookie_header(&Url::parse(victim).unwrap()),
                "",
                "{cookie} from {from}"
            );
        }
        assert_eq!(jar.snapshot().len(), 0);
    }

    #[test]
    fn a_public_suffix_domain_equal_to_the_request_host_makes_the_cookie_host_only() {
        let jar = CookieJar::new();
        let url = Url::parse("http://localhost/").unwrap();
        jar.set_cookie("own=1; Path=/; Domain=localhost", &url);
        assert_eq!(jar.get_cookie_header(&url), "own=1");
        assert_eq!(jar.get_cookie_header(&Url::parse("http://b.localhost/").unwrap()), "");
        assert!(jar.snapshot()[0].6, "the cookie is host-only");
    }

    #[test]
    fn a_secure_cookie_set_over_plain_http_is_ignored() {
        let jar = CookieJar::new();
        let url = Url::parse("http://example.com/").unwrap();
        jar.set_cookie("sec=s; Path=/; Secure", &url);
        jar.set_cookie_from_js("js=s; Path=/; Secure", &url);
        assert_eq!(jar.snapshot().len(), 0);
    }

    fn sorted_header(jar: &CookieJar, url: &str) -> Vec<String> {
        let header = jar.get_cookie_header(&Url::parse(url).unwrap());
        let mut cookies: Vec<String> = header.split("; ").filter(|c| !c.is_empty()).map(String::from).collect();
        cookies.sort_unstable();
        cookies
    }

    #[test]
    fn a_cookie_with_another_path_or_host_only_flag_does_not_replace_the_first() {
        let jar = CookieJar::new();
        let url = Url::parse("http://example.com/").unwrap();
        jar.set_cookie("a=1; Path=/x", &url);
        jar.set_cookie("a=2; Path=/y", &url);
        jar.set_cookie("b=1; Path=/", &url);
        jar.set_cookie("b=2; Path=/; Domain=example.com", &url);
        assert_eq!(
            jar.snapshot().len(),
            4,
            "the jar keeps four cookies: {:?}",
            jar.snapshot()
        );
        assert_eq!(sorted_header(&jar, "http://example.com/x"), ["a=1", "b=1", "b=2"]);
    }

    #[test]
    fn a_deletion_for_another_path_leaves_the_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("http://example.com/").unwrap();
        jar.set_cookie("keep=1; Path=/", &url);
        jar.set_cookie("drop=1; Path=/", &url);
        jar.set_cookie("keep=; Path=/other; Max-Age=0", &url);
        jar.set_cookie("drop=; Path=/; Max-Age=0", &url);
        assert_eq!(sorted_header(&jar, "http://example.com/n"), ["keep=1"]);
    }

    #[test]
    fn a_path_matches_only_on_a_slash_boundary() {
        let jar = CookieJar::new();
        let url = Url::parse("http://example.com/").unwrap();
        jar.set_cookie("only=1; Path=/only", &url);
        jar.set_cookie("slash=1; Path=/dir/", &url);
        assert_eq!(sorted_header(&jar, "http://example.com/onlyfoo"), Vec::<String>::new());
        assert_eq!(sorted_header(&jar, "http://example.com/only"), ["only=1"]);
        assert_eq!(sorted_header(&jar, "http://example.com/only/x"), ["only=1"]);
        assert_eq!(sorted_header(&jar, "http://example.com/dir/x"), ["slash=1"]);
    }

    #[test]
    fn a_missing_or_relative_path_uses_the_default_path_of_the_request() {
        let jar = CookieJar::new();
        jar.set_cookie("dflt=1", &Url::parse("http://example.com/dir/a").unwrap());
        jar.set_cookie("rel=1; Path=nope", &Url::parse("http://example.com/dir/a").unwrap());
        jar.set_cookie_from_js("js=1", &Url::parse("http://example.com/dir/a").unwrap());
        jar.set_cookie("root=1", &Url::parse("http://example.com/top").unwrap());
        assert_eq!(
            sorted_header(&jar, "http://example.com/dir/b"),
            ["dflt=1", "js=1", "rel=1", "root=1"]
        );
        assert_eq!(sorted_header(&jar, "http://example.com/other"), ["root=1"]);
    }

    #[test]
    fn an_http_page_does_not_overwrite_or_delete_a_secure_cookie() {
        let jar = CookieJar::new();
        jar.set_cookie("s=1; Path=/; Secure", &Url::parse("https://example.com/").unwrap());
        let http = Url::parse("http://example.com/").unwrap();
        jar.set_cookie("s=2; Path=/", &http);
        jar.set_cookie_from_js("s=3; Path=/", &http);
        jar.set_cookie("s=; Path=/; Max-Age=0", &http);
        jar.set_cookie("s=4; Path=/deeper", &http);
        assert_eq!(sorted_header(&jar, "https://example.com/deeper"), ["s=1"]);
        jar.set_cookie(
            "s=5; Path=/other; Domain=other.com",
            &Url::parse("http://other.com/").unwrap(),
        );
        jar.set_cookie("s=6; Path=/", &Url::parse("https://example.com/").unwrap());
        assert_eq!(
            sorted_header(&jar, "https://example.com/"),
            ["s=6"],
            "https may replace it"
        );
    }

    #[test]
    fn a_prefixed_cookie_that_breaks_its_prefix_rules_is_ignored() {
        let jar = CookieJar::new();
        let url = Url::parse("https://www.example.com/dir/").unwrap();
        jar.set_cookie("__Host-x=1; Path=/; Domain=example.com; Secure", &url);
        jar.set_cookie("__Host-p=1; Path=/dir; Secure", &url);
        jar.set_cookie("__Host-n=1; Secure", &url);
        jar.set_cookie("__host-l=1; Path=/", &url);
        jar.set_cookie("__Secure-y=1; Path=/", &url);
        jar.set_cookie_from_js("__secure-j=1; Path=/", &url);
        assert_eq!(jar.snapshot().len(), 0, "stored: {:?}", jar.snapshot());
        jar.set_cookie("__Host-ok=1; Path=/; Secure", &url);
        jar.set_cookie("__Secure-ok=1; Path=/; Domain=example.com; Secure", &url);
        assert_eq!(
            sorted_header(&jar, "https://www.example.com/"),
            ["__Host-ok=1", "__Secure-ok=1"]
        );
    }

    #[test]
    fn a_unicode_domain_is_stored_in_the_ascii_form_of_the_host() {
        let jar = CookieJar::new();
        let url = Url::parse("http://www.пример.рф/").unwrap();
        jar.set_cookie("u=1; Path=/; Domain=пример.рф", &url);
        jar.set_cookie("suffix=1; Path=/; Domain=рф", &url);
        assert_eq!(sorted_header(&jar, "http://www.пример.рф/"), ["u=1"]);
        assert_eq!(sorted_header(&jar, "http://xn--e1afmkfd.xn--p1ai/"), ["u=1"]);
    }

    #[test]
    fn strict_and_lax_cookies_follow_cross_site_navigation_rules() {
        let jar = CookieJar::new();
        let destination = Url::parse("https://accounts.example.com/finish").unwrap();
        jar.set_cookie("strict=1; Path=/; SameSite=Strict; Secure", &destination);
        jar.set_cookie("lax=1; Path=/; SameSite=Lax; Secure", &destination);
        jar.set_cookie("none=1; Path=/; SameSite=None; Secure", &destination);
        let cross_site = Url::parse("https://other.example.net/start").unwrap();

        let get_header =
            jar.get_cookie_header_for_request(&destination, CookieRequestContext::top_level(Some(&cross_site), true));
        let mut get_cookies: Vec<&str> = get_header.split("; ").collect();
        get_cookies.sort_unstable();
        assert_eq!(get_cookies, ["lax=1", "none=1"]);

        assert_eq!(
            jar.get_cookie_header_for_request(&destination, CookieRequestContext::top_level(Some(&cross_site), false),),
            "none=1"
        );
    }

    #[test]
    fn same_site_is_schemeful_and_uses_the_registrable_domain() {
        let jar = CookieJar::new();
        let destination = Url::parse("https://login.example.co.uk/finish").unwrap();
        jar.set_cookie("strict=1; Path=/; SameSite=Strict; Secure", &destination);

        let sibling = Url::parse("https://shop.example.co.uk/start").unwrap();
        assert_eq!(
            jar.get_cookie_header_for_request(&destination, CookieRequestContext::top_level(Some(&sibling), true),),
            "strict=1"
        );

        let insecure = Url::parse("http://shop.example.co.uk/start").unwrap();
        assert_eq!(
            jar.get_cookie_header_for_request(&destination, CookieRequestContext::top_level(Some(&insecure), true),),
            ""
        );
    }

    #[test]
    fn same_site_none_requires_secure() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("rejected=1; SameSite=None", &url);
        jar.set_cookie("accepted=1; SameSite=None; Secure", &url);

        assert_eq!(jar.get_cookie_header(&url), "accepted=1");
    }

    #[test]
    fn default_and_invalid_same_site_values_are_lax_like() {
        let jar = CookieJar::new();
        let destination = Url::parse("https://example.com/finish").unwrap();
        let cross_site = Url::parse("https://other.test/start").unwrap();
        jar.set_cookie("default=1; Secure", &destination);
        jar.set_cookie("invalid=1; SameSite=maybe; Secure", &destination);

        let top_level_get = CookieRequestContext::top_level(Some(&cross_site), true);
        let header = jar.get_cookie_header_for_request(&destination, top_level_get);
        let mut cookies: Vec<&str> = header.split("; ").collect();
        cookies.sort_unstable();
        assert_eq!(cookies, ["default=1", "invalid=1"]);
        assert_eq!(
            jar.get_cookie_header_for_request(&destination, CookieRequestContext::top_level(Some(&cross_site), false),),
            ""
        );
        assert_eq!(
            jar.get_cookie_header_for_request(&destination, CookieRequestContext::subresource(Some(&cross_site)),),
            ""
        );
    }

    #[test]
    fn cross_site_subresource_cannot_store_restricted_same_site_cookies() {
        let jar = CookieJar::new();
        let resource = Url::parse("https://cdn.example/image").unwrap();
        let top_level = Url::parse("https://other.test/page").unwrap();
        let context = CookieRequestContext::subresource(Some(&top_level));

        jar.set_cookie_for_request("strict=1; SameSite=Strict; Secure", &resource, context);
        jar.set_cookie_for_request("lax=1; SameSite=Lax; Secure", &resource, context);
        jar.set_cookie_for_request("default=1; Secure", &resource, context);
        jar.set_cookie_for_request("none=1; SameSite=None; Secure", &resource, context);

        assert_eq!(jar.get_cookie_header(&resource), "none=1");
    }
}
