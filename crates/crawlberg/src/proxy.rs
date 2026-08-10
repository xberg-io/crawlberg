//! Proxy provider trait + baseline impl.
//!
//! Substrate-level extension point for per-host proxy rotation. The engine
//! calls [`ProxyProvider::next_proxy`] from `reqwest::Proxy::custom` per HTTP
//! request, so implementations can rotate by host, by counter, or by external
//! state. Returning `None` short-circuits to a direct connection.
//!
//! Crawlberg ships [`StaticProxyProvider`] — a fixed-pool round-robin
//! rotator. Cloud impls (e.g. `BrightDataProxyProvider`) plug in via
//! [`crate::CrawlEngineBuilder::with_proxy_provider`].
//!
//! Browser-backend proxies (`config.browser.proxy`) are still configured at
//! launch time via the static `ProxyConfig` value on `CrawlConfig`; the
//! provider only routes the reqwest HTTP path. Mixing both is supported.
//!
//! ```
//! use std::sync::Arc;
//! use crawlberg::{ProxyConfig, ProxyProvider, StaticProxyProvider};
//!
//! let pool = StaticProxyProvider::new(vec![
//!     ProxyConfig { url: "http://p1:8080".into(), username: None, password: None },
//!     ProxyConfig { url: "http://p2:8080".into(), username: None, password: None },
//! ]);
//! let _arc: Arc<dyn ProxyProvider> = Arc::new(pool);
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::types::ProxyConfig;

/// Resolves a [`ProxyConfig`] for an outbound HTTP request.
///
/// Implementations must be cheap (called per request from inside
/// `reqwest::Proxy::custom`) and thread-safe. Returning `None` routes the
/// request directly without a proxy.
pub trait ProxyProvider: std::fmt::Debug + Send + Sync + 'static {
    /// Pick a proxy for the given target host. `host` is the URL host string
    /// (no scheme, no port) — implementations may key on it for sticky
    /// per-host routing or ignore it for stateless rotation.
    fn next_proxy(&self, host: &str) -> Option<ProxyConfig>;
}

/// Round-robin pool of statically-configured proxies. Baseline impl shipped
/// with crawlberg.
///
/// Threadsafe; uses an [`AtomicUsize`] counter incremented per call. Empty
/// pools always return `None` (direct connection).
pub struct StaticProxyProvider {
    entries: Vec<ProxyConfig>,
    counter: AtomicUsize,
}

impl std::fmt::Debug for StaticProxyProvider {
    /// Redacted: [`ProxyConfig`]'s own derived `Debug` prints `username`/`password` and
    /// any userinfo embedded in `url` verbatim, and `ProxyProvider: std::fmt::Debug`
    /// means any consumer holding a trait object can trigger this via `{:?}` — including
    /// through `tracing`'s `?field` capture. Show only the redacted URL per entry plus
    /// whether credentials are configured, never the credentials themselves.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_urls: Vec<String> = self
            .entries
            .iter()
            .map(|entry| crate::net::redact_url_credentials(&entry.url))
            .collect();
        let has_credentials = self
            .entries
            .iter()
            .any(|entry| entry.username.is_some() || entry.password.is_some());
        f.debug_struct("StaticProxyProvider")
            .field("entries", &redacted_urls)
            .field("has_credentials", &has_credentials)
            .field("counter", &self.counter.load(Ordering::Relaxed))
            .finish()
    }
}

impl StaticProxyProvider {
    /// Build a provider with the given pool. Order is preserved; rotation
    /// is round-robin starting from index 0.
    pub fn new(entries: Vec<ProxyConfig>) -> Self {
        Self {
            entries,
            counter: AtomicUsize::new(0),
        }
    }

    /// Build an empty provider — always returns `None`. Useful as a default
    /// placeholder in substrate-only setups.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Number of proxies in the pool.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no proxies are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl ProxyProvider for StaticProxyProvider {
    fn next_proxy(&self, _host: &str) -> Option<ProxyConfig> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % self.entries.len();
        Some(self.entries[idx].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(url: &str) -> ProxyConfig {
        ProxyConfig {
            url: url.into(),
            username: None,
            password: None,
        }
    }

    #[test]
    fn empty_provider_returns_none() {
        let provider = StaticProxyProvider::empty();
        assert!(provider.next_proxy("example.com").is_none());
        assert!(provider.is_empty());
        assert_eq!(provider.len(), 0);
    }

    #[test]
    fn debug_format_never_exposes_proxy_credentials() {
        // ~keep `ProxyProvider: std::fmt::Debug` means any consumer holding a trait
        // object (`tracing::debug!(?provider, ...)`, a Debug-derived struct that embeds
        // one) can print this. The derived Debug would show `password: Some("hunter2")`
        // and the credential-bearing URL verbatim.
        let provider = StaticProxyProvider::new(vec![ProxyConfig {
            url: "http://svc-account:hunter2@proxy.internal:8080".into(),
            username: Some("svc-account".into()),
            password: Some("hunter2".into()),
        }]);
        let rendered = format!("{provider:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug output must not contain the raw password, got '{rendered}'"
        );
        assert!(
            !rendered.contains("svc-account:hunter2"),
            "Debug output must not contain raw proxy URL userinfo, got '{rendered}'"
        );
    }

    #[test]
    fn round_robin_cycles_through_pool() {
        let provider = StaticProxyProvider::new(vec![
            proxy("http://p1:8080"),
            proxy("http://p2:8080"),
            proxy("http://p3:8080"),
        ]);
        let urls: Vec<_> = (0..6)
            .map(|_| provider.next_proxy("example.com").unwrap().url)
            .collect();
        assert_eq!(
            urls,
            vec![
                "http://p1:8080",
                "http://p2:8080",
                "http://p3:8080",
                "http://p1:8080",
                "http://p2:8080",
                "http://p3:8080",
            ]
        );
    }
}
