//! Userinfo (`user:pass@`) handling for URLs.
//!
//! A caller's URL is split once, when the engine admits it: the userinfo becomes a
//! [`CredentialScope`](super::credentials::CredentialScope) and only the clean URL goes on.
//! A URL a page supplies (a link, a sitemap `<loc>`, a redirect target) is stripped, so no
//! URL string inside the engine ever holds userinfo.

use url::Url;

/// Whether `url` carries a username or a password.
pub(crate) fn has_userinfo(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

/// Whether `input` parses as a URL that carries a username or a password.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn str_has_userinfo(input: &str) -> bool {
    Url::parse(input).is_ok_and(|url| has_userinfo(&url))
}

/// Remove the username and password from `url`.
pub(crate) fn strip(url: &mut Url) {
    if has_userinfo(url) {
        // ~keep Both setters fail only for a URL that cannot hold userinfo, which
        // ~keep `has_userinfo` has just ruled out.
        let _ = url.set_password(None);
        let _ = url.set_username("");
    }
}

/// Split `url` into the clean URL and its percent-decoded `(username, password)`.
pub(crate) fn split(mut url: Url) -> (Url, Option<(String, String)>) {
    if !has_userinfo(&url) {
        return (url, None);
    }
    let decode = |part: &str| percent_encoding::percent_decode_str(part).decode_utf8_lossy().into_owned();
    let username = decode(url.username());
    let password = url.password().map(decode).unwrap_or_default();
    strip(&mut url);
    (url, Some((username, password)))
}

/// Resolve a page-supplied `href` against `base`, without userinfo.
///
/// Returns `None` when `href` does not resolve to a URL.
pub(crate) fn resolve(base: &Url, href: &str) -> Option<Url> {
    let mut url = base.join(href).ok()?;
    strip(&mut url);
    Some(url)
}

/// Parse an absolute page-supplied URL, without userinfo.
pub(crate) fn parse(input: &str) -> Option<Url> {
    let mut url = Url::parse(input).ok()?;
    strip(&mut url);
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test URL must parse")
    }

    #[test]
    fn split_returns_the_clean_url_and_the_decoded_credentials() {
        let (clean, credentials) = split(url("http://us%40er:p%3Ass@example.com/a"));
        assert_eq!(clean.as_str(), "http://example.com/a");
        assert_eq!(credentials, Some(("us@er".to_owned(), "p:ss".to_owned())));
    }

    #[test]
    fn split_keeps_a_username_without_a_password() {
        let (clean, credentials) = split(url("http://user@example.com/"));
        assert_eq!(clean.as_str(), "http://example.com/");
        assert_eq!(credentials, Some(("user".to_owned(), String::new())));
    }

    #[test]
    fn split_leaves_a_url_without_userinfo_alone() {
        let (clean, credentials) = split(url("http://example.com/a?b=c"));
        assert_eq!(clean.as_str(), "http://example.com/a?b=c");
        assert_eq!(credentials, None);
    }

    #[test]
    fn resolve_strips_userinfo_from_an_absolute_href() {
        let base = url("http://example.com/dir/");
        let resolved = resolve(&base, "http://u:secret@other.test/x").expect("href must resolve");
        assert_eq!(resolved.as_str(), "http://other.test/x");
    }

    #[test]
    fn resolve_keeps_a_relative_href_on_the_base() {
        let base = url("http://example.com/dir/");
        let resolved = resolve(&base, "page?q=1").expect("href must resolve");
        assert_eq!(resolved.as_str(), "http://example.com/dir/page?q=1");
    }

    #[test]
    fn resolve_returns_none_for_an_unresolvable_href() {
        let base = url("data:text/plain,x");
        assert_eq!(resolve(&base, "page"), None);
    }

    #[test]
    fn parse_strips_userinfo() {
        let parsed = parse("https://u:secret@example.com/s.xml").expect("URL must parse");
        assert_eq!(parsed.as_str(), "https://example.com/s.xml");
        assert_eq!(parse("not a url"), None);
    }
}
