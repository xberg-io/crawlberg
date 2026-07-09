//! Disk-backed HTTP response cache using blake3 for key hashing.
#![allow(dead_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::error::CrawlError;
use crate::traits::CrawlCache;
use crate::types::CachedPage;

/// Filesystem-backed response cache.
///
/// Stores cached pages as JSON files in a configurable directory,
/// using blake3 hashes of URLs as filenames. Supports TTL-based
/// expiry and maximum entry count with LRU eviction.
///
/// Not available on `wasm32` targets (no filesystem access).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct DiskCache {
    cache_dir: PathBuf,
    ttl_secs: u64,
    max_entries: usize,
}

/// No-op cache that never stores or returns anything.
#[derive(Debug, Clone, Default)]
pub struct NoopCache;

#[cfg(not(target_arch = "wasm32"))]
impl DiskCache {
    /// Create a new disk cache.
    ///
    /// - `cache_dir`: Directory to store cached files. Created if not exists.
    /// - `ttl_secs`: Time-to-live for cached entries (0 = no expiry).
    /// - `max_entries`: Maximum number of cached entries (0 = unlimited).
    pub fn new(cache_dir: impl AsRef<Path>, ttl_secs: u64, max_entries: usize) -> Result<Self, CrawlError> {
        let cache_dir = cache_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| CrawlError::Other(format!("failed to create cache directory: {e}")))?;
        Ok(Self {
            cache_dir,
            ttl_secs,
            max_entries,
        })
    }

    /// Create a disk cache in the default location (`.crawlberg/cache/`).
    pub fn default_location() -> Result<Self, CrawlError> {
        let dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".crawlberg")
            .join("cache");
        Self::new(dir, 3600, 10000)
    }

    fn cache_key(url: &str) -> String {
        let hash = blake3::hash(url.as_bytes());
        hash.to_hex().to_string()
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        let hash = Self::cache_key(key);
        self.cache_dir.join(format!("{hash}.json"))
    }
}

#[cfg(test)]
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl CrawlCache for DiskCache {
    async fn get(&self, key: &str) -> Result<Option<CachedPage>, CrawlError> {
        let path = self.cache_path(key);
        let ttl_secs = self.ttl_secs;

        tokio::task::spawn_blocking(move || {
            if !path.exists() {
                return Ok(None);
            }
            let data =
                std::fs::read_to_string(&path).map_err(|e| CrawlError::Other(format!("cache read error: {e}")))?;
            let page: CachedPage = match serde_json::from_str(&data) {
                Ok(p) => p,
                Err(_) => {
                    let _ = std::fs::remove_file(&path);
                    return Ok(None);
                }
            };

            if ttl_secs > 0 {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if now.saturating_sub(page.cached_at) > ttl_secs {
                    let _ = std::fs::remove_file(&path);
                    return Ok(None);
                }
            }

            Ok(Some(page))
        })
        .await
        .unwrap_or(Ok(None))
    }

    async fn set(&self, key: &str, page: &CachedPage) -> Result<(), CrawlError> {
        let path = self.cache_path(key);
        let data = serde_json::to_string(page).map_err(|e| CrawlError::Other(format!("cache serialize error: {e}")))?;

        let max_entries = self.max_entries;
        let cache_dir = self.cache_dir.clone();

        tokio::task::spawn_blocking(move || {
            if max_entries > 0 {
                let entries: Vec<_> = match std::fs::read_dir(&cache_dir) {
                    Ok(dir) => dir
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                        .collect(),
                    Err(_) => return Ok(()),
                };

                if entries.len() >= max_entries {
                    let mut with_times: Vec<_> = entries
                        .into_iter()
                        .filter_map(|e| {
                            let modified = e.metadata().ok()?.modified().ok()?;
                            Some((e.path(), modified))
                        })
                        .collect();
                    with_times.sort_by_key(|(_, t)| *t);

                    let to_remove = with_times.len().saturating_sub(max_entries - 1);
                    for (path, _) in with_times.into_iter().take(to_remove) {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }

            let tmp_path = path.with_extension("tmp");
            std::fs::write(&tmp_path, data).map_err(|e| CrawlError::Other(format!("cache write error: {e}")))?;
            std::fs::rename(&tmp_path, &path).map_err(|e| CrawlError::Other(format!("cache rename error: {e}")))
        })
        .await
        .unwrap_or(Ok(()))
    }

    async fn has(&self, key: &str) -> Result<bool, CrawlError> {
        Ok(self.get(key).await?.is_some())
    }
}

#[async_trait]
impl CrawlCache for NoopCache {
    async fn get(&self, _key: &str) -> Result<Option<CachedPage>, CrawlError> {
        Ok(None)
    }
    async fn set(&self, _key: &str, _page: &CachedPage) -> Result<(), CrawlError> {
        Ok(())
    }
    async fn has(&self, _key: &str) -> Result<bool, CrawlError> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_page(url: &str) -> CachedPage {
        CachedPage {
            url: url.to_owned(),
            status_code: 200,
            content_type: "text/html".to_owned(),
            body: "<html>test</html>".to_owned(),
            etag: Some("\"abc\"".to_owned()),
            last_modified: None,
            cached_at: now_secs(),
        }
    }

    #[tokio::test]
    async fn test_disk_cache_set_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 0).unwrap();

        let page = make_page("http://example.com/page1");
        cache.set("http://example.com/page1", &page).await.unwrap();

        let retrieved = cache
            .get("http://example.com/page1")
            .await
            .unwrap()
            .expect("page should be cached");
        assert_eq!(retrieved.url, "http://example.com/page1");
        assert_eq!(retrieved.status_code, 200);
        assert_eq!(retrieved.body, "<html>test</html>");
        assert_eq!(retrieved.etag, Some("\"abc\"".to_owned()));
    }

    #[tokio::test]
    async fn test_disk_cache_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 0).unwrap();

        let result = cache.get("http://example.com/missing").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_disk_cache_has() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 0).unwrap();

        assert!(!cache.has("http://example.com/page").await.unwrap());

        let page = make_page("http://example.com/page");
        cache.set("http://example.com/page", &page).await.unwrap();

        assert!(cache.has("http://example.com/page").await.unwrap());
    }

    #[tokio::test]
    async fn test_disk_cache_ttl_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 60, 0).unwrap();

        let mut page = make_page("http://example.com/old");
        page.cached_at = now_secs() - 120;

        cache.set("http://example.com/old", &page).await.unwrap();

        let result = cache.get("http://example.com/old").await.unwrap();
        assert!(result.is_none(), "expired entry should return None");
    }

    #[tokio::test]
    async fn test_disk_cache_no_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 0, 0).unwrap();

        let mut page = make_page("http://example.com/forever");
        page.cached_at = 0;

        cache.set("http://example.com/forever", &page).await.unwrap();

        let result = cache.get("http://example.com/forever").await.unwrap();
        assert!(result.is_some(), "ttl=0 should never expire");
    }

    #[tokio::test]
    async fn test_disk_cache_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 2).unwrap();

        for i in 0..3 {
            let url = format!("http://example.com/{i}");
            let page = make_page(&url);
            cache.set(&url, &page).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert!(count <= 2, "should have at most 2 entries, got {count}");
    }

    #[tokio::test]
    async fn test_disk_cache_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 0).unwrap();

        let page1 = make_page("http://example.com/page");
        cache.set("http://example.com/page", &page1).await.unwrap();

        let mut page2 = make_page("http://example.com/page");
        page2.body = "updated body".to_owned();
        cache.set("http://example.com/page", &page2).await.unwrap();

        let retrieved = cache.get("http://example.com/page").await.unwrap().unwrap();
        assert_eq!(retrieved.body, "updated body");
    }

    #[tokio::test]
    async fn test_disk_cache_different_urls_different_keys() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 0).unwrap();

        let page_a = make_page("http://a.com");
        let page_b = make_page("http://b.com");
        cache.set("http://a.com", &page_a).await.unwrap();
        cache.set("http://b.com", &page_b).await.unwrap();

        assert!(cache.has("http://a.com").await.unwrap());
        assert!(cache.has("http://b.com").await.unwrap());
    }

    #[tokio::test]
    async fn test_disk_cache_key_is_deterministic() {
        let key1 = DiskCache::cache_key("http://example.com/page");
        let key2 = DiskCache::cache_key("http://example.com/page");
        assert_eq!(key1, key2);

        let key3 = DiskCache::cache_key("http://example.com/other");
        assert_ne!(key1, key3);
    }

    #[tokio::test]
    async fn test_disk_cache_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("cache");
        assert!(!nested.exists());

        let cache = DiskCache::new(&nested, 3600, 0).unwrap();
        assert!(nested.exists());

        let page = make_page("http://example.com");
        cache.set("http://example.com", &page).await.unwrap();
        assert!(cache.has("http://example.com").await.unwrap());
    }

    #[tokio::test]
    async fn test_noop_cache_always_misses() {
        let cache = NoopCache;
        assert!(cache.get("any-key").await.unwrap().is_none());
        assert!(!cache.has("any-key").await.unwrap());
        let page = make_page("http://example.com");
        cache.set("http://example.com", &page).await.unwrap();
        assert!(cache.get("http://example.com").await.unwrap().is_none());
    }
}
