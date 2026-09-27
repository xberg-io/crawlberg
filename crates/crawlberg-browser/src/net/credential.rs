//! The embedder's credential header, and the refusal of URLs that carry userinfo.
//!
//! The embedder (`crawlberg`) names one host and the header that host gets. The clients
//! add it to a request only when the request host is that host, on every redirect hop, so
//! a third-party subresource or a cross-host redirect never receives it. A URL with
//! `user:pass@` in it is refused before any request goes out.

use url::Url;

use super::client::NetError;

/// A credential header scoped to one host.
#[derive(Clone, PartialEq, Eq)]
pub struct OriginCredential {
    /// The host that receives the header. Scheme and port are not compared.
    pub host: String,
    /// The header name, such as `Authorization`.
    pub name: String,
    /// The header value.
    pub value: String,
}

impl std::fmt::Debug for OriginCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginCredential")
            .field("host", &self.host)
            .field("name", &self.name)
            .field("value", &"***")
            .finish()
    }
}

impl OriginCredential {
    /// The `(name, value)` header a request to `url` gets, if `url` is on the scoped host.
    pub fn header_for(&self, url: &Url) -> Option<(&str, &str)> {
        let host = url.host_str()?;
        host.eq_ignore_ascii_case(&self.host)
            .then_some((self.name.as_str(), self.value.as_str()))
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

    fn credential() -> OriginCredential {
        OriginCredential {
            host: "example.com".to_owned(),
            name: "Authorization".to_owned(),
            value: "Basic dXNlcjpwdw==".to_owned(),
        }
    }

    #[test]
    fn the_scoped_host_gets_the_header_on_any_scheme_and_port() {
        let credential = credential();
        assert_eq!(
            credential.header_for(&url("https://EXAMPLE.com:8443/a")),
            Some(("Authorization", "Basic dXNlcjpwdw=="))
        );
    }

    #[test]
    fn another_host_gets_nothing() {
        let credential = credential();
        for other in ["http://cdn.test/a.js", "http://sub.example.com/", "http://example.com.evil.test/"] {
            assert_eq!(credential.header_for(&url(other)), None, "{other} must get nothing");
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
        assert!(!rendered.contains("dXNlcjpwdw"), "{rendered}");
    }
}
