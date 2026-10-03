//! The DNS resolver the native clients connect through.
//!
//! Each client checks a URL with [`SsrfValidator::validate`] before it sends the request. That
//! check resolves the host, and the client's default resolver then resolves it again to
//! connect. A DNS answer that changes between the two lookups (rebinding) reaches an address
//! the policy denies. [`ValidatorResolver`] replaces the client's resolver with
//! [`SsrfValidator::resolve`], so the addresses the policy checked are the addresses the
//! client connects to.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::net::proxy::{ProxyError, UpstreamProxy, proxy_from_url};
use crate::net::ssrf::SsrfValidator;

/// Port the resolved addresses carry.
///
/// ~keep A resolver receives a bare host name. hyper and wreq replace port `0` with the
/// URL's port (or the scheme's default) before they connect.
const RESOLUTION_PORT: u16 = 0;

/// The error type both HTTP stacks expect from a resolver.
type ResolveError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SystemProxyIdentity {
    address: String,
    authorization: Option<Vec<u8>>,
    credentials: Option<(String, String)>,
}

#[derive(Clone, Debug)]
pub(crate) struct SystemProxy {
    upstream: UpstreamProxy,
    authorization: Option<reqwest::header::HeaderValue>,
}

impl SystemProxy {
    /// Select and admit the environment or operating-system proxy for `url`.
    ///
    /// ~keep Selection happens before client construction so a proxy connection does not
    /// ask the target's SSRF resolver to approve the proxy host, while a `NO_PROXY` request
    /// still uses the policy resolver.
    pub(crate) fn for_url(url: &url::Url) -> Result<Option<Self>, ProxyError> {
        let Ok(destination) = url.as_str().parse() else {
            return Ok(None);
        };
        let Some(intercepted) = hyper_util::client::proxy::matcher::Matcher::from_system().intercept(&destination)
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            upstream: proxy_from_url(&intercepted.uri().to_string())?,
            authorization: intercepted.basic_auth().cloned(),
        }))
    }

    #[cfg(test)]
    pub(crate) fn from_url(url: &str) -> Result<Self, ProxyError> {
        Ok(Self {
            upstream: proxy_from_url(url)?,
            authorization: None,
        })
    }

    pub(crate) fn identity(&self) -> SystemProxyIdentity {
        SystemProxyIdentity {
            address: self.upstream.address().as_str().to_owned(),
            authorization: self.authorization.as_ref().map(|value| value.as_bytes().to_vec()),
            credentials: self
                .upstream
                .credentials()
                .map(|value| (value.username.clone(), value.password.clone())),
        }
    }

    pub(crate) fn reqwest_proxy(&self) -> Result<reqwest::Proxy, ProxyError> {
        let proxy = self.upstream.reqwest_proxy()?;
        Ok(match &self.authorization {
            Some(authorization) => proxy.custom_http_auth(authorization.clone()),
            None => proxy,
        })
    }

    #[cfg(feature = "stealth")]
    pub(crate) fn wreq_proxy(&self) -> Result<wreq::Proxy, ProxyError> {
        let proxy = self.upstream.wreq_proxy()?;
        Ok(match &self.authorization {
            Some(authorization) => proxy.custom_http_auth(authorization.clone()),
            None => proxy,
        })
    }
}

pub(crate) trait SystemProxySelector: std::fmt::Debug + Send + Sync {
    fn proxy_for(&self, url: &url::Url) -> Result<Option<SystemProxy>, ProxyError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProxyRoute {
    Explicit,
    System,
    Direct,
}

impl ProxyRoute {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::System => "system",
            Self::Direct => "direct",
        }
    }
}

#[derive(Debug)]
pub(crate) struct EnvironmentSystemProxySelector;

impl SystemProxySelector for EnvironmentSystemProxySelector {
    fn proxy_for(&self, url: &url::Url) -> Result<Option<SystemProxy>, ProxyError> {
        SystemProxy::for_url(url)
    }
}

pub(crate) fn reqwest_builder_for_url(
    builder: reqwest::ClientBuilder,
    url: &url::Url,
    explicit: Option<&UpstreamProxy>,
    selector: &dyn SystemProxySelector,
    ssrf: &Arc<dyn SsrfValidator>,
) -> Result<reqwest::ClientBuilder, ProxyError> {
    reqwest_builder_and_route_for_url(builder, url, explicit, selector, ssrf).map(|(builder, _)| builder)
}

pub(crate) fn reqwest_builder_and_route_for_url(
    mut builder: reqwest::ClientBuilder,
    url: &url::Url,
    explicit: Option<&UpstreamProxy>,
    selector: &dyn SystemProxySelector,
    ssrf: &Arc<dyn SsrfValidator>,
) -> Result<(reqwest::ClientBuilder, ProxyRoute), ProxyError> {
    if let Some(proxy) = explicit {
        return Ok((builder.proxy(proxy.reqwest_proxy()?), ProxyRoute::Explicit));
    }
    match selector.proxy_for(url)? {
        Some(proxy) => Ok((builder.proxy(proxy.reqwest_proxy()?), ProxyRoute::System)),
        None => {
            builder = builder.no_proxy();
            Ok((with_policy_resolver(builder, false, ssrf), ProxyRoute::Direct))
        }
    }
}

/// A DNS resolver that returns only the addresses the SSRF policy permits.
#[derive(Debug, Clone)]
pub struct ValidatorResolver {
    ssrf: Arc<dyn SsrfValidator>,
}

impl ValidatorResolver {
    /// Build a resolver that asks `ssrf` for every host a client connects to.
    pub fn new(ssrf: Arc<dyn SsrfValidator>) -> Self {
        Self { ssrf }
    }

    async fn socket_addresses(ssrf: Arc<dyn SsrfValidator>, host: String) -> Result<Vec<SocketAddr>, ResolveError> {
        let addresses = ssrf.resolve(&host).await.map_err(|reason| {
            tracing::warn!(host = %host, %reason, "refusing to connect: the SSRF policy refused the resolved host");
            ResolveError::from(reason)
        })?;
        Ok(addresses
            .into_iter()
            .map(|ip| SocketAddr::new(ip, RESOLUTION_PORT))
            .collect())
    }
}

impl reqwest::dns::Resolve for ValidatorResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let lookup = Self::socket_addresses(self.ssrf.clone(), name.as_str().to_owned());
        Box::pin(async move { Ok(Box::new(lookup.await?.into_iter()) as reqwest::dns::Addrs) })
    }
}

#[cfg(feature = "stealth")]
impl wreq::dns::Resolve for ValidatorResolver {
    fn resolve(&self, name: wreq::dns::Name) -> wreq::dns::Resolving {
        let lookup = Self::socket_addresses(self.ssrf.clone(), name.as_str().to_owned());
        Box::pin(async move { Ok(Box::new(lookup.await?.into_iter()) as wreq::dns::Addrs) })
    }
}

/// Install [`ValidatorResolver`] on a reqwest client unless a proxy is set.
///
/// ~keep With a proxy the client resolves the proxy host, not the target, and the proxy
/// resolves the target. The policy would be applied to the wrong name, and a proxy on a
/// private address is a normal setup. The URL check still runs on the target.
pub(crate) fn with_policy_resolver(
    builder: reqwest::ClientBuilder,
    proxied: bool,
    ssrf: &Arc<dyn SsrfValidator>,
) -> reqwest::ClientBuilder {
    if proxied {
        return builder;
    }
    builder.dns_resolver(Arc::new(ValidatorResolver::new(ssrf.clone())))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::IpAddr;
    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use url::Url;

    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct TestProxySelector {
        proxy: Mutex<Option<SystemProxy>>,
        direct_hosts: Mutex<Vec<String>>,
    }

    impl TestProxySelector {
        pub(crate) fn set_proxy(&self, proxy: &str) {
            *self.proxy.lock().expect("proxy selector lock") = Some(SystemProxy::from_url(proxy).expect("test proxy"));
        }

        pub(crate) fn direct_host(&self, host: &str) {
            self.direct_hosts
                .lock()
                .expect("proxy selector lock")
                .push(host.to_owned());
        }
    }

    impl SystemProxySelector for TestProxySelector {
        fn proxy_for(&self, url: &url::Url) -> Result<Option<SystemProxy>, ProxyError> {
            if url.host_str().is_some_and(|host| {
                self.direct_hosts
                    .lock()
                    .expect("proxy selector lock")
                    .iter()
                    .any(|direct| direct == host)
            }) {
                return Ok(None);
            }
            Ok(self.proxy.lock().expect("proxy selector lock").clone())
        }
    }

    /// The address a rebinding DNS server gives the connection's lookup; the test policy denies it.
    pub(crate) const DENIED: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    /// The address it gives the check's lookup; the test policy permits it (TEST-NET-1, never routed).
    const ALLOWED: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));

    /// A policy behind a rebinding DNS server: the check's lookup of a name gets [`ALLOWED`],
    /// the connection's lookup gets [`DENIED`], and the policy denies [`DENIED`] only.
    ///
    /// `validate` is the check and `resolve` is the connection's lookup. A literal address in a
    /// URL passes, because no DNS answer is involved. Tests use the host `localhost`, which the
    /// system resolver answers with [`DENIED`]: a client that resolves the host itself instead
    /// of asking `resolve` connects there.
    #[derive(Debug, Default)]
    pub(crate) struct RebindingPolicy {
        pub(crate) resolved: Mutex<Vec<String>>,
    }

    impl RebindingPolicy {
        fn check(ip: IpAddr) -> Result<(), String> {
            if ip == DENIED {
                Err(format!("denied by the test policy: {ip}"))
            } else {
                Ok(())
            }
        }
    }

    #[async_trait::async_trait]
    impl SsrfValidator for RebindingPolicy {
        async fn validate(&self, url: &Url) -> Result<(), String> {
            match url.host() {
                Some(url::Host::Domain(_)) => Self::check(ALLOWED),
                Some(_) => Ok(()),
                None => Err("no host".to_owned()),
            }
        }

        async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
            self.resolved.lock().expect("lock").push(host.to_owned());
            Self::check(DENIED).map(|()| vec![DENIED])
        }
    }

    /// A server on the denied address that records every connection it accepts.
    pub(crate) async fn denied_server(response: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind((DENIED, 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                log.lock().expect("lock").push(
                    String::from_utf8_lossy(&buf[..read])
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_owned(),
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        (port, seen)
    }

    /// Resolves every host to one fixed address, which the policy permits.
    #[derive(Debug)]
    struct ResolvesTo(IpAddr);

    #[async_trait::async_trait]
    impl SsrfValidator for ResolvesTo {
        async fn validate(&self, _url: &Url) -> Result<(), String> {
            Ok(())
        }

        async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
            Ok(vec![self.0])
        }
    }

    #[test]
    fn request_route_classification_reports_only_the_selected_kind() {
        let ssrf: Arc<dyn SsrfValidator> = Arc::new(ResolvesTo(DENIED));
        let url = Url::parse("http://origin.test/").expect("URL");
        let selector = TestProxySelector::default();

        let (_, direct) = reqwest_builder_and_route_for_url(reqwest::Client::builder(), &url, None, &selector, &ssrf)
            .expect("direct route");
        assert_eq!(direct, ProxyRoute::Direct);
        assert_eq!(direct.as_str(), "direct");

        selector.set_proxy("http://localhost:8080");
        let (_, system) = reqwest_builder_and_route_for_url(reqwest::Client::builder(), &url, None, &selector, &ssrf)
            .expect("system proxy route");
        assert_eq!(system, ProxyRoute::System);
        assert_eq!(system.as_str(), "system");

        let explicit = proxy_from_url("http://localhost:8081").expect("explicit proxy");
        let (_, explicit) =
            reqwest_builder_and_route_for_url(reqwest::Client::builder(), &url, Some(&explicit), &selector, &ssrf)
                .expect("explicit proxy route");
        assert_eq!(explicit, ProxyRoute::Explicit);
        assert_eq!(explicit.as_str(), "explicit");
    }

    #[tokio::test]
    async fn a_client_connects_to_the_address_the_policy_resolved() {
        let (port, seen) = denied_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
        let ssrf: Arc<dyn SsrfValidator> = Arc::new(ResolvesTo(DENIED));
        let client = with_policy_resolver(reqwest::Client::builder(), false, &ssrf)
            .build()
            .expect("client");

        // ~keep `.invalid` never resolves, so the request can only reach the server through
        // ~keep the address the policy returned.
        let body = client
            .get(format!("http://rebind.invalid:{port}/"))
            .send()
            .await
            .expect("the resolved address must be connected to")
            .text()
            .await
            .expect("body");

        assert_eq!(body, "ok");
        assert_eq!(
            seen.lock().expect("lock").len(),
            1,
            "one connection to the resolved address"
        );
    }

    #[tokio::test]
    async fn a_proxied_client_does_not_ask_the_policy_to_resolve() {
        let policy = Arc::new(RebindingPolicy::default());
        let ssrf: Arc<dyn SsrfValidator> = policy.clone();
        let (port, seen) = denied_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
        let client = with_policy_resolver(
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .proxy(reqwest::Proxy::all(format!("http://localhost:{port}")).expect("proxy")),
            true,
            &ssrf,
        )
        .build()
        .expect("client");

        client
            .get("http://example.invalid/")
            .send()
            .await
            .expect("the proxy answers every request");

        assert!(
            policy.resolved.lock().expect("lock").is_empty(),
            "the proxy host is not the target, so the policy must not resolve it"
        );
        assert_eq!(seen.lock().expect("lock").len(), 1, "the request goes to the proxy");
    }

    /// Permits the host name `localhost` and refuses every other host, by name only.
    #[derive(Debug)]
    struct OnlyTheNameLocalhost;

    #[async_trait::async_trait]
    impl SsrfValidator for OnlyTheNameLocalhost {
        async fn validate(&self, url: &Url) -> Result<(), String> {
            match url.host() {
                Some(url::Host::Domain("localhost")) => Ok(()),
                _ => Err(format!("{url} is not the name localhost")),
            }
        }
    }

    #[tokio::test]
    async fn a_validator_that_decides_by_name_connects_to_what_the_system_resolves() {
        let (port, seen) = denied_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
        let ssrf: Arc<dyn SsrfValidator> = Arc::new(OnlyTheNameLocalhost);
        let client = with_policy_resolver(reqwest::Client::builder(), false, &ssrf)
            .build()
            .expect("client");

        // ~keep The default `resolve` does not check the addresses: this validator refuses every
        // ~keep literal address, so a check there would refuse the name it permits.
        let body = client
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .expect("the name the validator permits must be connected to")
            .text()
            .await
            .expect("body");

        assert_eq!(body, "ok");
        assert_eq!(seen.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn the_default_validator_refuses_a_name_that_resolves_into_private_space() {
        use crate::net::ssrf::DefaultSsrfValidator;

        let denying: Arc<dyn SsrfValidator> = Arc::new(DefaultSsrfValidator::with_deny_private(true));
        let error = denying
            .resolve("localhost")
            .await
            .expect_err("localhost resolves to loopback, which the deny-list refuses");
        assert!(
            error.starts_with("localhost resolves to the private/internal address "),
            "the refusal names the host and the address: {error}"
        );

        let permitting: Arc<dyn SsrfValidator> = Arc::new(DefaultSsrfValidator::with_deny_private(false));
        let addresses = permitting
            .resolve("localhost")
            .await
            .expect("with private networks allowed, loopback is permitted");
        assert!(
            !addresses.is_empty() && addresses.iter().all(IpAddr::is_loopback),
            "{addresses:?}"
        );
    }
}
