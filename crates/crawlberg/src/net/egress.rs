//! The proxy on loopback that carries every socket of a Chrome page under IP-level SSRF policy.
//!
//! Chrome's request interception never sees a WebSocket handshake or a QUIC datagram, and
//! Chrome resolves a host again after the check has passed. Through this proxy each connection
//! is resolved once, checked with [`resolve_permitted`], and made to the checked address.
//! Behind the crawl's own proxy a host name goes to that proxy unresolved, as the HTTP client
//! sends it, and only an IP address is checked here. The proxy speaks SOCKS5 to Chrome, or HTTP
//! when the crawl's proxy is an HTTP proxy, so that proxy receives the requests Chrome would send it.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use fast_socks5::ReplyError;
use fast_socks5::server::Socks5ServerProtocol;
use fast_socks5::util::target_addr::TargetAddr;
use http_body_util::{Either, Empty};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls;

use crate::error::CrawlError;
use crate::net::resolver::resolve_permitted;
use crate::net::ssrf::SsrfPolicy;
use crate::proxy::ChromeProxy;

/// The proxy a connection leaves through: none, or the crawl's proxy by its scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Upstream {
    Direct,
    HttpConnect(String),
    HttpsConnect(String),
    Socks4(String),
    Socks5(String),
}

impl Upstream {
    fn of(proxy: Option<&ChromeProxy>) -> Self {
        let Some(proxy) = proxy else {
            return Self::Direct;
        };
        let (scheme, address) = proxy
            .server
            .split_once("://")
            .unwrap_or(("http", proxy.server.as_str()));
        let address = address.to_owned();
        match scheme {
            "https" => Self::HttpsConnect(address),
            "socks4" => Self::Socks4(address),
            "socks5" => Self::Socks5(address),
            _ => Self::HttpConnect(address),
        }
    }

    /// True when the upstream is an HTTP proxy, which Chrome speaks HTTP to.
    fn speaks_http(&self) -> bool {
        matches!(self, Self::HttpConnect(_) | Self::HttpsConnect(_))
    }
}

/// A running proxy. Dropping it stops the listener; connections already carried run to their end.
pub(crate) struct Egress {
    policy: SsrfPolicy,
    upstream: Upstream,
    address: SocketAddr,
    refused: Arc<Mutex<Vec<String>>>,
    listener: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Egress")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Drop for Egress {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl Egress {
    /// Start a proxy for `policy` that leaves through `upstream`, or none with no IP denials.
    pub(crate) async fn start(policy: &SsrfPolicy, upstream: Option<&ChromeProxy>) -> Result<Option<Self>, CrawlError> {
        if !policy.enforces_ip_denials() {
            return Ok(None);
        }
        let upstream = Upstream::of(upstream);
        let socket = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to start the browser's SSRF proxy: {e}")))?;
        let address = socket
            .local_addr()
            .map_err(|e| CrawlError::browser_error(format!("failed to start the browser's SSRF proxy: {e}")))?;
        let refused = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::spawn(serve(socket, policy.clone(), upstream.clone(), Arc::clone(&refused)));
        Ok(Some(Self {
            policy: policy.clone(),
            upstream,
            address,
            refused,
            listener,
        }))
    }

    /// Whether this proxy serves `policy` leaving through `upstream`. The proxy checks an address
    /// against the private-network flag and configured IP lists, so those are compared.
    pub(crate) fn serves(&self, policy: &SsrfPolicy, upstream: Option<&ChromeProxy>) -> bool {
        self.policy.deny_private == policy.deny_private
            && self.policy.allowlist == policy.allowlist
            && self.policy.denylist == policy.denylist
            && Upstream::of(upstream) == self.upstream
    }

    /// The proxy as Chrome takes it.
    pub(crate) fn chrome_proxy(&self) -> ChromeProxy {
        let scheme = if self.upstream.speaks_http() { "http" } else { "socks5" };
        ChromeProxy {
            server: format!("{scheme}://{}", self.address),
        }
    }

    /// Every `host:port` refused so far, in order.
    pub(crate) fn refused(&self) -> Vec<String> {
        self.refused.lock().map(|refused| refused.clone()).unwrap_or_default()
    }
}

/// The policy the SSRF proxy applies to a Chrome at `endpoint`, or none when that Chrome is on
/// another machine: it cannot reach a proxy on this machine's loopback. Under IP-level denial
/// that case is logged once per `warned`, which the caller keeps for the browser's life.
pub(crate) fn socket_policy<'a>(
    policy: &'a SsrfPolicy,
    endpoint: Option<&str>,
    warned: &std::sync::Once,
) -> Option<&'a SsrfPolicy> {
    match endpoint {
        Some(endpoint) if !is_loopback_endpoint(endpoint) => {
            if policy.enforces_ip_denials() {
                warned.call_once(|| {
                    let host = url::Url::parse(endpoint)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_owned));
                    tracing::warn!(
                        host = host.as_deref().unwrap_or_default(),
                        "the SSRF network deny policy cannot check the WebSocket, WebRTC and WebTransport \
                         connections of a browser.endpoint on another machine; its HTTP requests are still checked"
                    );
                });
            }
            None
        }
        _ => Some(policy),
    }
}

/// True when `url` names this machine: `localhost` or a loopback address.
fn is_loopback_endpoint(url: &str) -> bool {
    match url::Url::parse(url)
        .ok()
        .and_then(|url| url.host().map(|host| host.to_owned()))
    {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Add each `host:port` in `sockets` to `refused` once, unless a URL already listed names it:
/// the check that refused a request also stops its connection, so both refuse the same thing.
pub(crate) fn add_refused(refused: &mut Vec<String>, sockets: Vec<String>) {
    for socket in sockets {
        let named = refused.iter().any(|listed| {
            *listed == socket
                || url::Url::parse(listed).is_ok_and(|url| {
                    url.host_str()
                        .zip(url.port_or_known_default())
                        .is_some_and(|(host, port)| format!("{host}:{port}") == socket)
                })
        });
        if !named {
            refused.push(socket);
        }
    }
}

async fn serve(socket: TcpListener, policy: SsrfPolicy, upstream: Upstream, refused: Arc<Mutex<Vec<String>>>) {
    let policy = Arc::new(policy);
    let upstream = Arc::new(upstream);
    while let Ok((client, _)) = socket.accept().await {
        let (policy, upstream, refused) = (Arc::clone(&policy), Arc::clone(&upstream), Arc::clone(&refused));
        tokio::spawn(async move {
            let carried = if upstream.speaks_http() {
                carry_http(client, policy, upstream, refused).await
            } else {
                carry(client, &policy, &upstream, &refused).await
            };
            if let Err(error) = carried {
                tracing::debug!(%error, "the browser's SSRF proxy dropped a connection");
            }
        });
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Carry one SOCKS5 connection from Chrome.
async fn carry(
    client: TcpStream,
    policy: &SsrfPolicy,
    upstream: &Upstream,
    refused: &Mutex<Vec<String>>,
) -> Result<(), BoxError> {
    let (protocol, command, target) = Socks5ServerProtocol::accept_no_auth(client)
        .await?
        .read_command()
        .await?;
    if command != fast_socks5::Socks5Command::TCPConnect {
        protocol.reply_error(&ReplyError::CommandNotSupported).await?;
        return Ok(());
    }
    let (host, port) = match target {
        TargetAddr::Ip(address) => (address.ip().to_string(), address.port()),
        TargetAddr::Domain(host, port) => (host, port),
    };
    let Some(targets) = permitted(&host, port, policy, upstream, refused).await else {
        protocol.reply_error(&ReplyError::ConnectionNotAllowed).await?;
        return Ok(());
    };
    match connect_first(&targets, upstream).await {
        Ok((mut server, bound)) => {
            let mut client = protocol.reply_success(bound).await?;
            tokio::io::copy_bidirectional(&mut client, &mut server).await?;
            Ok(())
        }
        Err(error) => {
            protocol.reply_error(&ReplyError::HostUnreachable).await?;
            Err(error.into())
        }
    }
}

type Body = Either<Incoming, Empty<bytes::Bytes>>;

/// Carry one HTTP proxy connection from Chrome: a CONNECT is tunnelled, any other request goes
/// to the upstream unchanged, each after the same check as a SOCKS5 connection.
async fn carry_http(
    client: TcpStream,
    policy: Arc<SsrfPolicy>,
    upstream: Arc<Upstream>,
    refused: Arc<Mutex<Vec<String>>>,
) -> Result<(), BoxError> {
    let service = hyper::service::service_fn(move |request| {
        let (policy, upstream, refused) = (Arc::clone(&policy), Arc::clone(&upstream), Arc::clone(&refused));
        async move { forward(request, &policy, &upstream, &refused).await }
    });
    hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(client), service)
        .with_upgrades()
        .await?;
    Ok(())
}

async fn forward(
    request: hyper::Request<Incoming>,
    policy: &SsrfPolicy,
    upstream: &Upstream,
    refused: &Mutex<Vec<String>>,
) -> Result<hyper::Response<Body>, hyper::Error> {
    let uri = request.uri();
    let Some(host) = uri
        .host()
        .map(|host| host.trim_start_matches('[').trim_end_matches(']').to_owned())
    else {
        return Ok(status(hyper::StatusCode::BAD_REQUEST));
    };
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("https") { 443 } else { 80 });
    let Some(targets) = permitted(&host, port, policy, upstream, refused).await else {
        return Ok(status(hyper::StatusCode::FORBIDDEN));
    };
    if request.method() == hyper::Method::CONNECT {
        let Ok((mut server, _)) = connect_first(&targets, upstream).await else {
            return Ok(status(hyper::StatusCode::BAD_GATEWAY));
        };
        tokio::spawn(async move {
            if let Ok(upgraded) = hyper::upgrade::on(request).await {
                let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(upgraded), &mut server).await;
            }
        });
        return Ok(status(hyper::StatusCode::OK));
    }
    let Ok(stream) = proxy_stream(upstream).await else {
        return Ok(status(hyper::StatusCode::BAD_GATEWAY));
    };
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(connection);
    Ok(sender.send_request(request).await?.map(Either::Left))
}

fn status(code: hyper::StatusCode) -> hyper::Response<Body> {
    let mut response = hyper::Response::new(Either::Right(Empty::new()));
    *response.status_mut() = code;
    response
}

/// Where `host:port` may be reached, or `None` after recording its refusal.
async fn permitted(
    host: &str,
    port: u16,
    policy: &SsrfPolicy,
    upstream: &Upstream,
    refused: &Mutex<Vec<String>>,
) -> Option<Vec<Target>> {
    // ~keep With an upstream proxy a name goes to it unresolved, as the HTTP client sends it:
    // ~keep the upstream resolves it, so a name only the upstream can resolve keeps working.
    if *upstream != Upstream::Direct && host.parse::<IpAddr>().is_err() {
        return Some(vec![Target::Name(host.to_owned(), port)]);
    }
    // ~keep The addresses come from this one lookup and are never looked up again, so Chrome
    // ~keep cannot reach an address other than the one the policy passed.
    match resolve_permitted(host, policy).await {
        Ok(addresses) => Some(
            addresses
                .into_iter()
                .map(|mut address| {
                    address.set_port(port);
                    Target::Address(address)
                })
                .collect(),
        ),
        Err(error) => {
            // ~keep An IPv6 socket is written `[::1]:80`, as a URL names its host, so the
            // ~keep refusal matches a refused URL for the same socket.
            let socket = match host.parse::<IpAddr>() {
                Ok(ip) => SocketAddr::new(ip, port).to_string(),
                Err(_) => format!("{host}:{port}"),
            };
            tracing::warn!(%socket, %error, "the browser's SSRF proxy refused a connection");
            if let Ok(mut refused) = refused.lock() {
                refused.push(socket);
            }
            None
        }
    }
}

/// A stream to the first of `targets` that connects, and the address it reached (unspecified
/// for a name an upstream resolves).
async fn connect_first(targets: &[Target], upstream: &Upstream) -> std::io::Result<(Box<dyn Stream>, SocketAddr)> {
    let mut last_error = None;
    for target in targets {
        match connect(target, upstream).await {
            Ok(server) => {
                let bound = match target {
                    Target::Address(address) => *address,
                    Target::Name(..) => SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0)),
                };
                return Ok((server, bound));
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("no address to connect to")))
}

/// Where a connection goes: an address the policy passed, or a name an upstream proxy resolves.
enum Target {
    Address(SocketAddr),
    Name(String, u16),
}

impl Target {
    fn host(&self) -> String {
        match self {
            Self::Address(address) => address.ip().to_string(),
            Self::Name(host, _) => host.clone(),
        }
    }

    fn port(&self) -> u16 {
        match self {
            Self::Address(address) => address.port(),
            Self::Name(_, port) => *port,
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Address(address) => address.fmt(f),
            Self::Name(host, port) => write!(f, "{host}:{port}"),
        }
    }
}

/// A stream to `target`, through `upstream` when there is one.
async fn connect(target: &Target, upstream: &Upstream) -> std::io::Result<Box<dyn Stream>> {
    match upstream {
        Upstream::Direct => match target {
            Target::Address(address) => Ok(Box::new(TcpStream::connect(address).await?)),
            Target::Name(..) => Err(std::io::Error::other("a name is carried only to an upstream proxy")),
        },
        Upstream::HttpConnect(_) | Upstream::HttpsConnect(_) => {
            Ok(Box::new(http_connect(proxy_stream(upstream).await?, target).await?))
        }
        Upstream::Socks4(proxy) => {
            // ~keep A name goes as SOCKS4a, which the upstream resolves.
            let stream =
                tokio_socks::tcp::Socks4Stream::connect(proxy.as_str(), (target.host().as_str(), target.port()))
                    .await
                    .map_err(std::io::Error::other)?;
            Ok(Box::new(stream))
        }
        Upstream::Socks5(proxy) => {
            let stream = fast_socks5::client::Socks5Stream::connect(
                proxy.as_str(),
                target.host(),
                target.port(),
                fast_socks5::client::Config::default(),
            )
            .await
            .map_err(std::io::Error::other)?;
            Ok(Box::new(stream))
        }
    }
}

/// A stream to an HTTP upstream proxy itself, over TLS for an https one.
async fn proxy_stream(upstream: &Upstream) -> std::io::Result<Box<dyn Stream>> {
    match upstream {
        Upstream::HttpConnect(proxy) => Ok(Box::new(TcpStream::connect(proxy.as_str()).await?)),
        Upstream::HttpsConnect(proxy) => {
            let host = proxy.rsplit_once(':').map_or(proxy.as_str(), |(host, _)| host);
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(std::io::Error::other)?;
            let stream = tokio_rustls::TlsConnector::from(tls_config()?)
                .connect(name, TcpStream::connect(proxy.as_str()).await?)
                .await?;
            Ok(Box::new(stream))
        }
        Upstream::Direct | Upstream::Socks4(_) | Upstream::Socks5(_) => {
            Err(std::io::Error::other("the upstream is not an HTTP proxy"))
        }
    }
}

/// The TLS client an https upstream is reached with: reqwest's, with the platform's certificate
/// check and the process's crypto provider, else aws-lc-rs.
fn tls_config() -> std::io::Result<Arc<rustls::ClientConfig>> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(BuilderVerifierExt::with_platform_verifier)
        .map_err(std::io::Error::other)?
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// `stream` after an HTTP CONNECT to `target` through it succeeded.
async fn http_connect<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut stream: S,
    target: &Target,
) -> std::io::Result<S> {
    stream
        .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await?;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 || stream.read_u8().await.map(|byte| head.push(byte)).is_err() {
            return Err(std::io::Error::other("the proxy closed the CONNECT tunnel"));
        }
    }
    let status = head.split(|&byte| byte == b' ').nth(1).unwrap_or_default();
    if !status.starts_with(b"2") {
        return Err(std::io::Error::other("the proxy refused the CONNECT tunnel"));
    }
    Ok(stream)
}

trait Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Stream for T {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::AsyncBufReadExt;

    use super::*;
    use crate::net::ssrf::HostMatcher;

    fn policy(allowlist: Vec<HostMatcher>) -> SsrfPolicy {
        SsrfPolicy {
            deny_private: true,
            allowlist,
            ..Default::default()
        }
    }

    fn loopback_cidr() -> HostMatcher {
        HostMatcher::cidr("127.0.0.0/8").expect("valid CIDR")
    }

    /// Both loopback ranges. A hosts file can map `localhost` to `::1` as well as `127.0.0.1`,
    /// and a name with one refused answer is refused whole, so `localhost` needs both.
    fn loopback_both_families() -> Vec<HostMatcher> {
        vec![loopback_cidr(), HostMatcher::cidr("::1/128").expect("valid CIDR")]
    }

    /// A listener that counts its connections and keeps each one open.
    async fn counting_listener(address: &str) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind(address).await.expect("the test listener must bind");
        let bound = listener.local_addr().expect("a bound listener has an address");
        let count = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&count);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let _keep = stream;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                });
            }
        });
        (bound, count)
    }

    async fn through(egress: &Egress, host: &str, port: u16) -> Result<(), fast_socks5::SocksError> {
        fast_socks5::client::Socks5Stream::connect(
            egress.address,
            host.to_owned(),
            port,
            fast_socks5::client::Config::default(),
        )
        .await
        .map(drop)
    }

    /// Ask `egress` for `host:port` in the protocol it speaks to Chrome. An answer that takes
    /// longer than a few seconds is an error, so an upstream spoken to in the wrong protocol
    /// fails the test instead of hanging it.
    async fn ask(egress: &Egress, host: &str, port: u16) -> Result<(), BoxError> {
        tokio::time::timeout(std::time::Duration::from_secs(5), ask_once(egress, host, port))
            .await
            .unwrap_or_else(|_| Err(format!("no answer for {host}:{port} within 5 seconds").into()))
    }

    async fn ask_once(egress: &Egress, host: &str, port: u16) -> Result<(), BoxError> {
        if egress.upstream.speaks_http() {
            let target = Target::Name(host.to_owned(), port);
            http_connect(TcpStream::connect(egress.address).await?, &target)
                .await
                .map(drop)?;
            Ok(())
        } else {
            Ok(through(egress, host, port).await?)
        }
    }

    async fn start(policy: &SsrfPolicy, upstream: Option<&str>) -> Egress {
        let upstream = upstream.map(|server| ChromeProxy {
            server: server.to_owned(),
        });
        Egress::start(policy, upstream.as_ref())
            .await
            .expect("the proxy must start")
            .expect("deny_private is on, so the proxy runs")
    }

    #[tokio::test]
    async fn no_proxy_runs_when_deny_private_is_off() {
        let off = SsrfPolicy {
            deny_private: false,
            ..Default::default()
        };
        let egress = Egress::start(&off, None).await.expect("start must not fail");
        assert!(egress.is_none(), "with deny_private off no proxy may run");
    }

    #[tokio::test]
    async fn configured_denylist_starts_a_proxy_when_private_denial_is_off() {
        let policy = SsrfPolicy {
            deny_private: false,
            denylist: vec![HostMatcher::cidr("203.0.113.0/24").expect("valid CIDR")],
            ..Default::default()
        };

        let egress = Egress::start(&policy, None).await.expect("start must not fail");

        assert!(
            egress.is_some(),
            "configured network denials still require the browser egress proxy"
        );
    }

    #[tokio::test]
    async fn a_proxy_serves_only_its_own_allowlist_and_upstream() {
        let egress = start(&policy(Vec::new()), None).await;
        let other_redirects = SsrfPolicy {
            max_redirects: 0,
            ..policy(Vec::new())
        };
        assert!(
            egress.serves(&other_redirects, None),
            "a policy the proxy checks the same way must reuse it"
        );
        assert!(
            !egress.serves(&policy(vec![loopback_cidr()]), None),
            "a policy with another allowlist must get its own proxy"
        );
        let mut custom_denial = policy(Vec::new());
        custom_denial
            .denylist
            .push(HostMatcher::cidr("203.0.113.0/24").expect("valid CIDR"));
        assert!(
            !egress.serves(&custom_denial, None),
            "a policy with another denylist must get its own proxy"
        );
        let upstream = ChromeProxy {
            server: "socks5://127.0.0.1:1080".to_owned(),
        };
        assert!(
            !egress.serves(&policy(Vec::new()), Some(&upstream)),
            "another upstream must get its own proxy"
        );
    }

    #[tokio::test]
    async fn a_denied_address_is_refused_and_an_allowlisted_one_connects() {
        let (target, count) = counting_listener("127.0.0.1:0").await;
        let denied = start(&policy(Vec::new()), None).await;
        assert!(
            through(&denied, "127.0.0.1", target.port()).await.is_err(),
            "loopback must be refused"
        );
        assert!(
            through(&denied, "localhost", target.port()).await.is_err(),
            "a name for loopback must be refused"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "a refused connection must never reach the address"
        );
        assert_eq!(
            denied.refused(),
            vec![
                format!("127.0.0.1:{}", target.port()),
                format!("localhost:{}", target.port())
            ],
            "each refusal is recorded as host:port"
        );

        let allowed = start(&policy(loopback_both_families()), None).await;
        through(&allowed, "127.0.0.1", target.port())
            .await
            .expect("an allowlisted address must connect");
        through(&allowed, "localhost", target.port())
            .await
            .expect("a name for an allowlisted address must connect");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "both allowed connections must reach the address"
        );
        assert!(allowed.refused().is_empty(), "nothing was refused");
    }

    #[tokio::test]
    async fn a_direct_connection_goes_to_the_checked_address_never_the_name() {
        let refused = Mutex::new(Vec::new());
        let targets = permitted(
            "localhost",
            9,
            &policy(loopback_both_families()),
            &Upstream::Direct,
            &refused,
        )
        .await
        .expect("an allowlisted name must be permitted");
        assert!(
            targets
                .iter()
                .all(|target| matches!(target, Target::Address(address) if address.port() == 9)),
            "a name is resolved once and carried as its checked address, got {:?}",
            targets.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
        assert!(
            connect(&Target::Name("localhost".to_owned(), 9), &Upstream::Direct)
                .await
                .is_err(),
            "without an upstream a name is never looked up again"
        );
    }

    #[tokio::test]
    async fn an_ipv6_refusal_is_written_as_a_url_names_it() {
        let denied = start(&policy(Vec::new()), None).await;
        assert!(
            through(&denied, "::1", 9).await.is_err(),
            "IPv6 loopback must be refused"
        );
        assert_eq!(denied.refused(), vec!["[::1]:9".to_owned()]);

        let mut listed = vec!["http://[::1]:9/x".to_owned()];
        add_refused(&mut listed, denied.refused());
        add_refused(&mut listed, vec!["127.0.0.1:9".to_owned(), "127.0.0.1:9".to_owned()]);
        assert_eq!(
            listed,
            vec!["http://[::1]:9/x".to_owned(), "127.0.0.1:9".to_owned()],
            "a socket a listed URL names is dropped, and each other socket is listed once"
        );
    }

    #[test]
    fn only_a_browser_on_this_machine_gets_the_socket_check() {
        let on = policy(Vec::new());
        for (endpoint, checked) in [
            (None, true),
            (Some("ws://localhost:9222/devtools/browser/x"), true),
            (Some("ws://127.8.9.10:9222"), true),
            (Some("ws://[::1]:9222"), true),
            (Some("ws://chrome.internal:9222"), false),
            (Some("ws://10.0.0.5:9222"), false),
            (Some("wss://[2001:db8::1]:9222"), false),
        ] {
            let got = socket_policy(&on, endpoint, &std::sync::Once::new()).is_some();
            assert_eq!(got, checked, "{endpoint:?}");
        }
    }

    /// An upstream that records what each client asks it for, speaking `kind`.
    async fn recording_upstream(kind: &'static str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("the upstream must bind");
        let bound = listener.local_addr().expect("a bound listener has an address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let record = Arc::clone(&record);
                tokio::spawn(async move {
                    let asked = match kind {
                        "http" => {
                            let mut stream = tokio::io::BufReader::new(stream);
                            let mut line = String::new();
                            let _ = stream.read_line(&mut line).await;
                            let mut rest = String::new();
                            while stream.read_line(&mut rest).await.is_ok_and(|n| n > 2) {
                                rest.clear();
                            }
                            let _ = stream.get_mut().write_all(b"HTTP/1.1 200 OK\r\n\r\n").await;
                            record.lock().expect("lock").push(line.trim_end().to_owned());
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            return;
                        }
                        "tls" => {
                            let mut stream = stream;
                            format!("first byte {:#04x}", stream.read_u8().await.unwrap_or_default())
                        }
                        "socks4" => {
                            let mut stream = stream;
                            let mut head = [0u8; 8];
                            let _ = stream.read_exact(&mut head).await;
                            let mut rest = Vec::new();
                            let mut byte = [0u8; 1];
                            let mut zeros = 0;
                            let domain = head[4..7] == [0, 0, 0] && head[7] != 0;
                            while zeros < if domain { 2 } else { 1 } && stream.read_exact(&mut byte).await.is_ok() {
                                if byte[0] == 0 {
                                    zeros += 1;
                                } else if zeros == 1 {
                                    rest.push(byte[0]);
                                }
                            }
                            let _ = stream.write_all(&[0, 0x5a, 0, 0, 0, 0, 0, 0]).await;
                            let port = u16::from_be_bytes([head[2], head[3]]);
                            let host = if domain {
                                String::from_utf8_lossy(&rest).into_owned()
                            } else {
                                std::net::Ipv4Addr::new(head[4], head[5], head[6], head[7]).to_string()
                            };
                            record.lock().expect("lock").push(format!("{host}:{port}"));
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            return;
                        }
                        _ => {
                            let Ok(protocol) = Socks5ServerProtocol::accept_no_auth(stream).await else {
                                return;
                            };
                            let Ok((protocol, _, target)) = protocol.read_command().await else {
                                return;
                            };
                            let _ = protocol.reply_success(SocketAddr::from(([0, 0, 0, 0], 0))).await;
                            target.to_string()
                        }
                    };
                    record.lock().expect("lock").push(asked);
                });
            }
        });
        (bound, seen)
    }

    #[tokio::test]
    async fn each_upstream_gets_a_name_unresolved_and_only_a_permitted_address() {
        let public = "93.184.215.14";
        for (kind, scheme) in [("http", "http"), ("socks5", "socks5"), ("socks4", "socks4")] {
            let (upstream, seen) = recording_upstream(kind).await;
            let egress = start(&policy(Vec::new()), Some(&format!("{scheme}://{upstream}"))).await;
            ask(&egress, "proxy-only.invalid", 443)
                .await
                .unwrap_or_else(|e| panic!("{kind}: a name must go to the upstream: {e}"));
            ask(&egress, public, 443)
                .await
                .unwrap_or_else(|e| panic!("{kind}: a public address must go to the upstream: {e}"));
            assert!(
                ask(&egress, "127.0.0.1", 443).await.is_err(),
                "{kind}: a denied address must be refused"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let seen = seen.lock().expect("lock").clone();
            let expected: Vec<String> = match kind {
                "http" => vec![
                    "CONNECT proxy-only.invalid:443 HTTP/1.1".into(),
                    format!("CONNECT {public}:443 HTTP/1.1"),
                ],
                _ => vec!["proxy-only.invalid:443".into(), format!("{public}:443")],
            };
            assert_eq!(
                seen, expected,
                "{kind}: the upstream sees the name and the permitted address, never the denied one"
            );
            assert_eq!(egress.refused(), vec!["127.0.0.1:443".to_owned()], "{kind}");
        }
    }

    #[tokio::test]
    async fn a_plain_request_goes_to_an_http_upstream_unchanged_unless_denied() {
        let (upstream, seen) = recording_upstream("http").await;
        let egress = start(&policy(Vec::new()), Some(&format!("http://{upstream}"))).await;
        assert!(
            egress.chrome_proxy().server.starts_with("http://"),
            "Chrome must speak HTTP to it"
        );
        let address = egress.address;
        let status_of = move |target: &'static str| async move {
            let mut client = TcpStream::connect(address).await.expect("the proxy must accept");
            client
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .expect("the request must be written");
            let mut line = String::new();
            let _ = tokio::io::BufReader::new(client).read_line(&mut line).await;
            line
        };
        let passed = status_of("http://proxy-only.invalid/page").await;
        assert!(
            passed.starts_with("HTTP/1.1 200"),
            "a name must be forwarded, got {passed:?}"
        );
        let denied = status_of("http://127.0.0.1:9/page").await;
        assert!(
            denied.starts_with("HTTP/1.1 403"),
            "a denied address must be refused, got {denied:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            seen.lock().expect("lock").clone(),
            vec!["GET http://proxy-only.invalid/page HTTP/1.1".to_owned()],
            "the upstream receives the request as Chrome sent it, and never the denied one"
        );
        assert_eq!(egress.refused(), vec!["127.0.0.1:9".to_owned()]);
    }

    #[tokio::test]
    async fn a_connection_tries_the_next_permitted_address_when_one_fails() {
        let closed = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener must bind")
            .local_addr()
            .expect("a bound listener has an address");
        let (open, count) = counting_listener("127.0.0.1:0").await;
        let (_, bound) = connect_first(&[Target::Address(closed), Target::Address(open)], &Upstream::Direct)
            .await
            .expect("the second address accepts");
        assert_eq!(bound, open, "the connection must go to the address that accepted");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_request_without_a_host_or_a_reachable_upstream_gets_an_error_status() {
        let closed = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener must bind")
            .local_addr()
            .expect("a bound listener has an address");
        let egress = start(&policy(Vec::new()), Some(&format!("http://{closed}"))).await;
        for (request, expected) in [
            ("GET /page HTTP/1.1\r\nHost: x\r\n\r\n", "HTTP/1.1 400"),
            (
                "GET http://proxy-only.invalid/page HTTP/1.1\r\nHost: x\r\n\r\n",
                "HTTP/1.1 502",
            ),
            (
                "CONNECT proxy-only.invalid:443 HTTP/1.1\r\nHost: x\r\n\r\n",
                "HTTP/1.1 502",
            ),
        ] {
            let mut client = TcpStream::connect(egress.address).await.expect("the proxy must accept");
            client
                .write_all(request.as_bytes())
                .await
                .expect("the request must be written");
            let mut line = String::new();
            let _ = tokio::io::BufReader::new(client).read_line(&mut line).await;
            assert!(
                line.starts_with(expected),
                "{request:?}: expected {expected}, got {line:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_https_upstream_is_spoken_to_over_tls() {
        let (upstream, seen) = recording_upstream("tls").await;
        let egress = start(&policy(Vec::new()), Some(&format!("https://{upstream}"))).await;
        let _ = ask(&egress, "proxy-only.invalid", 443).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            seen.lock().expect("lock").clone(),
            vec!["first byte 0x16".to_owned()],
            "the connection to an https upstream must open with a TLS handshake record"
        );
    }

    #[tokio::test]
    async fn a_socks5_command_other_than_connect_is_refused_as_unsupported() {
        let egress = start(&policy(Vec::new()), None).await;
        let mut client = TcpStream::connect(egress.address).await.expect("the proxy must accept");
        client.write_all(&[5, 1, 0]).await.expect("greeting");
        let mut chosen = [0u8; 2];
        client.read_exact(&mut chosen).await.expect("method reply");
        // ~keep UDP ASSOCIATE for 0.0.0.0:0, the request WebRTC or QUIC over SOCKS5 would send.
        client
            .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .expect("request");
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.expect("command reply");
        assert_eq!(
            reply,
            [5, 7],
            "a UDP ASSOCIATE must be refused as an unsupported command, not checked as a connection"
        );
    }

    #[tokio::test]
    async fn a_dropped_proxy_stops_listening() {
        let egress = start(&policy(Vec::new()), None).await;
        let address = egress.address;
        drop(egress);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            TcpStream::connect(address).await.is_err(),
            "a dropped proxy must stop listening on {address}"
        );
    }

    #[test]
    fn a_socket_is_listed_once_and_not_when_a_refused_url_names_it() {
        let mut refused = vec!["http://10.0.0.1/x".to_owned(), "https://[::1]/".to_owned()];
        add_refused(
            &mut refused,
            vec![
                "10.0.0.1:80".to_owned(),
                "[::1]:443".to_owned(),
                "10.0.0.1:81".to_owned(),
                "10.0.0.1:81".to_owned(),
            ],
        );
        assert_eq!(
            refused,
            vec![
                "http://10.0.0.1/x".to_owned(),
                "https://[::1]/".to_owned(),
                "10.0.0.1:81".to_owned()
            ],
            "a socket a refused URL names, or one already listed, must not be listed again"
        );
    }

    #[tokio::test]
    async fn an_upstream_that_refuses_the_tunnel_gives_no_stream() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener must bind");
        let upstream = listener.local_addr().expect("a bound listener has an address");
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0u8; 256];
                let _ = stream.read(&mut request).await;
                let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
            }
        });
        let stream = TcpStream::connect(upstream)
            .await
            .expect("connect to the test upstream");
        let tunnel = http_connect(stream, &Target::Name("example.com".to_owned(), 443)).await;
        assert!(tunnel.is_err(), "a CONNECT the upstream answers with 403 must fail");
    }

    #[test]
    fn only_a_remote_browser_under_ip_level_denial_logs_the_warning() {
        let off = SsrfPolicy {
            deny_private: false,
            ..Default::default()
        };
        let custom_denial = SsrfPolicy {
            deny_private: false,
            denylist: vec![HostMatcher::cidr("203.0.113.0/24").expect("valid CIDR")],
            ..Default::default()
        };
        for (policy, endpoint, warns) in [
            (policy(Vec::new()), "ws://10.0.0.5:9222", true),
            (policy(Vec::new()), "ws://127.0.0.1:9222", false),
            (custom_denial, "ws://10.0.0.5:9222", true),
            (off, "ws://10.0.0.5:9222", false),
        ] {
            let warned = std::sync::Once::new();
            let _ = socket_policy(&policy, Some(endpoint), &warned);
            assert_eq!(
                warned.is_completed(),
                warns,
                "{endpoint}, IP-level denial {}",
                policy.enforces_ip_denials()
            );
        }
    }
}
