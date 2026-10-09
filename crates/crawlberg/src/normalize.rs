//! URL normalization and resolution utilities.

use url::Url;

/// Collapse double slashes in a URL path.
fn collapse_double_slashes(u: &mut Url) {
    let path = u.path();
    if path.contains("//") {
        let collapsed = path.replace("//", "/");
        u.set_path(&collapsed);
    }
}

/// The octet that the percent-escape at the start of `escape` encodes, when its two digits are hex.
fn escaped_octet(escape: &str) -> Option<u8> {
    let digits = escape.as_bytes().get(1..3)?;
    let high = char::from(digits[0]).to_digit(16)?;
    let low = char::from(digits[1]).to_digit(16)?;
    u8::try_from(high * 16 + low).ok()
}

/// Apply to `address` the two percent-encoding normalisations of RFC 3986 section 6.2.2: decode
/// an escape of an unreserved character (a letter, a digit, `-`, `.`, `_` or `~`), and write
/// the hex digits of every other escape in upper case. A `%` that does not start an escape
/// stays as it is.
///
/// ~keep No other escape is decoded. An escaped reserved character such as `%2F` is not
/// ~keep equivalent to the character, so decoding it would merge two different resources.
fn normalize_percent_encoding(address: &str) -> String {
    let mut out = String::with_capacity(address.len());
    let mut rest = address;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let consumed = match escaped_octet(rest) {
            Some(octet) if octet.is_ascii_alphanumeric() || matches!(octet, b'-' | b'.' | b'_' | b'~') => {
                out.push(char::from(octet));
                3
            }
            Some(_) => {
                out.push('%');
                out.extend(rest[1..3].chars().map(|digit| digit.to_ascii_uppercase()));
                3
            }
            None => {
                out.push('%');
                1
            }
        };
        rest = &rest[consumed..];
    }
    out.push_str(rest);
    out
}

/// The name of one `&`-separated query parameter: the text before its first `=`, or all of it.
fn parameter_name(parameter: &str) -> &str {
    parameter.split_once('=').map_or(parameter, |(name, _)| name)
}

/// Sort the query parameters of `address`, a serialized URL with no fragment, by name. Each
/// parameter stays as it is written, and parameters with one name keep their order.
///
/// ~keep The sort by name is the documented behaviour of `dedup_include_query` (crawlberg#65),
/// ~keep not a rule of a standard: a server may read `?a=1&b=2` and `?b=2&a=1` differently.
/// ~keep Nothing else is merged. The order of the values of ONE name carries meaning
/// ~keep (`?tag=a&tag=b`), so the sort is stable and compares the name only. A parameter is
/// ~keep not decoded and encoded again: that would make `?a` equal to `?a=` and `+` equal to
/// ~keep `%20`, which no standard says of a URL.
fn sort_query_parameters(address: String) -> String {
    let Some((before, query)) = address.split_once('?') else {
        return address;
    };
    let mut parameters: Vec<&str> = query.split('&').collect();
    parameters.sort_by(|a, b| parameter_name(a).cmp(parameter_name(b)));
    format!("{before}?{}", parameters.join("&"))
}

/// The address of a page as `CrawlPageResult.normalized_url` reports it: the frontier key of
/// [`normalize_url_for_dedup`] with the query kept.
pub(crate) fn normalize_url(raw: &str) -> String {
    normalize_url_for_dedup(raw, true)
}

/// The key that decides whether two addresses are one page during crawling.
///
/// Two addresses get one key only where a standard says they name one resource: the URL
/// parser's own serialization, no fragment, and the percent-encoding rules of
/// [`normalize_percent_encoding`]. A trailing slash is kept, because `/docs` and `/docs/` are
/// two resources a server can answer differently. Doubled slashes in the path are collapsed.
/// `include_query` decides whether the query string participates in the key too:
/// `false` (the historical default) drops it entirely, so `?id=1` and `?id=2` collapse to one
/// key; `true` keeps it, with its parameters sorted by name ([`sort_query_parameters`]) and
/// its escapes in the same form as the path's, so they are treated as distinct pages.
///
/// ~keep Shared by the native and wasm crawl loops. The wasm loop used to carry its own
/// copy that omitted the `//` collapse, so the two targets disagreed on which URLs were
/// duplicates — one normalizer is the only way that stays fixed.
pub(crate) fn normalize_url_for_dedup(raw: &str, include_query: bool) -> String {
    if let Ok(mut u) = Url::parse(raw) {
        u.set_fragment(None);
        if !include_query {
            u.set_query(None);
        }
        collapse_double_slashes(&mut u);
        // ~keep Escapes first, the sort second: `%61` and `a` are one name and must sort as one.
        let key = normalize_percent_encoding(u.as_str());
        if include_query { sort_query_parameters(key) } else { key }
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

/// Resolve a redirect target against `base_url`, without userinfo. `target` may be relative
/// or absolute; `Url::join` parses either form on its own and returns the parsed URL.
/// Returns `None` in two cases: `base_url` parses but `target` fails to join against it, or
/// `base_url` fails to parse and `target` also fails to parse on its own. Either way, the
/// caller must refuse the target rather than follow or report it as raw text.
///
/// ~keep The return is always a parsed URL, never raw input, so a caller that re-checks it
/// ~keep (SSRF, policy) is checking what will actually be fetched, and cannot skip the check
/// ~keep for a target that fails to parse.
pub(crate) fn resolve_redirect(base_url: &str, target: &str) -> Option<Url> {
    if let Ok(base) = Url::parse(base_url) {
        return crate::net::userinfo::resolve(&base, target);
    }
    // base_url itself fails to parse; a target that stands on its own can still resolve.
    crate::net::userinfo::parse(target)
}

/// Human-readable form of `url_str`, for matching a caller's typed search term against an
/// address the parser has percent-encoded and idna-encoded.
///
/// ~keep Percent-encoding is substring-safe (each character encodes on its own), but
/// ~keep punycode is not: it transforms a whole host label, so encoding a substring of a
/// ~keep search term the way a host is encoded does not, in general, land inside that host's
/// ~keep encoded label. Decoding the address instead covers both the path and the host with
/// ~keep one pass, and an ASCII address decodes back to itself unchanged.
/// Falls back to `url_str` unchanged if it fails to parse. The output exists only to match a
/// search term: it omits userinfo and writes a non-special scheme as `scheme://`, and the check
/// against the raw address still covers both.
pub(crate) fn decoded_for_search(url_str: &str) -> String {
    let Ok(parsed) = Url::parse(url_str) else {
        return url_str.to_owned();
    };
    let mut out = String::new();
    out.push_str(parsed.scheme());
    out.push_str("://");
    if let Some(host) = parsed.host_str() {
        out.push_str(&idna::domain_to_unicode(host).0);
    }
    if let Some(port) = parsed.port() {
        out.push(':');
        out.push_str(&port.to_string());
    }
    out.push_str(&percent_encoding::percent_decode_str(parsed.path()).decode_utf8_lossy());
    if let Some(query) = parsed.query() {
        out.push('?');
        out.push_str(&percent_encoding::percent_decode_str(query).decode_utf8_lossy());
    }
    if let Some(fragment) = parsed.fragment() {
        out.push('#');
        out.push_str(&percent_encoding::percent_decode_str(fragment).decode_utf8_lossy());
    }
    out
}

/// The form in which a `map_search` term and an address are compared: canonical
/// decomposition, then Unicode default case folding, then canonical composition, so `é` and
/// `e` plus a combining acute accent match, and `ß` matches `SS`.
///
/// ~keep Default case folding is locale-free, so the Turkish dotted and dotless `i` never
/// ~keep match their Turkish case partners: `İ` does not match `i`, and `ışık` does not match
/// ~keep `IŞIK`. Joining them would need a locale, which a search term does not carry.
pub(crate) fn search_key(text: &str) -> String {
    let decomposed = icu_normalizer::DecomposingNormalizer::new_nfd().normalize(text);
    let folded = icu_casemap::CaseMapper::new().fold_string(&decomposed);
    icu_normalizer::ComposingNormalizer::new_nfc()
        .normalize(&folded)
        .into_owned()
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
            escaped, "http://example.com/?x=A%26y=B",
            "expected the one parameter kept as it is written, got {escaped:?}"
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
    fn removes_the_fragment_and_keeps_a_trailing_slash() {
        let normalized = normalize_url("http://example.com/path/#section");
        assert_eq!(
            normalized, "http://example.com/path/",
            "expected the fragment removed and the trailing slash kept, got {normalized:?}"
        );
    }

    #[test]
    fn a_trailing_slash_makes_another_key() {
        for include_query in [false, true] {
            assert_ne!(
                normalize_url_for_dedup("http://example.com/docs", include_query),
                normalize_url_for_dedup("http://example.com/docs/", include_query),
                "/docs and /docs/ are two resources (include_query={include_query})"
            );
        }
        assert_eq!(
            normalize_url_for_dedup("http://example.com", false),
            normalize_url_for_dedup("http://example.com/", false),
            "the URL standard gives an address without a path the path `/`"
        );
    }

    /// RFC 3986 section 6.2.2.2: an escape of an unreserved character is equivalent to the
    /// character. Section 6.2.2.1: the hex digits of an escape are case-insensitive.
    #[test]
    fn percent_encoding_is_normalised_by_the_two_rfc_3986_rules_only() {
        for (input, expected) in [
            ("http://example.com/a%2db", "http://example.com/a-b"),
            ("http://example.com/a%2Db", "http://example.com/a-b"),
            ("http://example.com/a-b", "http://example.com/a-b"),
            (
                "http://example.com/%41%7a%30%2D%2e%5F%7Ex",
                "http://example.com/Az0-._~x",
            ),
            ("http://example.com/a%2fb", "http://example.com/a%2Fb"),
            ("http://example.com/a%2Fb", "http://example.com/a%2Fb"),
            ("http://example.com/caf%c3%a9", "http://example.com/caf%C3%A9"),
            ("http://example.com/a%20b%3f%23%25", "http://example.com/a%20b%3F%23%25"),
            ("http://example.com/q%5cr", "http://example.com/q%5Cr"),
            ("http://example.com/100%", "http://example.com/100%"),
            ("http://example.com/a%2", "http://example.com/a%2"),
            ("http://example.com/a%zzb%4", "http://example.com/a%zzb%4"),
            ("http://example.com/a%%41", "http://example.com/a%A"),
            ("http://example.com/a%252Db", "http://example.com/a%252Db"),
        ] {
            assert_eq!(normalize_url_for_dedup(input, false), expected, "for {input:?}");
            assert_eq!(normalize_url(input), expected, "normalized_url for {input:?}");
        }
    }

    #[test]
    fn an_escaped_reserved_character_keeps_its_own_key() {
        for (escaped, literal) in [
            ("http://example.com/a%2Fb", "http://example.com/a/b"),
            ("http://example.com/a%3Fb", "http://example.com/a?b"),
            ("http://example.com/q%5Cr", "http://example.com/q/r"),
            ("http://example.com/a%2Bb", "http://example.com/a+b"),
            ("http://example.com/a%3Bb", "http://example.com/a;b"),
            ("http://example.com/Page", "http://example.com/page"),
        ] {
            assert_ne!(
                normalize_url(escaped),
                normalize_url(literal),
                "{escaped:?} and {literal:?} are two resources"
            );
        }
    }

    /// The URL parser reads an escaped dot segment as a dot segment, so decoding `%2e` after
    /// it cannot make a new one.
    #[test]
    fn an_escaped_dot_segment_is_resolved_by_the_parser_before_the_escapes_are_decoded() {
        assert_eq!(
            normalize_url_for_dedup("http://example.com/a/%2e%2E/b/%2e/c", false),
            "http://example.com/b/c"
        );
        assert_eq!(
            normalize_url_for_dedup("http://example.com/a/x%2e%2e/b", false),
            "http://example.com/a/x../b",
            "dots inside a longer segment are plain characters"
        );
    }

    #[test]
    fn percent_encoding_of_a_kept_query_is_normalised_too() {
        assert_eq!(
            normalize_url_for_dedup("http://example.com/s?q=a%2db&t=%7euser", true),
            normalize_url_for_dedup("http://example.com/s?q=a-b&t=~user", true),
        );
        assert_ne!(
            normalize_url_for_dedup("http://example.com/s?q=a%26b", true),
            normalize_url_for_dedup("http://example.com/s?q=a&b", true),
            "an escaped separator in a query value is not the separator"
        );
        assert_eq!(
            normalize_url("http://example.com/s?%7a=1&b=%c3%a9&y=%2d"),
            "http://example.com/s?b=%C3%A9&y=-&z=1",
            "an escaped name sorts as the name it encodes, and the two RFC 3986 rules apply to the query"
        );
    }

    /// The kept query merges by the documented sort of parameter names and by nothing else.
    #[test]
    fn a_kept_query_is_sorted_by_name_and_otherwise_kept_as_written() {
        assert_eq!(
            normalize_url_for_dedup("http://example.com/s?b=2&a=1", true),
            normalize_url_for_dedup("http://example.com/s?a=1&b=2", true),
            "the documented sort: the order of parameters with different names is not kept"
        );
        assert_eq!(
            normalize_url_for_dedup("http://example.com/s?tag=b&z=9&tag=a&a=0", true),
            "http://example.com/s?a=0&tag=b&tag=a&z=9",
            "the values of one name keep their order"
        );
        for (one, other) in [
            ("http://example.com/s?a=1&a=2", "http://example.com/s?a=2&a=1"),
            ("http://example.com/s?a", "http://example.com/s?a="),
            ("http://example.com/s?q=a+b", "http://example.com/s?q=a%20b"),
            ("http://example.com/s?a=1&", "http://example.com/s?a=1"),
            ("http://example.com/s?", "http://example.com/s"),
            ("http://example.com/s?a=1&b", "http://example.com/s?a=1%26b"),
            ("http://example.com/s?a=b=c", "http://example.com/s?a=b%3Dc"),
        ] {
            assert_ne!(
                normalize_url_for_dedup(one, true),
                normalize_url_for_dedup(other, true),
                "{one:?} and {other:?} are two addresses"
            );
            assert_eq!(
                normalize_url_for_dedup(one, false),
                normalize_url_for_dedup(other, false),
                "the default key drops the query of {one:?} and {other:?}"
            );
        }
        for kept in [
            "http://example.com/s?a",
            "http://example.com/s?a=",
            "http://example.com/s?q=a+b",
            "http://example.com/s?",
        ] {
            assert_eq!(
                normalize_url(kept),
                kept,
                "nothing is added to or removed from {kept:?}"
            );
        }
    }

    #[test]
    fn an_address_that_does_not_parse_is_returned_as_written() {
        assert_eq!(normalize_url_for_dedup("not a url/%2d/", false), "not a url/%2d/");
        assert_eq!(normalize_url("not a url/%2d/"), "not a url/%2d/");
    }

    #[test]
    fn normalized_url_is_the_frontier_key_with_the_query_kept() {
        let address = "http://example.com/a//b/%7Euser/?b=2&a=1#top";
        assert_eq!(normalize_url(address), "http://example.com/a/b/~user/?a=1&b=2");
        assert_eq!(normalize_url(address), normalize_url_for_dedup(address, true));
        assert_eq!(normalize_url_for_dedup(address, false), "http://example.com/a/b/~user/");
    }

    /// ~keep The wasm crawl loop used to carry its own dedup normalizer that did all of
    /// this *except* the `//` collapse, so the two targets disagreed on which URLs were
    /// duplicates. Both now call this function; this pins the contract they share.
    #[test]
    fn dedup_key_collapses_double_slashes_and_drops_the_query_and_the_fragment() {
        let normalized = normalize_url_for_dedup("http://example.com/a//b/?q=1#top", false);
        assert_eq!(
            normalized, "http://example.com/a/b/",
            "expected the query, the fragment and the doubled path separator normalized \
             away and the trailing slash kept, got {normalized:?}"
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
        let resolved = resolve_redirect("https://example.com/start", "https://example.com/\ta\nb").map(String::from);
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
        let resolved = resolve_redirect("not a url", "   https://example.com/next").map(String::from);
        assert_eq!(
            resolved,
            Some("https://example.com/next".to_owned()),
            "a leading space on an absolute target must be trimmed even when the base \
             doesn't parse, got {resolved:?}"
        );
    }

    #[test]
    fn absolute_target_with_trailing_space_is_trimmed() {
        let resolved = resolve_redirect("https://example.com/start", "https://example.com/next   ").map(String::from);
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
        let resolved = resolve_redirect("not a url", "HTTPS://example.com/x").map(String::from);
        assert_eq!(
            resolved,
            Some("https://example.com/x".to_owned()),
            "an absolute target must resolve on its own when the base doesn't parse, \
             whatever case its scheme is written in, got {resolved:?}"
        );
    }

    #[test]
    fn unparseable_absolute_target_is_refused() {
        let resolved = resolve_redirect("https://example.com/start", "https://ex ample.com/x").map(String::from);
        assert_eq!(
            resolved, None,
            "a target the URL parser refuses must come back as None, so a caller refuses it \
             instead of following or reporting it as raw text, got {resolved:?}"
        );
    }

    /// This target already IS the parser's normalized form (lower-case host, default path,
    /// no IDN/port/dot-segment to rewrite), so parsing it is a no-op. It does not show that
    /// every clean target survives unchanged: parsing still rewrites an IDN host to punycode,
    /// drops a default port, lower-cases the host, adds `/` to a bare origin, removes dot
    /// segments, percent-encodes a space, and canonicalizes `127.1` to `127.0.0.1`.
    ///
    /// ~keep GUARD: no hand arm reddens this; a target with nothing left to normalize passes
    /// ~keep through any resolver that round-trips clean input, so it cannot pin one mechanism.
    #[test]
    fn absolute_target_already_in_normalized_form_round_trips_unchanged() {
        let clean = "https://example.com/page?a=1&b=2";
        let resolved = resolve_redirect("https://example.com/start", clean).map(String::from);
        assert_eq!(
            resolved,
            Some(clean.to_owned()),
            "a target with nothing left to normalize must come back byte-identical, got {resolved:?}"
        );
    }

    #[test]
    fn decoded_for_search_percent_decodes_a_non_ascii_path() {
        let decoded = decoded_for_search("https://example.com/caf%C3%A9");
        assert_eq!(
            decoded, "https://example.com/café",
            "expected the percent-encoded path decoded back to the UTF-8 text it encodes, got {decoded:?}"
        );
    }

    #[test]
    fn decoded_for_search_idna_decodes_a_punycode_host() {
        let decoded = decoded_for_search("https://xn--bcher-kva.example/x");
        assert_eq!(
            decoded, "https://bücher.example/x",
            "expected the punycode host decoded back to its Unicode form, got {decoded:?}"
        );
    }

    #[test]
    fn decoded_for_search_percent_decodes_a_non_ascii_query_value() {
        let decoded = decoded_for_search("https://example.com/x?q=caf%C3%A9");
        assert_eq!(
            decoded, "https://example.com/x?q=café",
            "expected the percent-encoded query value decoded back to its UTF-8 text, got {decoded:?}"
        );
    }

    #[test]
    fn decoded_for_search_percent_decodes_a_non_ascii_fragment() {
        let decoded = decoded_for_search("https://example.com/x#caf%C3%A9");
        assert_eq!(
            decoded, "https://example.com/x#café",
            "expected the percent-encoded fragment decoded back to its UTF-8 text, got {decoded:?}"
        );
    }

    // ~keep GUARD: passes against an identity decoder; pins that ASCII and unparseable input are
    // ~keep unchanged.
    #[test]
    fn decoded_for_search_leaves_an_ascii_address_unchanged() {
        let decoded = decoded_for_search("https://example.com/keep-1?a=1");
        assert_eq!(
            decoded, "https://example.com/keep-1?a=1",
            "an ASCII address must decode back to itself, got {decoded:?}"
        );
    }

    // ~keep GUARD: passes against an identity decoder; pins that ASCII and unparseable input are
    // ~keep unchanged.
    #[test]
    fn decoded_for_search_falls_back_to_the_raw_string_when_unparseable() {
        let decoded = decoded_for_search("not a url");
        assert_eq!(
            decoded, "not a url",
            "an address the parser refuses must come back unchanged, got {decoded:?}"
        );
    }
}
