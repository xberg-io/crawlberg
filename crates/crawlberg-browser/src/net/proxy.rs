use url::Url;

/// Why a configured proxy URL cannot be used.
///
/// The messages never contain the URL itself, so credentials embedded in it cannot leak
/// into a log or an error returned to a caller.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyError {
    #[error("the proxy URL does not parse: {0}")]
    Unparseable(String),

    #[error("the proxy URL scheme '{0}' is not supported; use http or https")]
    UnsupportedScheme(&'static str),

    /// A scheme that is not on [`NAMED_SCHEMES`]. It is not shown: a proxy written without a
    /// scheme, such as `KEY:@host:port`, parses with its user name as the scheme.
    #[error("the proxy URL scheme is not supported; use http or https")]
    UnnamedScheme,
}

/// The only schemes a refusal shows by name.
const NAMED_SCHEMES: [&str; 6] = ["socks4", "socks5", "socks5h", "ftp", "ws", "wss"];

/// Check that `proxy_url` is a proxy the browser HTTP clients can use.
///
/// `Proxy::all` in reqwest and wreq only parses the URL. Their proxy matchers later drop
/// any scheme they cannot speak, and the request then goes direct with no error. A SOCKS
/// URL is kept, but neither client is built with SOCKS support, so every request through
/// it fails. So the scheme is checked here, before a client exists.
pub fn check_proxy_url(proxy_url: &str) -> Result<Url, ProxyError> {
    let parsed = Url::parse(proxy_url).map_err(|e| ProxyError::Unparseable(e.to_string()))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        other => Err(NAMED_SCHEMES
            .into_iter()
            .find(|named| *named == other)
            .map_or(ProxyError::UnnamedScheme, ProxyError::UnsupportedScheme)),
    }
}

/// Build the reqwest proxy for `proxy_url`, refusing any URL [`check_proxy_url`] refuses.
pub fn reqwest_proxy(proxy_url: &str) -> Result<reqwest::Proxy, ProxyError> {
    check_proxy_url(proxy_url)?;
    reqwest::Proxy::all(proxy_url).map_err(|e| ProxyError::Unparseable(e.to_string()))
}

/// Build the wreq proxy for `proxy_url`, refusing any URL [`check_proxy_url`] refuses.
#[cfg(feature = "stealth")]
pub fn wreq_proxy(proxy_url: &str) -> Result<wreq::Proxy, ProxyError> {
    check_proxy_url(proxy_url)?;
    wreq::Proxy::all(proxy_url).map_err(|e| ProxyError::Unparseable(e.to_string()))
}

/// Proxy URLs that carry a credential, and a check that a refusal shows none of it.
#[cfg(test)]
pub(crate) mod credential_urls {
    /// The last two have no scheme, so they parse with the user name as the scheme.
    pub(crate) const URLS: [&str; 3] = [
        "://operator:s3cr3t@proxy.test:8080",
        "operator:s3cr3t@proxy:8080",
        "KEY:@host:1",
    ];

    /// Panic if `message`, the refusal of `url`, shows a user name or password from [`URLS`].
    /// The url crate lowercases a scheme, so the check ignores case.
    pub(crate) fn assert_not_shown(url: &str, message: &str) {
        let lowered = message.to_lowercase();
        for credential in ["operator", "s3cr3t", "key"] {
            assert!(
                !lowered.contains(credential),
                "the refusal of {url} shows '{credential}': {message}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_and_https_proxies_are_accepted() {
        assert!(check_proxy_url("http://proxy.test:8080").is_ok());
        assert!(check_proxy_url("https://user:pw@proxy.test:8443").is_ok());
    }

    #[test]
    fn a_url_without_a_scheme_is_refused_without_echoing_it() {
        for url in credential_urls::URLS {
            let err = check_proxy_url(url).expect_err("no scheme must be refused");
            credential_urls::assert_not_shown(url, &err.to_string());
        }
        assert!(matches!(
            check_proxy_url("://operator:s3cr3t@proxy.test:8080"),
            Err(ProxyError::Unparseable(_))
        ));
        assert_eq!(
            check_proxy_url("operator:s3cr3t@proxy:8080"),
            Err(ProxyError::UnnamedScheme)
        );
    }

    #[test]
    fn socks_and_other_schemes_are_refused_by_name() {
        for (url, scheme) in [
            ("socks5://proxy.test:1080", "socks5"),
            ("socks5h://proxy.test:1080", "socks5h"),
            ("socks4://proxy.test:1080", "socks4"),
            ("ftp://proxy.test:21", "ftp"),
            ("wss://proxy.test:443", "wss"),
        ] {
            assert_eq!(
                check_proxy_url(url),
                Err(ProxyError::UnsupportedScheme(scheme)),
                "{url} must be refused"
            );
        }
    }

    #[test]
    fn an_unlisted_scheme_is_refused_without_its_name() {
        for url in ["localhost:3128", "gopher://proxy.test:70"] {
            assert_eq!(
                check_proxy_url(url),
                Err(ProxyError::UnnamedScheme),
                "{url} must be refused"
            );
        }
    }
}
