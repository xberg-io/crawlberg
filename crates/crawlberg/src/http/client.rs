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

/// Process-wide cache of built `reqwest::Client`s, keyed by [`ClientCacheKey`].
///
/// ~keep `build_client` is called on the hot fetch path (once per tier attempt in
/// `engine/mod.rs::run_tier`), so without this cache every HTTP request pays a fresh
/// TCP/TLS handshake and gets no connection-pool reuse. `reqwest::Client` is
/// `Arc`-backed internally, so cloning a cached entry is cheap, and each distinct
/// proxy/auth/timeout/cookie identity still gets its own client rather than one client
/// silently serving unrelated sessions (see [`ClientCacheKey`]).
fn client_cache() -> &'static Mutex<HashMap<ClientCacheKey, reqwest::Client>> {
    static CACHE: OnceLock<Mutex<HashMap<ClientCacheKey, reqwest::Client>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
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

/// Whether a cached client already exists for `config`'s identity. Test-only
/// introspection for verifying [`build_client`]'s caching behavior.
#[cfg(test)]
pub(crate) fn client_cache_contains(config: &CrawlConfig) -> bool {
    let key = ClientCacheKey::from_config(config);
    client_cache()
        .lock()
        .map(|cache| cache.contains_key(&key))
        .unwrap_or(false)
}

/// Build a `reqwest::Client` with the given configuration (redirect policy, timeout, cookies, proxy).
///
/// Returns a cached, cheaply-cloned client when one matching this configuration's
/// [`ClientCacheKey`] already exists; otherwise builds one and caches it for reuse.
pub(crate) fn build_client(config: &CrawlConfig) -> Result<reqwest::Client, CrawlError> {
    let key = ClientCacheKey::from_config(config);
    if let Ok(cache) = client_cache().lock()
        && let Some(client) = cache.get(&key)
    {
        return Ok(client.clone());
    }

    let client = configure_client(config)?
        .build()
        .map_err(|e| CrawlError::other(format!("Failed to build HTTP client: {e}")))?;

    cache_client(key, &client);

    Ok(client)
}

/// wasm32 has no redirect policy, request timeout, cookie jar, proxy or DNS resolver to
/// configure: the browser's own `fetch` owns every one of them. ~keep
#[cfg(target_arch = "wasm32")]
fn configure_client(_config: &CrawlConfig) -> Result<reqwest::ClientBuilder, CrawlError> {
    Ok(reqwest::Client::builder())
}

/// Apply `config` to a fresh `reqwest::ClientBuilder`.
#[cfg(not(target_arch = "wasm32"))]
fn configure_client(config: &CrawlConfig) -> Result<reqwest::ClientBuilder, CrawlError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(config.request_timeout);

    // ~keep `cookie_provider`, not `cookie_store(true)`: reqwest's default jar loads no
    // public-suffix list, so a host may set Domain= to a shared multi-tenant suffix
    // (herokuapp.com, github.io, a bare TLD). One client is reused across a whole crawl
    // that can span hosts, so that would be a supercookie leak between unrelated tenants.
    if config.cookies_enabled {
        builder = builder.cookie_provider(std::sync::Arc::new(crate::net::cookie::PolicyCookieStore::default()));
    }

    builder = apply_proxy(builder, config)?;

    // ~keep Closes the DNS-rebinding TOCTOU: `validate_url` resolves the host and checks
    // the answers, then hyper resolves it *again* to connect, so the checked addresses are
    // not the connected ones. `PolicyResolver` re-checks inside the resolution hyper
    // actually uses, leaving no second lookup to disagree with the first.
    //
    // Skipped whenever a proxy is configured, because hyper then resolves the *proxy*
    // host rather than the target: the policy would be applied to the wrong name (a proxy
    // on a private address is a normal, previously-working setup), and the target's
    // resolution happens at the proxy, out of this process's reach, so client-side
    // pinning cannot be achieved through a proxy at all. `validate_url`'s own pre-check
    // still runs on the target in that case.
    if config.proxy_provider.is_none() && config.proxy.is_none() {
        builder = builder.dns_resolver(std::sync::Arc::new(crate::net::resolver::PolicyResolver::new(
            config.ssrf.clone(),
        )));
    }

    Ok(builder)
}

/// Attach whichever proxy `config` asks for, if any.
#[cfg(not(target_arch = "wasm32"))]
fn apply_proxy(builder: reqwest::ClientBuilder, config: &CrawlConfig) -> Result<reqwest::ClientBuilder, CrawlError> {
    // ~keep `proxy_provider` takes precedence over static proxy so reqwest can rotate per request.
    if let Some(provider) = config.proxy_provider.clone() {
        return Ok(builder.proxy(rotating_proxy(provider)));
    }

    let Some(ref proxy_config) = config.proxy else {
        return Ok(builder);
    };

    let mut proxy = reqwest::Proxy::all(&proxy_config.url)
        .map_err(|e| CrawlError::invalid_config(format!("invalid proxy URL: {e}")))?;
    if let (Some(user), Some(pass)) = (&proxy_config.username, &proxy_config.password) {
        proxy = proxy.basic_auth(user, pass);
    }
    Ok(builder.proxy(proxy))
}

/// A `reqwest::Proxy` that asks `provider` which proxy to use, per request.
#[cfg(not(target_arch = "wasm32"))]
fn rotating_proxy(provider: std::sync::Arc<dyn crate::ProxyProvider>) -> reqwest::Proxy {
    reqwest::Proxy::custom(move |url| {
        let host = url.host_str().unwrap_or("");
        // ~keep `None` here is the provider deliberately routing this host direct
        // (a no-proxy list), not a failure — so it is not logged.
        let cfg = provider.next_proxy(host)?;

        // ~keep `Proxy::custom` can only answer Some/None: there is no channel to
        // fail the request, and `None` means "connect directly". A malformed proxy
        // URL therefore silently becomes an egress-control bypass — the one outcome
        // an operator most needs to know about — so it is logged at ERROR. Failing
        // closed is not reachable from inside this closure.
        //
        // ~keep The offending URL is deliberately NOT logged: `redact_url_credentials`
        // returns its input unchanged when the input does not parse, which is exactly
        // the case here — so naming it would print any embedded `user:pass@` verbatim.
        let Ok(mut parsed) = reqwest::Url::parse(&cfg.url) else {
            tracing::error!(
                target_host = %host,
                "proxy provider returned an unparseable URL; connecting DIRECTLY, bypassing the proxy"
            );
            return None;
        };

        if let (Some(user), Some(pass)) = (&cfg.username, &cfg.password) {
            // ~keep Deliberately still proxied when the credentials cannot be
            // attached: the proxy answers 407 and the request fails visibly, whereas
            // returning `None` would send the traffic direct and defeat egress
            // control outright. The louder failure is the safer one.
            if parsed.set_username(user).is_err() || parsed.set_password(Some(pass)).is_err() {
                tracing::error!(
                    target_host = %host,
                    proxy_url = %crate::net::redact_url_credentials(&cfg.url),
                    "proxy URL does not accept credentials; connecting through the proxy unauthenticated"
                );
            }
        }
        Some(parsed)
    })
}

/// Store `client` under `key`, clearing the cache wholesale once it is full.
fn cache_client(key: ClientCacheKey, client: &reqwest::Client) {
    let Ok(mut cache) = client_cache().lock() else {
        return;
    };
    if cache.len() >= MAX_CACHED_CLIENTS {
        tracing::debug!(
            cached = cache.len(),
            cap = MAX_CACHED_CLIENTS,
            "HTTP client cache full, clearing"
        );
        cache.clear();
    }
    cache.insert(key, client.clone());
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::net::ssrf::SsrfPolicy;
    use crate::types::ProxyConfig;
    use std::time::Duration;

    #[test]
    fn build_client_reuses_cached_client_for_matching_config() {
        // ~keep A distinct, unlikely-to-collide timeout so this test's cache entry
        // cannot already be populated by another test running in parallel.
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_273),
            ..CrawlConfig::default()
        };
        assert!(
            !client_cache_contains(&config),
            "precondition failed: another test already cached this exact config identity"
        );

        let _first = build_client(&config).expect("first build must succeed");
        assert!(
            client_cache_contains(&config),
            "build_client must populate the cache after building a client"
        );

        let _second = build_client(&config).expect("second build with the same config must succeed");
        assert!(
            client_cache_contains(&config),
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

        let _permissive_client = build_client(&permissive).expect("permissive client must build");
        assert!(
            !client_cache_contains(&restrictive),
            "a client built under deny_private=false must not be served to a deny_private=true \
             config — its resolver carries the permissive policy"
        );

        let _restrictive_client = build_client(&restrictive).expect("restrictive client must build");
        assert!(
            client_cache_contains(&permissive) && client_cache_contains(&restrictive),
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
    /// `should_retry_status` only matches status-derived variants. Measured in a standalone
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
        let config = CrawlConfig {
            request_timeout: Duration::from_millis(918_276),
            ..CrawlConfig::default()
        };
        assert!(
            !client_cache_contains(&config),
            "precondition failed: another test already cached this exact config identity"
        );

        let runtime_a = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime a must build");
        runtime_a.block_on(async {
            let _client = build_client(&config).expect("client must build on runtime a");
            assert!(
                client_cache_contains(&config),
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
                !client_cache_contains(&config),
                "a client cached on a since-dropped runtime must not be reused on a new one: \
                 its pooled connections are driven by tasks that died with that runtime"
            );
        });
    }

    #[test]
    fn build_client_uses_distinct_cache_entries_for_distinct_timeouts() {
        let config_a = CrawlConfig {
            request_timeout: Duration::from_millis(918_274),
            ..CrawlConfig::default()
        };
        let config_b = CrawlConfig {
            request_timeout: Duration::from_millis(918_275),
            ..CrawlConfig::default()
        };

        let _a = build_client(&config_a).expect("client a must build");
        assert!(
            client_cache_contains(&config_a),
            "config_a's identity must be cached after building it"
        );
        assert!(
            !client_cache_contains(&config_b),
            "building a client for config_a must not also cache config_b's distinct identity"
        );
    }
}
