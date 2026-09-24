use std::collections::HashMap;
use std::sync::RwLock;
use url::Url;

pub struct CookieJar {
    cookies: RwLock<HashMap<String, HashMap<String, CookieEntry>>>,
}

#[derive(Debug, Clone)]
struct CookieEntry {
    name: String,
    value: String,
    path: String,
    domain: String,
    secure: bool,
    http_only: bool,
    expires: Option<u64>,
}

impl CookieJar {
    pub fn new() -> Self {
        CookieJar {
            cookies: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_cookie(&self, set_cookie_str: &str, url: &Url) {
        let Some((name, value, attribute_list)) = split_cookie_string(set_cookie_str) else {
            return;
        };
        let mut attributes = CookieAttributes::defaults_for(url);
        attributes.apply_all(attribute_list);
        self.commit(name, value, attributes);
    }

    /// Store a parsed cookie, honouring the delete sentinel and dropping already-expired cookies.
    fn commit(&self, name: String, value: String, attributes: CookieAttributes) {
        if let Some(expiry) = attributes.expires {
            if expiry == EXPIRY_DELETE_SENTINEL {
                let mut cookies = self.cookies.write().unwrap();
                if let Some(domain_cookies) = cookies.get_mut(&attributes.domain) {
                    domain_cookies.remove(&name);
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
            expires: attributes.expires,
        };

        let mut cookies = self.cookies.write().unwrap();
        cookies.entry(attributes.domain).or_default().insert(name, entry);
    }

    pub fn get_cookie_header(&self, url: &Url) -> String {
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
            for entry in domain_cookies.values() {
                if let Some(exp) = entry.expires
                    && exp < now
                {
                    continue;
                }
                if entry.secure && !is_secure {
                    continue;
                }
                if !path.starts_with(&entry.path) {
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
                expires: None,
            };
            jar.entry(cookie.domain).or_default().insert(cookie.name, entry);
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
            for entry in domain_cookies.values() {
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
                if !path.starts_with(&entry.path) {
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
        self.commit(name, value, attributes);
    }

    pub fn delete_cookie(&self, name: &str, domain: &str) {
        let mut cookies = self.cookies.write().unwrap();
        if domain.is_empty() {
            for domain_cookies in cookies.values_mut() {
                domain_cookies.remove(name);
            }
        } else {
            let domains_to_try = [
                domain.to_string(),
                format!(".{}", domain.trim_start_matches('.')),
                domain.trim_start_matches('.').to_string(),
            ];
            for d in &domains_to_try {
                if let Some(domain_cookies) = cookies.get_mut(d.as_str()) {
                    domain_cookies.remove(name);
                }
            }
        }
    }

    /// Insert a cookie from pre-parsed fields (not a raw Set-Cookie header).
    pub fn set_parsed_cookie(
        &self,
        name: &str,
        value: &str,
        domain: Option<&str>,
        path: Option<&str>,
        secure: bool,
        http_only: bool,
    ) {
        let domain = domain.unwrap_or("").trim_start_matches('.').to_lowercase();
        let path = path.unwrap_or("/").to_string();
        let entry = CookieEntry {
            name: name.to_string(),
            value: value.to_string(),
            path,
            domain: domain.clone(),
            secure,
            http_only,
            expires: None,
        };
        let mut cookies = self.cookies.write().unwrap();
        cookies.entry(domain).or_default().insert(name.to_string(), entry);
    }

    /// Snapshot all non-expired cookies as flat tuples.
    pub fn snapshot(&self) -> Vec<(String, String, String, String, bool, bool)> {
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
    path: String,
    secure: bool,
    http_only: bool,
    expires: Option<u64>,
}

impl CookieAttributes {
    fn defaults_for(url: &Url) -> Self {
        CookieAttributes {
            domain: url.host_str().unwrap_or("").to_lowercase(),
            path: url.path().to_string(),
            secure: false,
            http_only: false,
            expires: None,
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
            "domain" => self.domain = value.trim_start_matches('.').to_lowercase(),
            "path" => self.path = value.to_string(),
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
            _ => {}
        }
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

fn domain_matches(host: &str, domain: &str) -> bool {
    let host = host.to_lowercase();
    let domain = domain.trim_start_matches('.').to_lowercase();
    host == domain || host.ends_with(&format!(".{}", domain))
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
        let (name, value, domain, path, secure, http_only) = snapshot[0].clone();
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
        jar.set_cookie("a=1; SameSite=Lax; Priority=High; Partitioned", &url);
        jar.set_cookie_from_js("b=2; SameSite=Lax; Partitioned", &url);

        let snapshot = jar.snapshot();
        assert_eq!(snapshot.len(), 2);
        for entry in snapshot {
            assert!(!entry.4, "no unknown attribute should set Secure");
            assert!(!entry.5, "no unknown attribute should set HttpOnly");
            assert_eq!(entry.3, "/", "path stays the request path");
        }
    }
}
