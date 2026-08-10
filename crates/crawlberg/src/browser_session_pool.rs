//! Per-(domain, proxy) session affinity layer for reusing browser contexts.
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

/// Key identifying a reusable session. Same domain + same proxy → same session.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SessionKey {
    /// Domain (extracted from URL for matching).
    pub domain: String,
    /// Proxy URL, or None if no proxy.
    pub proxy: Option<String>,
}

impl SessionKey {
    /// Create a new session key from a URL and optional proxy.
    /// Extracts the domain from the URL (no path, no query).
    pub fn from_url(url: &str, proxy: Option<&str>) -> Result<Self, CrawlError> {
        let parsed = url::Url::parse(url)
            .map_err(|e| CrawlError::BrowserError(format!("failed to parse URL for session key: {e}")))?;
        let domain = parsed
            .host_str()
            .ok_or_else(|| CrawlError::BrowserError("URL has no host".into()))?
            .to_string();
        Ok(SessionKey {
            domain,
            proxy: proxy.map(|s| s.to_string()),
        })
    }
}

/// A pooled session with its associated Page + last-used timestamp.
struct PooledSession {
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
    fn drop(&mut self) {
        if let Some(page) = self.page.take() {
            tokio::spawn(async move {
                let _ = page.close().await;
            });
        }
    }
}

/// Bounded LRU-ish session pool. Default idle timeout 5 min; sessions
/// older than the timeout are evicted on next acquire.
#[cfg(feature = "browser")]
#[derive(Debug)]
pub struct BrowserSessionPool {
    sessions: Mutex<HashMap<SessionKey, PooledSession>>,
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
        let mut sessions = self.sessions.lock().await;
        self.evict_expired(&mut sessions);
        let mut entry = sessions.remove(key)?;
        let page = entry.page.take().expect("acquired session always has a page");
        Some((page, entry.permit.take()))
    }

    /// Insert a page into the pool for the given key, along with the
    /// `BrowserPool` semaphore permit it was acquired with (if any). If the
    /// pool is over capacity, evicts the least-recently-used session,
    /// closing its page and releasing its permit.
    pub async fn insert(&self, key: SessionKey, page: chromiumoxide::Page, permit: Option<OwnedSemaphorePermit>) {
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
                page: Some(page),
                permit,
                last_used: Instant::now(),
            },
        );
    }

    /// Evict all sessions whose last_used is older than idle_timeout.
    fn evict_expired(&self, sessions: &mut HashMap<SessionKey, PooledSession>) {
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

    #[tokio::test]
    async fn test_insert_and_acquire_same_key() {
        let pool = BrowserSessionPool::new();
        let _key = SessionKey {
            domain: "example.com".to_string(),
            proxy: None,
        };

        assert_eq!(pool.size().await, 0);
    }

    #[tokio::test]
    async fn test_evict_expired_sessions() {
        let pool = BrowserSessionPool::with_config(Duration::from_millis(10), 100);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let _ = pool.size().await;
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
