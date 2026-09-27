//! The embedder's host-scoped headers, and the refusal of URLs that carry userinfo.
//!
//! The embedder (`crawlberg`) names one host and the headers that host gets: its custom
//! headers and its credential. The clients add them to a request only when the request host
//! is that host, on every redirect hop, so a third-party subresource or a cross-host redirect
//! never receives them. A URL with `user:pass@` in it is refused before any request goes out.

use url::Url;

use super::client::NetError;

/// Request headers scoped to one host, such as a credential.
#[derive(Clone, PartialEq, Eq)]
pub struct OriginHeaders {
    /// The host that receives the headers. Scheme and port are not compared.
    pub host: String,
    /// The `(name, value)` pairs, such as `("Authorization", "Basic ...")`. Each name is
    /// unique, and each one replaces a header of the same name the request already has.
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for OriginHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("OriginHeaders")
            .field("host", &self.host)
            .field("names", &names)
            .finish_non_exhaustive()
    }
}

impl OriginHeaders {
    /// The headers a request to `url` gets: all of them on the scoped host, none elsewhere.
    pub fn headers_for(&self, url: &Url) -> &[(String, String)] {
        match url.host_str() {
            Some(host) if host.eq_ignore_ascii_case(&self.host) => &self.headers,
            _ => &[],
        }
    }
}

/// Whether `url` carries a username or a password.
pub fn has_userinfo(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

/// Refuse `url` when it carries userinfo, naming it without the userinfo.
pub(crate) fn refuse_userinfo(url: &Url) -> Result<(), NetError> {
    if !has_userinfo(url) {
        return Ok(());
    }
    Err(NetError::Blocked(format!(
        "a URL with credentials in it is refused: {}",
        without_userinfo(url)
    )))
}

/// `url` without its username and password.
pub(crate) fn without_userinfo(url: &Url) -> Url {
    let mut clean = url.clone();
    // ~keep Both setters fail only for a URL that cannot hold userinfo, and such a URL has
    // ~keep none to remove.
    let _ = clean.set_password(None);
    let _ = clean.set_username("");
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test URL must parse")
    }

    fn credential() -> OriginHeaders {
        OriginHeaders {
            host: "example.com".to_owned(),
            headers: vec![
                ("X-Custom".to_owned(), "value".to_owned()),
                ("Authorization".to_owned(), "Basic dXNlcjpwdw==".to_owned()),
            ],
        }
    }

    #[test]
    fn the_scoped_host_gets_the_headers_on_any_scheme_and_port() {
        let credential = credential();
        assert_eq!(
            credential.headers_for(&url("https://EXAMPLE.com:8443/a")),
            credential.headers.as_slice()
        );
    }

    #[test]
    fn another_host_gets_nothing() {
        let credential = credential();
        for other in [
            "http://cdn.test/a.js",
            "http://sub.example.com/",
            "http://example.com.evil.test/",
        ] {
            assert!(
                credential.headers_for(&url(other)).is_empty(),
                "{other} must get nothing"
            );
        }
    }

    #[test]
    fn a_url_with_userinfo_is_refused_without_it() {
        let error = refuse_userinfo(&url("http://user:s3cret@example.com/a")).expect_err("must be refused");
        let text = error.to_string();
        assert!(!text.contains("s3cret"), "{text}");
        assert!(text.contains("http://example.com/a"), "{text}");
        assert!(refuse_userinfo(&url("http://example.com/a")).is_ok());
    }

    #[test]
    fn debug_output_hides_the_value() {
        let rendered = format!("{:?}", credential());
        assert!(
            !rendered.contains("dXNlcjpwdw") && !rendered.contains("value"),
            "{rendered}"
        );
        assert!(rendered.contains("Authorization"), "{rendered}");
    }
}
