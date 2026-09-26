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

/// Sort `u`'s query parameters by key, re-encoding with a proper x-www-form-urlencoded
/// serializer instead of `format!("{k}={v}")`.
///
/// ~keep The decoded pairs may contain '&' or '=' (e.g. from a percent-encoded value), and
/// writing them back unescaped would collapse two genuinely different URLs onto the same
/// normalized string.
fn sort_query(u: &mut Url) {
    let pairs: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    if pairs.is_empty() {
        return;
    }
    let mut sorted = pairs;
    sorted.sort();
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in &sorted {
        serializer.append_pair(k, v);
    }
    u.set_query(Some(&serializer.finish()));
}

/// Normalize a URL by removing fragments, sorting query parameters,
/// removing trailing slashes (except root), and fixing double slashes in the path.
pub(crate) fn normalize_url(raw: &str) -> String {
    if let Ok(mut u) = Url::parse(raw) {
        u.set_fragment(None);
        sort_query(&mut u);
        clean_url_path(&mut u);
        u.to_string()
    } else {
        raw.to_owned()
    }
}

/// Normalize a URL for deduplication during crawling.
///
/// Strips fragments, removes trailing slashes (except root), and fixes double slashes in
/// the path. `include_query` decides whether the query string participates in the key too:
/// `false` (the historical default) drops it entirely, so `?id=1` and `?id=2` collapse to one
/// key; `true` keeps it, sorted, so they are treated as distinct pages.
///
/// ~keep Shared by the native and wasm crawl loops. The wasm loop used to carry its own
/// copy that omitted the `//` collapse, so the two targets disagreed on which URLs were
/// duplicates — one normalizer is the only way that stays fixed.
pub(crate) fn normalize_url_for_dedup(raw: &str, include_query: bool) -> String {
    if let Ok(mut u) = Url::parse(raw) {
        u.set_fragment(None);
        if include_query {
            sort_query(&mut u);
        } else {
            u.set_query(None);
        }
        clean_url_path(&mut u);
        u.to_string()
    } else {
        raw.to_owned()
    }
}

/// Whether `name` matches a tracking-parameter pattern. A pattern ending in `*` matches any
/// parameter name sharing that prefix (`utm_*` matches `utm_source`, `utm_campaign`, ...);
/// any other pattern must match the parameter name exactly.
fn matches_tracking_pattern(name: &str, pattern: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

/// Remove query parameters matching `patterns` (see [`matches_tracking_pattern`]) from `raw`,
/// preserving the order and encoding of the parameters that remain. Returns `raw` unchanged
/// if it fails to parse, carries no query string, or `patterns` is empty.
///
/// ~keep Applied once, at the point a URL is discovered (seed or link), rather than only at
/// the dedup key: `CrawlPageResult.normalized_url` and the URL actually fetched both derive
/// from that already-stripped string, so stripping downstream in `normalize_url` as well
/// would be redundant, and stripping only the dedup key would leave the tracking parameters
/// in the fetched and reported URL.
pub(crate) fn strip_tracking_params(raw: &str, patterns: &[String]) -> String {
    if patterns.is_empty() {
        return raw.to_owned();
    }
    let Ok(mut u) = Url::parse(raw) else {
        return raw.to_owned();
    };
    if u.query().is_none() {
        return raw.to_owned();
    }
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .filter(|(k, _)| !patterns.iter().any(|pattern| matches_tracking_pattern(k, pattern)))
        .collect();
    if kept.is_empty() {
        u.set_query(None);
    } else {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &kept {
            serializer.append_pair(k, v);
        }
        u.set_query(Some(&serializer.finish()));
    }
    u.to_string()
}

/// Build the robots.txt URL for a given parsed URL.
///
/// ~keep RFC 9309 section 2.3 scopes a robots.txt file to a scheme, a host and a port, so
/// this is built from the URL's origin. `host_str()` drops the port and asks a site on a
/// non-default port for a file it does not serve; `authority()` keeps the userinfo and
/// would send the caller's credentials to the robots.txt request.
pub(crate) fn robots_url(parsed: &Url) -> String {
    // ~keep `set_username`/`set_password` fail only for a scheme with no authority, which has
    // ~keep no credentials to strip, so the discarded result carries no information here.
    let mut origin = parsed.clone();
    origin.set_fragment(None);
    origin.set_query(None);
    let _ = origin.set_username("");
    let _ = origin.set_password(None);
    origin.set_path("/robots.txt");
    origin.to_string()
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

/// Resolve a redirect target against `base_url`. `target` may be relative or absolute;
/// `Url::join` parses either form on its own and returns the parser's normalized string.
/// Returns `None` when neither `target` nor `base_url` parses, so a caller must refuse the
/// target rather than follow or report it as raw text.
///
/// ~keep The return is always the parser's normalized form, never raw input, so a caller that
/// ~keep re-checks it (SSRF, policy) is checking what will actually be fetched.
pub(crate) fn resolve_redirect(base_url: &str, target: &str) -> Option<String> {
    if let Ok(base) = Url::parse(base_url) {
        return base.join(target).ok().map(|resolved| resolved.to_string());
    }
    // base_url itself fails to parse; a target that stands on its own can still resolve.
    Url::parse(target).ok().map(|resolved| resolved.to_string())
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

    /// ~keep The wasm crawl loop used to carry its own dedup normalizer that did all of
    /// this *except* the `//` collapse, so the two targets disagreed on which URLs were
    /// duplicates. Both now call this function; this pins the contract they share.
    #[test]
    fn dedup_key_collapses_double_slashes_alongside_query_fragment_and_trailing_slash() {
        let normalized = normalize_url_for_dedup("http://example.com/a//b/?q=1#top", false);
        assert_eq!(
            normalized, "http://example.com/a/b",
            "expected query, fragment, trailing slash and doubled path separator all \
             normalized away, got {normalized:?}"
        );
    }

    #[test]
    fn dedup_key_with_query_included_distinguishes_different_query_values() {
        let first = normalize_url_for_dedup("http://example.com/item?id=1", true);
        let second = normalize_url_for_dedup("http://example.com/item?id=2", true);
        assert_ne!(
            first, second,
            "with include_query the dedup key must distinguish ?id=1 from ?id=2"
        );
    }

    #[test]
    fn dedup_key_with_query_included_ignores_parameter_order() {
        let first = normalize_url_for_dedup("http://example.com/item?a=1&b=2", true);
        let second = normalize_url_for_dedup("http://example.com/item?b=2&a=1", true);
        assert_eq!(
            first, second,
            "differently-ordered query parameters must still produce one dedup key"
        );
    }

    #[test]
    fn strip_tracking_params_removes_prefix_and_exact_matches() {
        let patterns = [
            "utm_*".to_owned(),
            "fbclid".to_owned(),
            "gclid".to_owned(),
            "ref".to_owned(),
        ];
        let stripped = strip_tracking_params(
            "http://example.com/promo?utm_source=newsletter&id=1&fbclid=abc",
            &patterns,
        );
        assert_eq!(
            stripped, "http://example.com/promo?id=1",
            "utm_* and fbclid must be stripped while other params are kept, got {stripped:?}"
        );
    }

    #[test]
    fn strip_tracking_params_drops_the_question_mark_when_nothing_remains() {
        let patterns = ["utm_*".to_owned()];
        let stripped = strip_tracking_params("http://example.com/promo?utm_source=newsletter", &patterns);
        assert_eq!(
            stripped, "http://example.com/promo",
            "an all-tracking query string must leave no trailing '?', got {stripped:?}"
        );
    }

    #[test]
    fn robots_url_keeps_an_explicit_non_default_port() {
        let parsed = Url::parse("http://127.0.0.1:8081/deep/page.html").expect("valid URL");
        assert_eq!(
            robots_url(&parsed),
            "http://127.0.0.1:8081/robots.txt",
            "a site on a non-default port serves its robots.txt on that port"
        );
    }

    #[test]
    fn robots_url_omits_the_schemes_default_port() {
        let parsed = Url::parse("https://example.com:443/a").expect("valid URL");
        assert_eq!(
            robots_url(&parsed),
            "https://example.com/robots.txt",
            "the default port is not written into the robots.txt URL"
        );
    }

    #[test]
    fn robots_url_strips_credentials_from_the_seed() {
        let parsed = Url::parse("http://user:secret@example.com:8081/a").expect("valid URL");
        assert_eq!(
            robots_url(&parsed),
            "http://example.com:8081/robots.txt",
            "the caller's credentials must not be sent with the robots.txt request"
        );
    }

    #[test]
    fn robots_url_drops_the_query_and_the_fragment() {
        let parsed = Url::parse("http://example.com:8081/a?q=1#top").expect("valid URL");
        assert_eq!(
            robots_url(&parsed),
            "http://example.com:8081/robots.txt",
            "robots.txt is a fixed path on the origin, not a rewrite of the seed"
        );
    }

    #[test]
    fn dedup_key_maps_a_doubled_separator_onto_its_single_separator_twin() {
        assert_eq!(
            normalize_url_for_dedup("http://example.com/a//b", false),
            normalize_url_for_dedup("http://example.com/a/b", false),
            "a doubled path separator must not produce a second frontier entry for one page"
        );
    }

    #[test]
    fn absolute_target_with_embedded_tab_and_newline_is_parser_normalized() {
        let resolved = resolve_redirect("https://example.com/start", "https://example.com/\ta\nb");
        assert_eq!(
            resolved,
            Some("https://example.com/ab".to_owned()),
            "an embedded tab/newline in an absolute target must be stripped the same way \
             the URL parser strips it from a relative target, got {resolved:?}"
        );
    }

    /// A leading space never reached the old prefix branch (`starts_with` doesn't match), so
    /// it was already trimmed by the relative-join fallback whenever `base_url` parsed. Using
    /// a `base_url` that fails to parse instead exercises the case the old dispatch got wrong.
    #[test]
    fn absolute_target_with_leading_space_is_trimmed_even_when_base_fails_to_parse() {
        let resolved = resolve_redirect("not a url", "   https://example.com/next");
        assert_eq!(
            resolved,
            Some("https://example.com/next".to_owned()),
            "a leading space on an absolute target must be trimmed even when the base \
             doesn't parse, got {resolved:?}"
        );
    }

    #[test]
    fn absolute_target_with_trailing_space_is_trimmed() {
        let resolved = resolve_redirect("https://example.com/start", "https://example.com/next   ");
        assert_eq!(
            resolved,
            Some("https://example.com/next".to_owned()),
            "trailing spaces on an absolute target must be trimmed like a relative target's \
             are, got {resolved:?}"
        );
    }

    /// A `base_url` that fails to parse is the only case where the old prefix check
    /// (`starts_with("https://")`, case-sensitive) mattered: with a valid base, the relative
    /// branch already resolves an absolute target on its own, uppercase scheme included.
    #[test]
    fn uppercase_scheme_target_still_resolves_when_base_fails_to_parse() {
        let resolved = resolve_redirect("not a url", "HTTPS://example.com/x");
        assert_eq!(
            resolved,
            Some("https://example.com/x".to_owned()),
            "an absolute target must resolve on its own when the base doesn't parse, \
             whatever case its scheme is written in, got {resolved:?}"
        );
    }

    #[test]
    fn unparseable_absolute_target_is_refused() {
        let resolved = resolve_redirect("https://example.com/start", "https://ex ample.com/x");
        assert_eq!(
            resolved, None,
            "a target the URL parser refuses must come back as None, so a caller refuses it \
             instead of following or reporting it as raw text, got {resolved:?}"
        );
    }

    #[test]
    fn clean_absolute_target_is_unchanged() {
        let clean = "https://example.com/page?a=1&b=2";
        let resolved = resolve_redirect("https://example.com/start", clean);
        assert_eq!(
            resolved,
            Some(clean.to_owned()),
            "a target that already parses cleanly must come back byte-identical, got {resolved:?}"
        );
    }
}
