//! Per-(domain, proxy, SSRF policy) session affinity layer for reusing browser contexts.
//!
//! Reuses an existing chromiumoxide Page for follow-up requests against the same
//! origin so cookies + fingerprint + any solved challenge persist within the idle
//! window. Improves Cloudflare / DataDome pass-through rate when the WAF issues
//! a one-time challenge on first request and trusts the session afterward.
//!
//! This module pools chromiumoxide Pages (not BrowserContext) because:
//! - BrowserContext lives in crawlberg-browser (separate crate).
//! - chromiumoxide::Page is what `page_fetch` consumes directly.
//! - Pages naturally carry their own cookie state via CDP.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedSemaphorePermit};

use crate::error::CrawlError;
use crate::net::ssrf::{HostMatcher, SsrfPolicy};

/// Key identifying a reusable session. Same domain + same proxy → same session.
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct SessionKey {
    /// Domain (extracted from URL for matching).
    pub domain: String,
    /// Proxy URL, or None if no proxy.
    pub proxy: Option<String>,
}

impl std::fmt::Debug for SessionKey {
    /// Redacted: the proxy URL can carry `user:pass@` credentials or a token in its path or
    /// query, so only its origin prints, as in `ProxyConfig`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { domain, proxy } = self;
        f.debug_struct("SessionKey")
            .field("domain", domain)
            .field("proxy", &proxy.as_deref().map(crate::net::redact::redact_url_to_origin))
            .finish()
    }
}

impl SessionKey {
    /// Create a new session key from a URL and optional proxy.
    /// Extracts the domain from the URL (no path, no query).
    pub fn from_url(url: &str, proxy: Option<&str>) -> Result<Self, CrawlError> {
        let parsed = url::Url::parse(url)
            .map_err(|e| CrawlError::browser_error(format!("failed to parse URL for session key: {e}")))?;
        let domain = parsed
            .host_str()
            .ok_or_else(|| CrawlError::browser_error("URL has no host"))?
            .to_string();
        Ok(SessionKey {
            domain,
            proxy: proxy.map(|s| s.to_string()),
        })
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum HostMatcherIdentity {
    Exact(String),
    Suffix(String),
    Cidr(String),
}

impl From<&HostMatcher> for HostMatcherIdentity {
    fn from(matcher: &HostMatcher) -> Self {
        match matcher {
            HostMatcher::Exact { value } => Self::Exact(value.clone()),
            HostMatcher::Suffix { value } => Self::Suffix(value.clone()),
            HostMatcher::Cidr { value } => Self::Cidr(value.clone()),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct SsrfPolicyIdentity {
    deny_private: bool,
    allowlist: Vec<HostMatcherIdentity>,
    denylist: Vec<HostMatcherIdentity>,
    max_redirects: u8,
    scheme_allowlist: Vec<String>,
}

impl From<&SsrfPolicy> for SsrfPolicyIdentity {
    fn from(policy: &SsrfPolicy) -> Self {
        Self {
            deny_private: policy.deny_private,
            allowlist: policy.allowlist.iter().map(HostMatcherIdentity::from).collect(),
            denylist: policy.denylist.iter().map(HostMatcherIdentity::from).collect(),
            max_redirects: policy.max_redirects,
            scheme_allowlist: policy.scheme_allowlist.clone(),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum PolicyNamespace {
    Public,
    Protected(SsrfPolicyIdentity),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct PoolEntryKey {
    session: SessionKey,
    policy: PolicyNamespace,
}

impl PoolEntryKey {
    fn public(session: SessionKey) -> Self {
        Self {
            session,
            policy: PolicyNamespace::Public,
        }
    }

    fn with_policy(session: SessionKey, policy: &SsrfPolicy) -> Self {
        Self {
            session,
            policy: PolicyNamespace::Protected(policy.into()),
        }
    }
}

/// A pooled session with its associated Page + last-used timestamp.
struct PooledSession {
    handler_end: Option<crate::browser_pool::HandlerEnd>,
    /// The chromiumoxide Page from the browser pool. This is what carries
    /// cookies, fingerprint, and any solved challenge state across requests.
    page: Option<chromiumoxide::Page>,
    /// The `BrowserPool` semaphore permit this page was acquired with. Held
    /// here (rather than released back to the pool) so a page parked for
    /// reuse still counts against `max_pages` — it still occupies a real
    /// Chrome tab. `None` for pages that did not come from a `BrowserPool`
    /// (e.g. tests constructing a page directly).
    permit: Option<OwnedSemaphorePermit>,
    /// Last time this session was used (for idle eviction).
    last_used: Instant,
}

impl std::fmt::Debug for PooledSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledSession")
            .field("last_used", &self.last_used)
            .finish()
    }
}

impl Drop for PooledSession {
    // ~keep Without this, evicted/replaced sessions leaked their Chrome tab: chromiumoxide::Page
    // ~keep is a cheap Arc handle with no Drop of its own, so letting it fall out of the map
    // ~keep silently abandoned the CDP target instead of closing it.
    // ~keep `tokio::spawn` panics with no active runtime, and these sessions can be dropped
    // ~keep from an FFI teardown or GC-finalizer thread; guard rather than abort the host.
    fn drop(&mut self) {
        if let Some(page) = self.page.take() {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let _ = page.close().await;
                    });
                }
                Err(_) => {
                    tracing::warn!(
                        "dropping a pooled session outside a Tokio runtime; its CDP target is left to Chrome"
                    );
                }
            }
        }
    }
}

/// Bounded LRU-ish session pool. Default idle timeout 5 min; sessions
/// older than the timeout are evicted on next acquire.
#[cfg(feature = "browser")]
#[derive(Debug)]
pub struct BrowserSessionPool {
    sessions: Mutex<HashMap<PoolEntryKey, PooledSession>>,
    idle_timeout: Duration,
    max_sessions: usize,
}

#[cfg(feature = "browser")]
impl BrowserSessionPool {
    /// Create a new session pool with a default idle timeout of 5 minutes
    /// and a max of 100 sessions.
    pub fn new() -> Self {
        Self::with_config(Duration::from_secs(300), 100)
    }

    /// Create a new session pool with custom idle timeout and max sessions.
    pub fn with_config(idle_timeout: Duration, max_sessions: usize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            idle_timeout,
            max_sessions,
        }
    }

    /// Look up an existing session for the key, refreshing its last_used.
    /// Evicts expired entries opportunistically. Returns `None` if the
    /// session was not found or was expired.
    ///
    /// Returns the page together with the `BrowserPool` semaphore permit it
    /// was inserted with (if any), so the caller keeps holding the same
    /// concurrency slot across reuse instead of re-acquiring a fresh one.
    pub async fn acquire(&self, key: &SessionKey) -> Option<(chromiumoxide::Page, Option<OwnedSemaphorePermit>)> {
        self.acquire_entry(PoolEntryKey::public(key.clone()))
            .await
            .map(|(page, permit, _)| (page, permit))
    }

    pub(crate) async fn acquire_with_policy(
        &self,
        key: &SessionKey,
        policy: &SsrfPolicy,
    ) -> Option<(
        chromiumoxide::Page,
        Option<OwnedSemaphorePermit>,
        Option<crate::browser_pool::HandlerEnd>,
    )> {
        self.acquire_entry(PoolEntryKey::with_policy(key.clone(), policy)).await
    }

    async fn acquire_entry(
        &self,
        key: PoolEntryKey,
    ) -> Option<(
        chromiumoxide::Page,
        Option<OwnedSemaphorePermit>,
        Option<crate::browser_pool::HandlerEnd>,
    )> {
        let mut sessions = self.sessions.lock().await;
        self.evict_expired(&mut sessions);
        let mut entry = sessions.remove(&key)?;
        let page = entry.page.take().expect("acquired session always has a page");
        Some((page, entry.permit.take(), entry.handler_end.take()))
    }

    /// Insert a page into the pool for the given key, along with the
    /// `BrowserPool` semaphore permit it was acquired with (if any). If the
    /// pool is over capacity, evicts the least-recently-used session,
    /// closing its page and releasing its permit.
    pub async fn insert(&self, key: SessionKey, page: chromiumoxide::Page, permit: Option<OwnedSemaphorePermit>) {
        self.insert_entry(PoolEntryKey::public(key), page, permit, None).await;
    }

    pub(crate) async fn insert_with_policy(
        &self,
        key: SessionKey,
        policy: &SsrfPolicy,
        page: chromiumoxide::Page,
        permit: Option<OwnedSemaphorePermit>,
        handler_end: Option<crate::browser_pool::HandlerEnd>,
    ) {
        self.insert_entry(PoolEntryKey::with_policy(key, policy), page, permit, handler_end)
            .await;
    }

    async fn insert_entry(
        &self,
        key: PoolEntryKey,
        page: chromiumoxide::Page,
        permit: Option<OwnedSemaphorePermit>,
        handler_end: Option<crate::browser_pool::HandlerEnd>,
    ) {
        let mut sessions = self.sessions.lock().await;
        self.evict_expired(&mut sessions);

        if sessions.len() >= self.max_sessions
            && let Some((k, _)) = sessions
                .iter()
                .min_by_key(|(_, v)| v.last_used)
                .map(|(k, v)| (k.clone(), v.last_used))
        {
            sessions.remove(&k);
        }

        sessions.insert(
            key,
            PooledSession {
                handler_end,
                page: Some(page),
                permit,
                last_used: Instant::now(),
            },
        );
    }

    /// Evict all sessions whose last_used is older than idle_timeout.
    fn evict_expired(&self, sessions: &mut HashMap<PoolEntryKey, PooledSession>) {
        let now = Instant::now();
        sessions.retain(|_, v| now.duration_since(v.last_used) < self.idle_timeout);
    }

    /// Return the number of active sessions in the pool.
    pub async fn size(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Shut down the pool and close all pages. This is best-effort; failures
    /// in closing individual pages are silently ignored.
    pub async fn shutdown(&self) {
        let mut sessions = self.sessions.lock().await;
        // ~keep Dropping each entry runs `PooledSession::drop`, which closes the page and
        // ~keep (via the permit field) releases the BrowserPool concurrency slot it held.
        sessions.clear();
    }
}

#[cfg(feature = "browser")]
impl Default for BrowserSessionPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "browser"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_acquire_returns_none_when_empty() {
        let pool = BrowserSessionPool::new();
        let key = SessionKey {
            domain: "example.com".to_string(),
            proxy: None,
        };
        assert!(pool.acquire(&key).await.is_none());
    }

    /// ~keep Named `test_insert_and_acquire_same_key` until 2026-08-18, but it inserted nothing:
    /// `insert` needs a real `chromiumoxide::Page`, so that pairing cannot be exercised without a
    /// browser. Renamed to what it actually asserts rather than left claiming coverage it lacked.
    #[tokio::test]
    async fn should_start_with_an_empty_pool() {
        let pool = BrowserSessionPool::new();
        assert_eq!(pool.size().await, 0);
    }

    /// ~keep `PooledSession.page` is `Option`, and `evict_expired` reads only `last_used`, so the
    /// eviction path is reachable without a live browser. The previous version of this test slept
    /// past the idle timeout on an *empty* pool and discarded `size()`, so it passed whether or not
    /// eviction worked at all.
    fn parked_session(age: Duration) -> PooledSession {
        PooledSession {
            handler_end: None,
            page: None,
            permit: None,
            last_used: Instant::now().checked_sub(age).expect("test clock underflow"),
        }
    }

    fn key(domain: &str) -> SessionKey {
        SessionKey {
            domain: domain.to_string(),
            proxy: None,
        }
    }

    fn public_key(domain: &str) -> PoolEntryKey {
        PoolEntryKey::public(key(domain))
    }

    #[tokio::test]
    async fn should_evict_only_the_sessions_older_than_the_idle_timeout() {
        let pool = BrowserSessionPool::with_config(Duration::from_secs(30), 100);
        {
            let mut sessions = pool.sessions.lock().await;
            sessions.insert(public_key("stale.example"), parked_session(Duration::from_secs(60)));
            sessions.insert(public_key("fresh.example"), parked_session(Duration::from_secs(1)));
            pool.evict_expired(&mut sessions);
        }

        assert_eq!(pool.size().await, 1, "exactly one session should survive eviction");
        let sessions = pool.sessions.lock().await;
        assert!(
            !sessions.contains_key(&public_key("stale.example")),
            "the expired session must be evicted"
        );
        assert!(
            sessions.contains_key(&public_key("fresh.example")),
            "the live session must be kept"
        );
    }

    #[tokio::test]
    async fn should_return_none_when_acquiring_a_session_past_its_idle_timeout() {
        let pool = BrowserSessionPool::with_config(Duration::from_secs(30), 100);
        pool.sessions
            .lock()
            .await
            .insert(public_key("stale.example"), parked_session(Duration::from_secs(60)));

        assert!(
            pool.acquire(&key("stale.example")).await.is_none(),
            "an expired session must not be handed back to a caller"
        );
        assert_eq!(pool.size().await, 0, "acquiring must also drop the expired entry");
    }

    #[test]
    fn test_session_key_from_url() {
        let key = SessionKey::from_url("https://example.com/path?query=1", None).unwrap();
        assert_eq!(key.domain, "example.com");
        assert_eq!(key.proxy, None);
    }

    #[test]
    fn test_session_key_from_url_with_proxy() {
        let key = SessionKey::from_url("https://example.com/path", Some("http://proxy:8080")).unwrap();
        assert_eq!(key.domain, "example.com");
        assert_eq!(key.proxy, Some("http://proxy:8080".to_string()));
    }

    #[test]
    fn should_not_reuse_a_session_across_different_ssrf_policies() {
        let strict = crate::net::ssrf::SsrfPolicy::default();
        let mut permissive = strict.clone();
        permissive.deny_private = false;

        let session = SessionKey::from_url("https://example.com/path", None).unwrap();
        let strict_key = PoolEntryKey::with_policy(session.clone(), &strict);
        let permissive_key = PoolEntryKey::with_policy(session, &permissive);

        assert_ne!(strict_key, permissive_key);
    }

    #[test]
    fn should_include_every_effective_ssrf_policy_field_in_the_session_key() {
        let baseline = crate::net::ssrf::SsrfPolicy::default();
        let session = SessionKey::from_url("https://example.com/path", None).unwrap();
        let baseline_key = PoolEntryKey::with_policy(session.clone(), &baseline);

        let mut policies = Vec::new();

        let mut allowlist = baseline.clone();
        allowlist
            .allowlist
            .push(crate::net::ssrf::HostMatcher::exact("internal.example"));
        policies.push(allowlist);

        let mut denylist = baseline.clone();
        denylist
            .denylist
            .push(crate::net::ssrf::HostMatcher::cidr("203.0.113.0/24").expect("literal CIDR is valid"));
        policies.push(denylist);

        let mut redirects = baseline.clone();
        redirects.max_redirects += 1;
        policies.push(redirects);

        let mut schemes = baseline.clone();
        schemes.scheme_allowlist = vec!["https".to_owned()];
        policies.push(schemes);

        for policy in policies {
            let key = PoolEntryKey::with_policy(session.clone(), &policy);
            assert_ne!(baseline_key, key);
        }
    }

    #[test]
    fn should_isolate_the_source_compatible_public_pool_namespace_from_policy_aware_entries() {
        let session = SessionKey::from_url("https://example.com/path", None).unwrap();
        let public_key = PoolEntryKey::public(session.clone());
        let protected_key = PoolEntryKey::with_policy(session, &crate::net::ssrf::SsrfPolicy::default());

        assert_ne!(public_key, protected_key);
    }

    #[test]
    fn test_session_key_equality() {
        let key1 = SessionKey {
            domain: "example.com".to_string(),
            proxy: None,
        };
        let key2 = SessionKey {
            domain: "example.com".to_string(),
            proxy: None,
        };
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_session_key_different_domains() {
        let key1 = SessionKey {
            domain: "example.com".to_string(),
            proxy: None,
        };
        let key2 = SessionKey {
            domain: "other.com".to_string(),
            proxy: None,
        };
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_session_key_different_proxies() {
        let key1 = SessionKey {
            domain: "example.com".to_string(),
            proxy: Some("http://proxy1:8080".to_string()),
        };
        let key2 = SessionKey {
            domain: "example.com".to_string(),
            proxy: Some("http://proxy2:8080".to_string()),
        };
        assert_ne!(key1, key2);
    }
}
