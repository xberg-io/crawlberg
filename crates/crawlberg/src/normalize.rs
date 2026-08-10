//! URL normalization and resolution utilities.

use url::Url;

/// Remove trailing slashes (except root) and collapse double slashes in a URL path.
fn clean_url_path(u: &mut Url) {
    let path = u.path().to_owned();
    if path.len() > 1 && path.ends_with('/') {
        u.set_path(&path[..path.len() - 1]);
    }
    let path = u.path().to_owned();
    if path.contains("//") {
        u.set_path(&path.replace("//", "/"));
    }
}

/// Normalize a URL by removing fragments, sorting query parameters,
/// removing trailing slashes (except root), and fixing double slashes in the path.
pub(crate) fn normalize_url(raw: &str) -> String {
    if let Ok(mut u) = Url::parse(raw) {
        u.set_fragment(None);
        let pairs: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        if !pairs.is_empty() {
            let mut sorted = pairs;
            sorted.sort();
            // ~keep Re-encode with a proper x-www-form-urlencoded serializer instead of
            // `format!("{k}={v}")`. The decoded pairs may contain '&' or '=' (e.g. from a
            // percent-encoded value), and writing them back unescaped would collapse two
            // genuinely different URLs onto the same normalized string.
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in &sorted {
                serializer.append_pair(k, v);
            }
            u.set_query(Some(&serializer.finish()));
        }
        clean_url_path(&mut u);
        u.to_string()
    } else {
        raw.to_owned()
    }
}

/// Normalize a URL for deduplication during crawling.
///
/// Strips query parameters and fragments, removes trailing slashes (except root),
/// and fixes double slashes in the path.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn normalize_url_for_dedup(raw: &str) -> String {
    if let Ok(mut u) = Url::parse(raw) {
        u.set_fragment(None);
        u.set_query(None);
        clean_url_path(&mut u);
        u.to_string()
    } else {
        raw.to_owned()
    }
}

/// Build the robots.txt URL for a given parsed URL.
pub(crate) fn robots_url(parsed: &Url) -> String {
    format!("{}://{}/robots.txt", parsed.scheme(), parsed.authority())
}

/// Strip the fragment from a URL string, returning the cleaned URL.
pub(crate) fn strip_fragment(url: &str) -> String {
    if let Ok(mut u) = Url::parse(url) {
        u.set_fragment(None);
        u.to_string()
    } else {
        url.to_owned()
    }
}

pub(crate) fn rewrite_url_host(url_str: &str, base: &Url) -> String {
    if let Ok(parsed) = Url::parse(url_str)
        && parsed.host_str() != base.host_str()
    {
        let mut resolved = base.clone();
        resolved.set_path(parsed.path());
        resolved.set_query(parsed.query());
        return resolved.to_string();
    }
    url_str.to_owned()
}

/// Resolve a redirect target against a base URL.
///
/// If the target is already absolute, returns it as-is. Otherwise, resolves
/// it relative to the base URL.
pub(crate) fn resolve_redirect(base_url: &str, target: &str) -> String {
    if target.starts_with("http://") || target.starts_with("https://") {
        return target.to_owned();
    }
    if let Ok(base) = Url::parse(base_url)
        && let Ok(resolved) = base.join(target)
    {
        return resolved.to_string();
    }
    target.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_urls_with_escaped_and_literal_separators_stay_distinct() {
        let escaped = normalize_url("http://example.com/?x=A%26y=B");
        let literal = normalize_url("http://example.com/?x=A&y=B");
        assert_ne!(
            escaped, literal,
            "?x=A%26y=B (one param, value contains '&') and ?x=A&y=B (two params) must not \
             normalize to the same string, but both produced {escaped:?}"
        );
        assert_eq!(
            escaped, "http://example.com/?x=A%26y%3DB",
            "expected the single-pair value 'A&y=B' to round-trip fully percent-encoded, got {escaped:?}"
        );
        assert_eq!(
            literal, "http://example.com/?x=A&y=B",
            "expected the two literal params to be preserved and sorted, got {literal:?}"
        );
    }

    #[test]
    fn sorts_query_parameters_by_key() {
        let normalized = normalize_url("http://example.com/?b=2&a=1");
        assert_eq!(
            normalized, "http://example.com/?a=1&b=2",
            "expected query parameters sorted by key, got {normalized:?}"
        );
    }

    #[test]
    fn removes_fragment_and_trailing_slash() {
        let normalized = normalize_url("http://example.com/path/#section");
        assert_eq!(
            normalized, "http://example.com/path",
            "expected fragment removed and trailing slash trimmed, got {normalized:?}"
        );
    }
}
