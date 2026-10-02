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

    #[error("the proxy address holds a user name or password; pass them as the proxy credentials")]
    CredentialsInAddress,

    /// ~keep An unencoded `#`, `/` or `?` in a password ends the authority early, so the url crate
    /// ~keep reads the user name as the host and leaves the rest, `@` included, in the path, the
    /// ~keep query or the fragment.
    #[error(
        "the proxy address holds an @ after the host; if it is part of a user name or password, \
         percent-encode it, or pass it as the proxy credentials"
    )]
    AtAfterHost,
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

/// The proxy at `proxy_url`, a URL that can hold a user name and password: the credentials,
/// percent-decoded, move out of the address.
///
/// An address that an unencoded `#`, `/` or `?` in a credential cut short is refused by
/// [`UpstreamProxy::new`].
pub(crate) fn proxy_from_url(proxy_url: &str) -> Result<UpstreamProxy, ProxyError> {
    let mut address = check_proxy_url(proxy_url)?;
    let credentials = if address.username().is_empty() && address.password().is_none() {
        None
    } else {
        let decoded = |part: &str| {
            percent_encoding::percent_decode_str(part)
                .decode_utf8()
                .map(std::borrow::Cow::into_owned)
                .map_err(|_| ProxyError::Unparseable("a user name or password in it is not UTF-8".to_string()))
        };
        Some(ProxyCredentials {
            username: decoded(address.username())?,
            password: decoded(address.password().unwrap_or(""))?,
        })
    };
    // ~keep Cannot fail: the address has a host, so it can hold userinfo and lose it.
    let _ = address.set_username("");
    let _ = address.set_password(None);
    UpstreamProxy::new(address, credentials)
}

/// The user name and password a proxy asks for, kept apart from its address.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyCredentials {
    pub username: String,
    pub password: String,
}

/// Shows the user name only: the password is the secret.
impl std::fmt::Debug for ProxyCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyCredentials")
            .field("username", &self.username)
            .field("password", &crate::redact::REDACTED)
            .finish()
    }
}

/// A proxy the browser HTTP clients send requests through: an address that holds no user
/// name or password, and the credentials apart from it.
///
/// ~keep The credentials join the address only inside [`Self::reqwest_proxy`] and
/// ~keep [`Self::wreq_proxy`], where the connection is made, so the address can be logged.
#[derive(Clone, PartialEq, Eq)]
pub struct UpstreamProxy {
    address: Url,
    credentials: Option<ProxyCredentials>,
}

impl UpstreamProxy {
    /// Refuses an address with a scheme the clients cannot use, an address that holds a user
    /// name or password (those go in `credentials`), and an address with an `@` after the host.
    pub fn new(address: Url, credentials: Option<ProxyCredentials>) -> Result<Self, ProxyError> {
        if !address.username().is_empty() || address.password().is_some() {
            return Err(ProxyError::CredentialsInAddress);
        }
        if [Some(address.path()), address.query(), address.fragment()]
            .into_iter()
            .flatten()
            .any(|part| part.contains('@'))
        {
            return Err(ProxyError::AtAfterHost);
        }
        if !address.has_host() {
            return Err(ProxyError::Unparseable(
                "expected an address such as http://proxy:8080".to_string(),
            ));
        }
        if !SUPPORTED_SCHEMES.contains(&address.scheme()) {
            return Err(ProxyError::UnsupportedScheme(address.scheme().to_string()));
        }
        Ok(Self { address, credentials })
    }

    /// The proxy's address. It never holds a user name or password.
    pub fn address(&self) -> &Url {
        &self.address
    }

    pub fn credentials(&self) -> Option<&ProxyCredentials> {
        self.credentials.as_ref()
    }

    /// The reqwest proxy, with the credentials sent as `Proxy-Authorization`.
    pub fn reqwest_proxy(&self) -> Result<reqwest::Proxy, ProxyError> {
        let proxy = reqwest::Proxy::all(self.address.as_str()).map_err(|e| ProxyError::Unparseable(e.to_string()))?;
        Ok(match &self.credentials {
            Some(credentials) => proxy.basic_auth(&credentials.username, &credentials.password),
            None => proxy,
        })
    }

    /// The wreq proxy, with the credentials sent as `Proxy-Authorization`.
    #[cfg(feature = "stealth")]
    pub fn wreq_proxy(&self) -> Result<wreq::Proxy, ProxyError> {
        let proxy = wreq::Proxy::all(self.address.as_str()).map_err(|e| ProxyError::Unparseable(e.to_string()))?;
        Ok(match &self.credentials {
            Some(credentials) => proxy.basic_auth(&credentials.username, &credentials.password),
            None => proxy,
        })
    }
}

/// Shows the address and whether credentials are set, never the credentials.
impl std::fmt::Debug for UpstreamProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamProxy")
            .field("address", &self.address.as_str())
            .field("credentials", &self.credentials.is_some())
            .finish()
    }
}

/// The proxy at `raw`, read as [`check_proxy_url`] reads it, for tests.
#[cfg(test)]
pub(crate) fn test_proxy(raw: &str) -> Result<UpstreamProxy, ProxyError> {
    UpstreamProxy::new(check_proxy_url(raw)?, None)
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

/// A local proxy that asks for credentials, for the tests of each proxy consumer.
#[cfg(test)]
pub(crate) mod credentialed_proxy {
    use std::sync::{Arc, Mutex};

    use base64::Engine as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{ProxyCredentials, UpstreamProxy, check_proxy_url};

    pub(crate) const PASSWORD: &str = "IMPL385-PROXY-PW";

    /// The proxy answers a request with the credentials with this.
    pub(crate) const ACCEPTED: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 9\r\nConnection: close\r\n\r\nvia-proxy";
    /// The proxy answers a request with the wrong credentials, or none, with this.
    pub(crate) const REFUSED: &str = "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// Start a server on 127.0.0.1 that records the head of every request it accepts. The
    /// proxy it returns sends `operator` and [`PASSWORD`] to it.
    pub(crate) async fn start() -> (UpstreamProxy, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = format!("http://{}", listener.local_addr().expect("addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 8192];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let answer = if proxy_authorization(&request).as_deref() == Some(expected_authorization().as_str()) {
                    ACCEPTED
                } else {
                    REFUSED
                };
                log.lock().expect("lock").push(request);
                let _ = socket.write_all(answer.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        let credentials = ProxyCredentials {
            username: "operator".to_string(),
            password: PASSWORD.to_string(),
        };
        let proxy = UpstreamProxy::new(check_proxy_url(&address).expect("an http address"), Some(credentials))
            .expect("a usable proxy");
        (proxy, requests)
    }

    /// `proxy` with a password the server refuses.
    pub(crate) fn with_wrong_password(proxy: &UpstreamProxy) -> UpstreamProxy {
        let credentials = ProxyCredentials {
            username: "operator".to_string(),
            password: format!("{PASSWORD}-WRONG"),
        };
        UpstreamProxy::new(proxy.address().clone(), Some(credentials)).expect("a usable proxy")
    }

    pub(crate) fn expected_authorization() -> String {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("operator:{PASSWORD}"));
        format!("Basic {token}")
    }

    pub(crate) fn proxy_authorization(request: &str) -> Option<String> {
        request
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("proxy-authorization:"))
            .map(|line| line.split_once(':').expect("header line").1.trim().to_string())
    }

    /// Panic unless the proxy received exactly one request for `target`, in absolute form,
    /// with the configured credentials.
    pub(crate) fn assert_one_authenticated_request(requests: &Mutex<Vec<String>>, target: &str) {
        let requests = requests.lock().expect("lock");
        assert_eq!(requests.len(), 1, "the proxy must receive the request: {requests:?}");
        assert!(
            requests[0].starts_with(&format!("GET {target} ")),
            "the proxy must receive the absolute-form request: {requests:?}"
        );
        assert_eq!(
            proxy_authorization(&requests[0]),
            Some(expected_authorization()),
            "the proxy must receive the configured credentials"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upstream_proxy_refuses_an_address_that_a_password_character_ended_early() {
        for raw in [
            "http://operator:4242#s3cr3t@proxy.test:8080",
            "http://operator:4242/s3cr3t@proxy.test:8080",
            "http://operator:4242?s3cr3t@proxy.test:8080",
        ] {
            let address = Url::parse(raw).expect("the url crate reads the cut-short address");
            let err = UpstreamProxy::new(address, None).expect_err("an @ after the host must be refused");
            assert!(err.to_string().contains("percent-encode"), "{raw}: {err}");
            credential_urls::assert_not_shown(raw, &err.to_string());
        }

        let encoded = proxy_from_url("http://operator:s3%23cr%2F3t%3F@proxy.test:8080")
            .expect("a percent-encoded password is usable");
        assert_eq!(encoded.address().as_str(), "http://proxy.test:8080/");
        let credentials = encoded.credentials().expect("the URL holds credentials");
        assert_eq!(
            (credentials.username.as_str(), credentials.password.as_str()),
            ("operator", "s3#cr/3t?")
        );

        let path = Url::parse("http://proxy.test:8080/proxy").expect("parses");
        assert!(
            UpstreamProxy::new(path, None).is_ok(),
            "a path with no @ stays usable, as the crawl config check accepts it"
        );
    }

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
    fn an_upstream_proxy_refuses_an_address_that_holds_credentials() {
        for raw in [
            "http://operator:IMPL385-UP@proxy.test:8080",
            "http://operator@proxy.test:8080",
            "http://:IMPL385-UP@proxy.test:8080",
        ] {
            let address = Url::parse(raw).expect("the address parses");
            let err = UpstreamProxy::new(address, None).expect_err("credentials belong apart from the address");
            assert_eq!(err, ProxyError::CredentialsInAddress, "{raw}");
            assert!(!err.to_string().contains("IMPL385-UP"), "{err}");
        }
        let err = UpstreamProxy::new(Url::parse("ftp://proxy.test:21").expect("parses"), None)
            .expect_err("a scheme the clients cannot use is refused");
        assert_eq!(err, ProxyError::UnsupportedScheme("ftp".to_string()));
        assert!(
            err.to_string().contains("'ftp'"),
            "the error must name the scheme: {err}"
        );
    }

    #[test]
    fn an_upstream_proxy_debug_shows_the_address_and_never_the_password() {
        let proxy = UpstreamProxy::new(
            Url::parse("http://proxy.test:8080").expect("parses"),
            Some(ProxyCredentials {
                username: "operator".to_string(),
                password: "IMPL385-UP-DBG".to_string(),
            }),
        )
        .expect("a usable proxy");
        let shown = format!("{proxy:?} {:?}", proxy.credentials());
        assert!(!shown.contains("IMPL385-UP-DBG"), "{shown}");
        assert!(
            shown.contains("proxy.test:8080") && shown.contains("operator"),
            "{shown}"
        );
        assert!(proxy.reqwest_proxy().is_ok());
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
