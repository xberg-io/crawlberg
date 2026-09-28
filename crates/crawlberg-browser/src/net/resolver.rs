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

use crate::net::ssrf::SsrfValidator;

/// Port the resolved addresses carry.
///
/// ~keep A resolver receives a bare host name. hyper and wreq replace port `0` with the
/// URL's port (or the scheme's default) before they connect.
const RESOLUTION_PORT: u16 = 0;

/// The error type both HTTP stacks expect from a resolver.
type ResolveError = Box<dyn std::error::Error + Send + Sync>;

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

    #[tokio::test]
    async fn the_default_resolve_checks_every_address_through_validate() {
        #[derive(Debug)]
        struct DenyLoopback;

        #[async_trait::async_trait]
        impl SsrfValidator for DenyLoopback {
            async fn validate(&self, url: &Url) -> Result<(), String> {
                match url.host() {
                    Some(url::Host::Ipv4(ip)) if ip.is_loopback() => Err(format!("loopback {ip}")),
                    Some(url::Host::Ipv6(ip)) if ip.is_loopback() => Err(format!("loopback {ip}")),
                    _ => Ok(()),
                }
            }
        }

        let error = DenyLoopback
            .resolve("localhost")
            .await
            .expect_err("localhost resolves to loopback, which validate refuses");
        assert!(error.starts_with("loopback "), "the refusal is validate's: {error}");

        let error = DenyLoopback
            .resolve("no-such-host.invalid")
            .await
            .expect_err("an unresolvable host has no address to connect to");
        assert!(error.starts_with("dns resolution failed"), "{error}");
    }
}
