use url::Url;

/// Why a configured proxy URL cannot be used.
///
/// The messages never contain the URL itself, so credentials embedded in it cannot leak
/// into a log or an error returned to a caller.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyError {
    #[error("the proxy URL does not parse: {0}")]
    Unparseable(String),

    /// The scheme shown is always a real one: an address whose first word the url crate
    /// reads as a scheme has no host, so [`check_proxy_url`] reads it as `http://` instead.
    #[error("the proxy URL scheme '{0}' is not supported; use http or https")]
    UnsupportedScheme(String),
}

/// The proxy schemes both browser HTTP clients can use.
pub const SUPPORTED_SCHEMES: [&str; 2] = ["http", "https"];

/// Check that `proxy_url` is a proxy the browser HTTP clients can use, and return the URL
/// to hand them.
///
/// `Proxy::all` in reqwest and wreq only parses the URL. Their proxy matchers later drop
/// any scheme they cannot speak, and the request then goes direct with no error. So the
/// scheme is checked here, before a client exists.
pub fn check_proxy_url(proxy_url: &str) -> Result<Url, ProxyError> {
    let parsed = match Url::parse(proxy_url) {
        Ok(url) if url.has_host() => url,
        // ~keep reqwest's proxy parser retries `http://<input>` when the input has no scheme
        // ~keep (`127.0.0.1:3128`) and when it parses without a host (`localhost:3128`,
        // ~keep `user:pass@proxy:8080`, whose first word the url crate reads as a scheme).
        // ~keep wreq has no retry, so it is given the URL returned here.
        Ok(_) | Err(url::ParseError::RelativeUrlWithoutBase) => Url::parse(&format!("http://{proxy_url}"))
            .ok()
            .filter(Url::has_host)
            .ok_or_else(|| ProxyError::Unparseable("expected an address such as http://proxy:8080".to_string()))?,
        Err(e) => return Err(ProxyError::Unparseable(e.to_string())),
    };
    if SUPPORTED_SCHEMES.contains(&parsed.scheme()) {
        Ok(parsed)
    } else {
        Err(ProxyError::UnsupportedScheme(parsed.scheme().to_string()))
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
    let url = check_proxy_url(proxy_url)?;
    wreq::Proxy::all(url.as_str()).map_err(|e| ProxyError::Unparseable(e.to_string()))
}

/// Proxy URLs that carry a credential and are refused, and a check that a refusal shows
/// none of it.
#[cfg(test)]
pub(crate) mod credential_urls {
    pub(crate) const URLS: [&str; 3] = [
        "://operator:s3cr3t@proxy.test:8080",
        "operator:s3cr3t@proxy:99999",
        "gopher://operator:s3cr3t@proxy.test:70",
    ];

    /// Panic if `message`, the refusal of `url`, shows a user name or password from [`URLS`].
    pub(crate) fn assert_not_shown(url: &str, message: &str) {
        for credential in ["operator", "s3cr3t"] {
            assert!(
                !message.contains(credential),
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
    fn proxy_addresses_are_read_exactly_as_reqwest_reads_them() {
        for raw in [
            "http://proxy.test:8080",
            "https://u:p@proxy.test:8443",
            "127.0.0.1:3128",
            "[::1]:3128",
            "localhost:3128",
            "myproxy.corp:3128",
            "operator:s3cr3t@proxy:8080",
            "KEY:@host:1",
            "user@127.0.0.1:3128",
            "proxy.test",
            "http:proxy.test:8080",
            "://operator:s3cr3t@proxy.test:8080",
            "operator:s3cr3t@proxy:99999",
            "mailto:someone",
            "",
        ] {
            match (reqwest::Proxy::all(raw), check_proxy_url(raw)) {
                (Ok(theirs), Ok(ours)) => {
                    let (theirs, ours) = (format!("{theirs:?}"), format!("{ours:?}"));
                    assert!(
                        theirs.contains(&ours),
                        "{raw:?}: reqwest reads {theirs}, we read {ours}"
                    );
                }
                (Err(_), Err(_)) => {}
                (theirs, ours) => panic!(
                    "{raw:?}: reqwest accepts it: {}, we accept it: {}",
                    theirs.is_ok(),
                    ours.is_ok()
                ),
            }
        }
    }

    #[test]
    fn a_scheme_less_proxy_is_read_as_http() {
        for (proxy, expected) in [
            ("127.0.0.1:3128", "http://127.0.0.1:3128/"),
            ("[::1]:3128", "http://[::1]:3128/"),
            ("localhost:3128", "http://localhost:3128/"),
            ("operator:s3cr3t@proxy:8080", "http://operator:s3cr3t@proxy:8080/"),
        ] {
            assert_eq!(
                check_proxy_url(proxy).map(|url| url.to_string()),
                Ok(expected.to_string()),
                "{proxy} must be used as an HTTP proxy"
            );
        }
    }

    #[test]
    fn a_refused_proxy_is_not_shown() {
        for url in credential_urls::URLS {
            let err = check_proxy_url(url).expect_err("the proxy must be refused");
            credential_urls::assert_not_shown(url, &err.to_string());
        }
    }

    #[test]
    fn socks_and_other_schemes_are_refused_by_name() {
        for (url, scheme) in [
            ("socks5://proxy.test:1080", "socks5"),
            ("socks5h://proxy.test:1080", "socks5h"),
            ("socks4://proxy.test:1080", "socks4"),
            ("ftp://proxy.test:21", "ftp"),
            ("gopher://proxy.test:70", "gopher"),
        ] {
            assert_eq!(
                check_proxy_url(url),
                Err(ProxyError::UnsupportedScheme(scheme.to_string())),
                "{url} must be refused"
            );
        }
    }
}
