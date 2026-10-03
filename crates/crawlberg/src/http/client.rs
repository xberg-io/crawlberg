//! The shared `reqwest::Client` cache and the builder that populates it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::error::CrawlError;
use crate::types::{AuthConfig, CrawlConfig};

/// Identity of the `reqwest::Client` configuration knobs that legitimately vary per
/// fetch — timeout, cookie jar, proxy, and auth — used to key the shared client cache
/// in [`build_client`] so that requests sharing an identity reuse one connection pool
/// instead of paying a fresh TCP/TLS handshake on every call.
///
/// Auth is included even though it is applied as a per-request header (not baked into
/// the `reqwest::Client` itself) because a shared client's cookie jar (when
/// `cookies_enabled`) must not be reused across distinct credentials — otherwise two
/// concurrent sessions to the same host with different auth would leak session cookies
/// between them.
///
/// Runtime identity is included because hyper drives each pooled connection with a task
/// spawned on the runtime that built the client. When that runtime is dropped the
/// connection task dies, but the client stays in this process-global cache, so a caller
/// on a new runtime would check out a dead connection and fail mid-request. Keying on the
/// runtime keeps each one's pool to itself; a cold entry costs a handshake, not
/// correctness. `Handle::try_current()` is `Err` outside a runtime, which is its own
/// stable identity. ~keep
#[derive(Clone, PartialEq, Eq, Hash)]
struct ClientCacheKey {
    runtime: Option<tokio::runtime::Id>,
    timeout_micros: u128,
    cookies_enabled: bool,
    proxy: String,
    auth: String,
    ssrf: String,
}

impl ClientCacheKey {
    fn from_config(config: &CrawlConfig) -> Self {
        Self {
            runtime: tokio::runtime::Handle::try_current().map(|handle| handle.id()).ok(),
            timeout_micros: config.request_timeout.as_micros(),
            cookies_enabled: config.cookies_enabled,
            proxy: proxy_identity(config),
            auth: auth_identity(config),
            ssrf: ssrf_identity(config),
        }
    }
}

/// Encode the part of `config`'s SSRF policy that is baked into the client's DNS resolver.
///
/// ~keep The resolver captures the policy at build time, so two configs with different
/// policies must not share a cached client — otherwise the first caller's policy would
/// silently govern the second's connections. Only `deny_private` and `allowlist` reach the
/// resolver: `scheme_allowlist` and `max_redirects` are enforced in `validate_url` against
/// the URL, never during resolution, so folding them in would fragment the cache for
/// nothing.
fn ssrf_identity(config: &CrawlConfig) -> String {
    format!("{}:{:?}", config.ssrf.deny_private, config.ssrf.allowlist)
}

/// Encode `config`'s proxy configuration as an opaque identity string.
///
/// A `ProxyProvider` is a trait object with no `Eq`/`Hash` impl, so its identity is its
/// `Arc` data address — two `CrawlConfig`s sharing the same provider `Arc` (the normal
/// case: one engine, cloned config) resolve to the same key.
fn proxy_identity(config: &CrawlConfig) -> String {
    if let Some(ref provider) = config.proxy_provider {
        format!("provider:{:p}", std::sync::Arc::as_ptr(provider))
    } else if let Some(ref proxy) = config.proxy {
        format!(
            "static:{}:{}:{}",
            proxy.url,
            proxy.username.as_deref().unwrap_or(""),
            proxy.password.as_deref().unwrap_or("")
        )
    } else {
        "none".to_owned()
    }
}

/// Encode `config`'s auth configuration as an opaque identity string.
fn auth_identity(config: &CrawlConfig) -> String {
    match &config.auth {
        Some(AuthConfig::Basic { username, password }) => format!("basic:{username}:{password}"),
        Some(AuthConfig::Bearer { token }) => format!("bearer:{token}"),
        Some(AuthConfig::Header { name, value }) => format!("header:{name}:{value}"),
        None => "none".to_owned(),
    }
}

/// Built `reqwest::Client`s, keyed by [`ClientCacheKey`].
///
/// ~keep `build_client` is called on the hot fetch path (once per tier attempt in
/// `engine/mod.rs::run_tier`), so without this cache every HTTP request pays a fresh
/// TCP/TLS handshake and gets no connection-pool reuse. `reqwest::Client` is
/// `Arc`-backed internally, so cloning a cached entry is cheap, and each distinct
/// proxy/auth/timeout/cookie identity still gets its own client rather than one client
/// silently serving unrelated sessions (see [`ClientCacheKey`]).
#[derive(Default)]
struct ClientCache {
    clients: Mutex<HashMap<ClientCacheKey, std::sync::Arc<StaticClients>>>,
}

impl ClientCache {
    /// The cached client for `config`'s identity, built and cached on a miss.
    fn get_or_build(&self, config: &CrawlConfig) -> Result<reqwest::Client, CrawlError> {
        Ok(self.get_or_build_set(config)?.client.clone())
    }

    fn get_or_build_set(&self, config: &CrawlConfig) -> Result<std::sync::Arc<StaticClients>, CrawlError> {
        let key = ClientCacheKey::from_config(config);
        if let Ok(clients) = self.clients.lock()
            && let Some(clients) = clients.get(&key)
        {
            return Ok(std::sync::Arc::clone(clients));
        }

        let clients = std::sync::Arc::new(StaticClients::new(config)?);

        self.insert(key, &clients);

        Ok(clients)
    }

    /// Store `clients` under `key`, clearing the cache wholesale once it is full.
    fn insert(&self, key: ClientCacheKey, clients: &std::sync::Arc<StaticClients>) {
        let Ok(mut cache) = self.clients.lock() else {
            return;
        };
        insert_bounded(&mut cache, key, std::sync::Arc::clone(clients));
    }

    /// Whether a client is cached for `config`'s identity.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn contains(&self, config: &CrawlConfig) -> bool {
        let key = ClientCacheKey::from_config(config);
        self.clients
            .lock()
            .map(|clients| clients.contains_key(&key))
            .unwrap_or(false)
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct StaticClients {
    client: reqwest::Client,
    jar: Option<std::sync::Arc<crate::net::cookie::PolicyCookieStore>>,
    environment: Mutex<HashMap<EnvironmentProxyIdentity, reqwest::Client>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl StaticClients {
    fn new(config: &CrawlConfig) -> Result<Self, CrawlError> {
        let jar = new_cookie_jar(config);
        let client = build_static_client(config, jar.clone())?;
        Ok(Self {
            client,
            jar,
            environment: Mutex::new(HashMap::new()),
        })
    }

    fn environment_client(
        &self,
        config: &CrawlConfig,
        proxy: &EnvironmentProxy,
    ) -> Result<reqwest::Client, CrawlError> {
        let identity = proxy.identity();
        if let Ok(clients) = self.environment.lock()
            && let Some(client) = clients.get(&identity)
        {
            return Ok(client.clone());
        }
        let client = configure_environment_client(config, proxy, self.jar.clone())?
            .build()
            .map_err(|e| CrawlError::other(format!("Failed to build HTTP client: {e}")))?;
        if let Ok(mut clients) = self.environment.lock() {
            insert_bounded(&mut clients, identity, client.clone());
        }
        Ok(client)
    }
}

#[cfg(target_arch = "wasm32")]
struct StaticClients {
    client: reqwest::Client,
}

#[cfg(target_arch = "wasm32")]
impl StaticClients {
    fn new(config: &CrawlConfig) -> Result<Self, CrawlError> {
        Ok(Self {
            client: build_static_client(config)?,
        })
    }
}

/// The one process-wide [`ClientCache`] that [`build_client`] serves from.
fn client_cache() -> &'static ClientCache {
    static CACHE: OnceLock<ClientCache> = OnceLock::new();
    CACHE.get_or_init(ClientCache::default)
}

/// Upper bound on distinct cached clients.
///
/// ~keep An unbounded cache is a slow leak: a long-lived process that rotates proxies
/// or per-tenant auth mints a new identity per rotation and never releases the old
/// client — nor the provider `Arc` captured inside it. Past this many entries the cache
/// is cleared wholesale rather than evicted by recency; entries are interchangeable
/// (rebuilding one costs a handshake, not correctness), so tracking access order would
/// buy nothing for the extra state.
const MAX_CACHED_CLIENTS: usize = 64;

/// Build a `reqwest::Client` with the given configuration (redirect policy, timeout, cookies, proxy).
///
/// Returns a cached, cheaply-cloned client when one matching this configuration's
/// [`ClientCacheKey`] already exists; otherwise builds one and caches it for reuse.
///
/// With a `proxy_provider`, this is the client for a request that goes direct: each request
/// gets its own client from [`request_client`].
pub(crate) fn build_client(config: &CrawlConfig) -> Result<reqwest::Client, CrawlError> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(provider) = &config.proxy_provider {
        return provider_clients(config, provider).client(config, None);
    }

    client_cache().get_or_build(config)
}

/// The client for one request to `url`.
///
/// With a `proxy_provider`, the provider is asked once and the request gets the checked
/// client for its answer; `None` is the provider's explicit direct route, while an unusable
/// proxy is an error. Without a provider, the system proxy matcher selects either a proxy
/// client or the direct `client`.
///
/// ~keep The pick is made here, above reqwest, and never in a reqwest custom proxy: reqwest
/// ~keep asks a custom proxy up to three times for one request (for the request headers and
/// ~keep again for the connection), so a rotating provider could send the credentials of one
/// ~keep proxy to another. Each proxy has its own client, so a pooled connection is never
/// ~keep reused through a proxy that was not picked.
pub(crate) fn request_client(
    client: &reqwest::Client,
    config: &CrawlConfig,
    url: &url::Url,
) -> Result<reqwest::Client, CrawlError> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(provider) = &config.proxy_provider {
        let proxy = crate::proxy::pick_proxy(provider.as_ref(), url.host_str().unwrap_or(""))?;
        return provider_clients(config, provider).client(config, proxy.as_ref());
    }
    #[cfg(not(target_arch = "wasm32"))]
    if config.proxy.is_none()
        && let Some(proxy) = EnvironmentProxy::for_url(url)?
    {
        return client_cache()
            .get_or_build_set(config)?
            .environment_client(config, &proxy);
    }
    let _ = (config, url);
    Ok(client.clone())
}

/// The clients of one `proxy_provider` config: one for each proxy it picked, and one for
/// requests that go direct.
///
/// ~keep They share one cookie jar, so a cookie set through one proxy is sent through the
/// ~keep next, as it was when one client served every proxy.
#[cfg(not(target_arch = "wasm32"))]
struct ProviderClients {
    /// ~keep Held so the provider's address, which keys this entry, is not reused by another
    /// ~keep provider while the entry is cached.
    _provider: std::sync::Arc<dyn crate::ProxyProvider>,
    jar: Option<std::sync::Arc<crate::net::cookie::PolicyCookieStore>>,
    clients: Mutex<HashMap<String, reqwest::Client>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ProviderClients {
    /// The client that sends through `proxy`, or direct when it is `None`.
    fn client(
        &self,
        config: &CrawlConfig,
        proxy: Option<&crate::proxy::AdmittedProxy>,
    ) -> Result<reqwest::Client, CrawlError> {
        let identity = proxy.map(admitted_identity).unwrap_or_default();
        if let Ok(clients) = self.clients.lock()
            && let Some(client) = clients.get(&identity)
        {
            return Ok(client.clone());
        }
        let client = configure_client(config, proxy, self.jar.clone())?
            .build()
            .map_err(|e| CrawlError::other(format!("Failed to build HTTP client: {e}")))?;
        if let Ok(mut clients) = self.clients.lock() {
            insert_bounded(&mut clients, identity, client.clone());
        }
        Ok(client)
    }
}

/// The address and credentials of an admitted proxy, as one identity string.
#[cfg(not(target_arch = "wasm32"))]
fn admitted_identity(proxy: &crate::proxy::AdmittedProxy) -> String {
    let (username, password) = proxy
        .credentials()
        .map_or(("", ""), |c| (c.username.as_str(), c.password.as_str()));
    format!("{}\n{username}\n{password}", proxy.address().as_url())
}

#[cfg(not(target_arch = "wasm32"))]
struct EnvironmentProxy {
    admitted: crate::proxy::AdmittedProxy,
    authorization: Option<reqwest::header::HeaderValue>,
}

#[cfg(not(target_arch = "wasm32"))]
impl EnvironmentProxy {
    /// Select and admit the environment or operating-system proxy for `url`.
    ///
    /// ~keep Selection happens above reqwest so a proxy client can omit
    /// [`crate::net::resolver::PolicyResolver`],
    /// while a `NO_PROXY` request keeps the direct client's resolver. Letting reqwest select
    /// inside one client makes the resolver mistake a private proxy for a private target.
    fn for_url(url: &url::Url) -> Result<Option<Self>, CrawlError> {
        let Ok(destination) = url.as_str().parse() else {
            return Ok(None);
        };
        let Some(intercepted) = hyper_util::client::proxy::matcher::Matcher::from_system().intercept(&destination)
        else {
            return Ok(None);
        };
        let admitted = crate::proxy::admit_proxy(&crate::types::ProxyConfig {
            url: intercepted.uri().to_string(),
            ..crate::types::ProxyConfig::default()
        })?;
        crate::proxy::ensure_supported_scheme(admitted.address())?;
        Ok(Some(Self {
            admitted,
            authorization: intercepted.basic_auth().cloned(),
        }))
    }

    fn identity(&self) -> EnvironmentProxyIdentity {
        EnvironmentProxyIdentity {
            address: admitted_identity(&self.admitted),
            authorization: self.authorization.as_ref().map(|value| value.as_bytes().to_vec()),
        }
    }

    fn reqwest_proxy(&self) -> Result<reqwest::Proxy, CrawlError> {
        let proxy = self.admitted.reqwest_proxy()?;
        Ok(match &self.authorization {
            Some(authorization) => proxy.custom_http_auth(authorization.clone()),
            None => proxy,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(PartialEq, Eq, Hash)]
struct EnvironmentProxyIdentity {
    address: String,
    authorization: Option<Vec<u8>>,
}

/// Process-wide cache of [`ProviderClients`], keyed by [`ClientCacheKey`] with the
/// provider's identity as its proxy.
#[cfg(not(target_arch = "wasm32"))]
fn provider_clients(
    config: &CrawlConfig,
    provider: &std::sync::Arc<dyn crate::ProxyProvider>,
) -> std::sync::Arc<ProviderClients> {
    type Cache = Mutex<HashMap<ClientCacheKey, std::sync::Arc<ProviderClients>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let key = ClientCacheKey::from_config(config);
    let fresh = || {
        std::sync::Arc::new(ProviderClients {
            _provider: std::sync::Arc::clone(provider),
            jar: new_cookie_jar(config),
            clients: Mutex::new(HashMap::new()),
        })
    };
    let Ok(mut cache) = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() else {
        return fresh();
    };
    if let Some(clients) = cache.get(&key) {
        return std::sync::Arc::clone(clients);
    }
    let clients = fresh();
    insert_bounded(&mut cache, key, std::sync::Arc::clone(&clients));
    clients
}

/// wasm32 has no redirect policy, request timeout, cookie jar, proxy or DNS resolver to
/// configure: the browser's own `fetch` owns every one of them. ~keep
#[cfg(target_arch = "wasm32")]
fn build_static_client(_config: &CrawlConfig) -> Result<reqwest::Client, CrawlError> {
    reqwest::Client::builder()
        .build()
        .map_err(|e| CrawlError::other(format!("Failed to build HTTP client: {e}")))
}

/// Build the client for a config with at most a static proxy.
#[cfg(not(target_arch = "wasm32"))]
fn build_static_client(
    config: &CrawlConfig,
    jar: Option<std::sync::Arc<crate::net::cookie::PolicyCookieStore>>,
) -> Result<reqwest::Client, CrawlError> {
    let proxy = config.proxy.as_ref().map(crate::proxy::admit_proxy).transpose()?;
    configure_client(config, proxy.as_ref(), jar)?
        .build()
        .map_err(|e| CrawlError::other(format!("Failed to build HTTP client: {e}")))
}

/// A fresh cookie jar when `config` enables cookies.
///
/// ~keep `cookie_provider`, not `cookie_store(true)`: reqwest's default jar loads no
/// ~keep public-suffix list, so a host may set Domain= to a shared multi-tenant suffix
/// ~keep (herokuapp.com, github.io, a bare TLD). One client is reused across a whole crawl
/// ~keep that can span hosts, so that would be a supercookie leak between unrelated tenants.
#[cfg(not(target_arch = "wasm32"))]
fn new_cookie_jar(config: &CrawlConfig) -> Option<std::sync::Arc<crate::net::cookie::PolicyCookieStore>> {
    config
        .cookies_enabled
        .then(|| std::sync::Arc::new(crate::net::cookie::PolicyCookieStore::default()))
}

/// Apply `config`, `proxy` and `jar` to a fresh `reqwest::ClientBuilder`.
#[cfg(not(target_arch = "wasm32"))]
fn configure_client(
    config: &CrawlConfig,
    proxy: Option<&crate::proxy::AdmittedProxy>,
    jar: Option<std::sync::Arc<crate::net::cookie::PolicyCookieStore>>,
) -> Result<reqwest::ClientBuilder, CrawlError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(config.request_timeout);

    if let Some(jar) = jar {
        builder = builder.cookie_provider(jar);
    }

    // ~keep Closes the DNS-rebinding TOCTOU: `validate_url` resolves the host and checks
    // the answers, then hyper resolves it *again* to connect, so the checked addresses are
    // not the connected ones. `PolicyResolver` re-checks inside the resolution hyper
    // actually uses, leaving no second lookup to disagree with the first.
    //
    // Skipped whenever this client sends through a proxy, because hyper then resolves the
    // *proxy* host rather than the target: the policy would be applied to the wrong name (a
    // proxy on a private address is a normal, previously-working setup), and the target's
    // resolution happens at the proxy, out of this process's reach, so client-side
    // pinning cannot be achieved through a proxy at all. `validate_url`'s own pre-check
    // still runs on the target in that case.
    match proxy {
        Some(proxy) => builder = builder.proxy(proxy.reqwest_proxy()?),
        None => {
            builder = builder
                .no_proxy()
                .dns_resolver(std::sync::Arc::new(crate::net::resolver::PolicyResolver::new(
                    config.ssrf.clone(),
                )));
        }
    }

    Ok(builder)
}

#[cfg(not(target_arch = "wasm32"))]
fn configure_environment_client(
    config: &CrawlConfig,
    proxy: &EnvironmentProxy,
    jar: Option<std::sync::Arc<crate::net::cookie::PolicyCookieStore>>,
) -> Result<reqwest::ClientBuilder, CrawlError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(config.request_timeout)
        .proxy(proxy.reqwest_proxy()?);
    if let Some(jar) = jar {
        builder = builder.cookie_provider(jar);
    }
    Ok(builder)
}

/// Insert `value` under `key`, clearing `cache` wholesale once it holds [`MAX_CACHED_CLIENTS`].
fn insert_bounded<K: std::hash::Hash + Eq, V>(cache: &mut HashMap<K, V>, key: K, value: V) {
    if cache.len() >= MAX_CACHED_CLIENTS {
        tracing::debug!(
            cached = cache.len(),
            cap = MAX_CACHED_CLIENTS,
            "HTTP client cache full, clearing"
        );
        cache.clear();
    }
    cache.insert(key, value);
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::net::ssrf::SsrfPolicy;
    use crate::types::ProxyConfig;
    use std::time::Duration;

    #[test]
    fn get_or_build_keeps_the_entry_for_a_matching_config() {
        // ~keep A cache of its own: the process-wide one is shared with every test in this
        // binary, and any of them can fill it past its cap and clear it mid-test.
        let cache = ClientCache::default();
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_273),
            ..CrawlConfig::default()
        };

        let _first = cache.get_or_build(&config).expect("first build must succeed");
        assert!(
            cache.contains(&config),
            "a build must populate the cache after building a client"
        );

        let _second = cache
            .get_or_build(&config)
            .expect("second build with the same config must succeed");
        assert!(
            cache.contains(&config),
            "the cache entry must still be present after a second build with a matching identity"
        );
    }

    /// Join an error with every error in its `source()` chain.
    ///
    /// ~keep reqwest reports a resolver refusal as a generic connect error and keeps the
    /// underlying cause only in the chain, so the policy reason is invisible to `Display`
    /// on the outermost error alone.
    fn error_chain(error: &dyn std::error::Error) -> String {
        let mut parts = vec![error.to_string()];
        let mut current = error.source();
        while let Some(cause) = current {
            parts.push(cause.to_string());
            current = cause.source();
        }
        parts.join(" / ")
    }

    /// The end-to-end proof that [`build_client`] actually installs [`PolicyResolver`].
    ///
    /// ~keep The resolver's own unit tests exercise it in isolation, so all of them still
    /// pass if the `dns_resolver` call is dropped from `build_client`. This one fails,
    /// because it goes through a real client and asserts on what the connection did.
    #[tokio::test]
    async fn build_client_enforces_the_ssrf_policy_during_dns_resolution() {
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_276),
            ssrf: SsrfPolicy {
                deny_private: true,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };
        let client = build_client(&config).expect("client must build");

        // ~keep Port 1 is never listening, so a request that got past the resolver would
        // fail with a connection-refused error instead — a different message, which is
        // exactly what distinguishes "policy enforced" from "policy absent" here.
        let error = client
            .get("http://localhost:1/")
            .send()
            .await
            .expect_err("localhost resolves to loopback and must be refused by the policy");

        let chain = error_chain(&error);
        assert!(
            chain.contains("denied by SSRF policy: loopback"),
            "expected the resolver to refuse the loopback answer, got: {chain}"
        );
    }

    #[tokio::test]
    async fn build_client_skips_the_policy_resolver_when_a_proxy_is_configured() {
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_277),
            proxy: Some(ProxyConfig {
                url: "http://127.0.0.1:1".to_owned(),
                ..ProxyConfig::default()
            }),
            ssrf: SsrfPolicy {
                deny_private: true,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };
        let client = build_client(&config).expect("client must build");

        let error = client
            .get("http://localhost:1/")
            .send()
            .await
            .expect_err("the proxy is not listening, so the request must fail");

        let chain = error_chain(&error);
        assert!(
            !chain.contains("denied by SSRF policy"),
            "hyper resolves the proxy host, not the target, so the policy must not be \
             applied during resolution here; got: {chain}"
        );
    }

    #[test]
    fn build_client_uses_distinct_cache_entries_for_distinct_ssrf_policies() {
        let permissive = CrawlConfig {
            request_timeout: Duration::from_millis(918_278),
            ssrf: SsrfPolicy {
                deny_private: false,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };
        let restrictive = CrawlConfig {
            request_timeout: Duration::from_millis(918_278),
            ssrf: SsrfPolicy {
                deny_private: true,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };

        let cache = ClientCache::default();

        let _permissive_client = cache.get_or_build(&permissive).expect("permissive client must build");
        assert!(
            !cache.contains(&restrictive),
            "a client built under deny_private=false must not be served to a deny_private=true \
             config — its resolver carries the permissive policy"
        );

        let _restrictive_client = cache.get_or_build(&restrictive).expect("restrictive client must build");
        assert!(
            cache.contains(&permissive) && cache.contains(&restrictive),
            "both policies must hold their own cache entry"
        );
    }

    /// A `reqwest::Client` cached on one tokio runtime must not be handed to another.
    ///
    /// ~keep hyper drives each pooled connection with a task spawned on the runtime that
    /// created it, so when that runtime is dropped the connection dies while the client
    /// stays in this process-global cache. A later caller on a new runtime then checks out
    /// a corpse and fails mid-request -- as `error sending request` if it dies during send,
    /// or `error decoding response body` (classified `DataLoss`) if it dies during
    /// `resp.chunk()`. Neither is retryable, since `retry_count` defaults to 0 and
    /// `should_retry_error` only matches status-derived variants. Measured in a standalone
    /// harness at ~8.5% of requests across 28 short-lived runtimes; 0% once the cache key
    /// carries runtime identity. Every consumer's `#[tokio::test]` suite is this shape.
    ///
    /// SCOPE: this asserts cache-key differentiation, which is the mechanism, and it fails
    /// 100% of the time without the fix. It deliberately does NOT reproduce the mid-flight
    /// request failure -- that reproduction is statistical (~8.5%), and a test that passes
    /// 91% of the time on broken code is worse than no test, because it reads as a pass.
    /// A deterministic version would have to block a pooled connection's task until after
    /// its runtime is dropped, which reqwest exposes no hook for.
    #[test]
    fn build_client_uses_distinct_cache_entries_across_tokio_runtimes() {
        let cache = ClientCache::default();
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_276),
            ..CrawlConfig::default()
        };

        let runtime_a = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime a must build");
        runtime_a.block_on(async {
            let _client = cache.get_or_build(&config).expect("client must build on runtime a");
            assert!(
                cache.contains(&config),
                "building on runtime a must cache that runtime's identity"
            );
        });
        drop(runtime_a);

        let runtime_b = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime b must build");
        runtime_b.block_on(async {
            assert!(
                !cache.contains(&config),
                "a client cached on a since-dropped runtime must not be reused on a new one: \
                 its pooled connections are driven by tasks that died with that runtime"
            );
        });
    }

    #[test]
    fn build_client_uses_distinct_cache_entries_for_distinct_timeouts() {
        let cache = ClientCache::default();
        let config_a = CrawlConfig {
            request_timeout: Duration::from_millis(918_274),
            ..CrawlConfig::default()
        };
        let config_b = CrawlConfig {
            request_timeout: Duration::from_millis(918_275),
            ..CrawlConfig::default()
        };

        let _a = cache.get_or_build(&config_a).expect("client a must build");
        assert!(
            cache.contains(&config_a),
            "config_a's identity must be cached after building it"
        );
        assert!(
            !cache.contains(&config_b),
            "building a client for config_a must not also cache config_b's distinct identity"
        );
    }

    fn provider_config(timeout_millis: u64) -> CrawlConfig {
        CrawlConfig {
            request_timeout: Duration::from_millis(timeout_millis),
            proxy_provider: Some(std::sync::Arc::new(crate::proxy::StaticProxyProvider::new(vec![
                ProxyConfig {
                    url: "http://proxy.test:8080".to_owned(),
                    ..ProxyConfig::default()
                },
            ]))),
            ..CrawlConfig::default()
        }
    }

    #[test]
    fn with_a_proxy_provider_the_client_for_a_config_is_its_direct_client_not_a_second_one() {
        let config = provider_config(918_277);
        let _client = build_client(&config).expect("client must build");
        assert!(
            !client_cache().contains(&config),
            "a provider config must not build a client of its own beside its per-proxy clients"
        );
        let clients = provider_clients(&config, config.proxy_provider.as_ref().expect("provider set"));
        let cached = clients.clients.lock().expect("lock").len();
        assert_eq!(cached, 1, "the direct client of the provider must be cached once");
    }

    #[test]
    fn a_request_asks_the_provider_for_the_host_of_its_url() {
        #[derive(Debug, Default)]
        struct Hosts(std::sync::Mutex<Vec<String>>);
        impl crate::ProxyProvider for Hosts {
            fn next_proxy(&self, host: &str) -> Option<ProxyConfig> {
                self.0.lock().expect("hosts lock").push(host.to_owned());
                None
            }
        }
        let provider = std::sync::Arc::new(Hosts::default());
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_279),
            proxy_provider: Some(provider.clone()),
            ..CrawlConfig::default()
        };
        let client = build_client(&config).expect("client must build");
        let url = url::Url::parse("http://page.example.com:8080/a").expect("test URL must parse");
        let _client = request_client(&client, &config, &url).expect("client must build");
        assert_eq!(*provider.0.lock().expect("hosts lock"), ["page.example.com"]);
    }

    #[test]
    fn a_request_refuses_an_unsupported_proxy_from_the_provider() {
        let config = CrawlConfig {
            proxy_provider: Some(std::sync::Arc::new(crate::proxy::StaticProxyProvider::new(vec![
                ProxyConfig {
                    url: "ftp://proxy.test:21".to_owned(),
                    ..ProxyConfig::default()
                },
            ]))),
            ..CrawlConfig::default()
        };
        let client = build_client(&config).expect("the provider's direct client must build");
        let url = url::Url::parse("https://page.example.com/").expect("test URL must parse");

        let error = request_client(&client, &config, &url)
            .expect_err("an unsupported provider proxy must fail instead of sending direct");

        assert_eq!(
            error.to_string(),
            "invalid_config: invalid proxy URL scheme 'ftp': expected http or https"
        );
    }

    /// A keep-alive HTTP proxy on a local port that answers every request with `200 ok` and
    /// counts the connections it accepts.
    async fn counting_proxy() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = format!("http://{}", listener.local_addr().expect("local address"));
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0_u8; 1024];
                    while let Ok(read @ 1..) = stream.read(&mut buf).await {
                        request.extend_from_slice(&buf[..read]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            request.clear();
                            let reply = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
                            if stream.write_all(reply).await.is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        (address, accepted)
    }

    #[tokio::test]
    async fn a_proxy_picked_twice_gets_one_client_and_a_second_proxy_gets_its_own() {
        let config = provider_config(918_278);
        let clients = provider_clients(&config, config.proxy_provider.as_ref().expect("provider set"));
        let admit = |url: &str| {
            crate::proxy::admit_proxy(&ProxyConfig {
                url: url.to_owned(),
                ..ProxyConfig::default()
            })
            .expect("a plain proxy is admitted")
        };
        let ((a_url, at_a), (b_url, at_b)) = (counting_proxy().await, counting_proxy().await);
        let (a, b) = (admit(&a_url), admit(&b_url));
        for proxy in [&a, &a, &b] {
            let client = clients.client(&config, Some(proxy)).expect("client must build");
            let response = client
                .get("http://site.test/")
                .send()
                .await
                .expect("the request through the proxy must succeed");
            let _body = response.text().await.expect("the body must read");
        }
        let cached = clients.clients.lock().expect("lock").len();
        assert_eq!(cached, 2, "one client for each picked proxy");
        let connections = |count: &std::sync::atomic::AtomicUsize| count.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            connections(&at_a),
            1,
            "the second pick of proxy A must reuse the first pick's client and its pooled connection"
        );
        assert_eq!(connections(&at_b), 1, "proxy B must get its own client");
    }

    #[test]
    fn a_full_client_cache_is_cleared_before_the_next_insert() {
        let mut cache = HashMap::new();
        for key in 0..=MAX_CACHED_CLIENTS {
            insert_bounded(&mut cache, key, ());
        }
        assert_eq!(cache.len(), 1, "the insert past the cap must start from an empty cache");
        assert!(cache.contains_key(&MAX_CACHED_CLIENTS), "the newest entry must be kept");
    }

    /// A hit must serve the cached client itself, not a fresh build.
    ///
    /// ~keep Two clients cannot be compared for identity, so the test plants a client whose
    /// resolver refuses loopback under a permissive config's key. Only the planted client
    /// refuses `localhost`; a fresh build for the permissive config would try to connect.
    #[tokio::test]
    async fn get_or_build_serves_the_cached_client_on_a_hit() {
        let cache = ClientCache::default();
        let permissive = CrawlConfig {
            ssrf: SsrfPolicy {
                deny_private: false,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };
        let restrictive = CrawlConfig {
            ssrf: SsrfPolicy {
                deny_private: true,
                ..SsrfPolicy::default()
            },
            ..CrawlConfig::default()
        };
        let planted = configure_client(&restrictive, None, None)
            .expect("restrictive builder must configure")
            .build()
            .expect("restrictive client must build");
        cache.insert(ClientCacheKey::from_config(&permissive), &planted);

        let served = cache.get_or_build(&permissive).expect("a hit must not fail");
        let error = served
            .get("http://localhost:1/")
            .send()
            .await
            .expect_err("port 1 is never listening");

        let chain = error_chain(&error);
        assert!(
            chain.contains("denied by SSRF policy: loopback"),
            "the hit must serve the planted client, whose resolver refuses loopback; got: {chain}"
        );
    }

    #[test]
    fn a_full_cache_is_cleared_before_the_next_insert() {
        let cache = ClientCache::default();
        let client = cache.get_or_build(&CrawlConfig::default()).expect("client must build");
        let config_at = |millis: u64| CrawlConfig {
            request_timeout: Duration::from_millis(millis),
            ..CrawlConfig::default()
        };
        for millis in 1..MAX_CACHED_CLIENTS as u64 {
            cache.insert(ClientCacheKey::from_config(&config_at(millis)), &client);
        }
        assert!(
            cache.contains(&CrawlConfig::default()) && cache.contains(&config_at(1)),
            "the cache must hold every entry up to its cap"
        );

        let past_cap = config_at(MAX_CACHED_CLIENTS as u64);
        cache.insert(ClientCacheKey::from_config(&past_cap), &client);
        assert!(
            cache.contains(&past_cap),
            "the entry that found the cache full must be stored"
        );
        assert!(
            !cache.contains(&CrawlConfig::default()) && !cache.contains(&config_at(1)),
            "an insert into a full cache must clear the earlier entries"
        );
    }

    /// `build_client` must store what it builds in the process-wide cache.
    ///
    /// ~keep Every test in this binary shares that cache, and any of them can fill it past
    /// its cap, which clears it. A sentinel entry, stored first, tells a clear apart from a
    /// missing store: while the sentinel is still there, no clear has happened since, so a
    /// missing entry for `config` is the fault of `build_client`. An attempt that sees a
    /// clear starts over.
    #[test]
    fn build_client_stores_its_client_in_the_process_wide_cache() {
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_279),
            ..CrawlConfig::default()
        };
        let sentinel_key = ClientCacheKey::from_config(&CrawlConfig {
            request_timeout: Duration::from_millis(918_280),
            ..CrawlConfig::default()
        });
        let config_key = ClientCacheKey::from_config(&config);
        let sentinel_client = ClientCache::default()
            .get_or_build(&CrawlConfig::default())
            .expect("sentinel client must build");

        for _ in 0..100 {
            client_cache().insert(sentinel_key.clone(), &sentinel_client);
            let _client = build_client(&config).expect("client must build");
            let clients = client_cache().clients.lock().expect("cache lock must not be poisoned");
            if clients.contains_key(&sentinel_key) {
                assert!(
                    clients.contains_key(&config_key),
                    "build_client must store the client it builds in the process-wide cache"
                );
                return;
            }
        }
        panic!("another test cleared the process-wide cache during each of 100 attempts");
    }
}
