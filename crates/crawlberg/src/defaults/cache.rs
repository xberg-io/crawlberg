//! Disk-backed HTTP response cache using blake3 for key hashing.
#![allow(dead_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::error::CrawlError;
use crate::traits::CrawlCache;
use crate::types::CachedPage;

/// Maximum serialized size of a single cached entry (10 MiB).
///
/// Response bodies are attacker/origin-controlled; without a cap a single
/// oversized response could fill the cache disk.
#[cfg(not(target_arch = "wasm32"))]
const MAX_ENTRY_SIZE_BYTES: usize = 10 * 1024 * 1024;

/// Monotonic counter mixed into temp-file names so concurrent writers for the
/// same key never collide, even within the same nanosecond.
#[cfg(not(target_arch = "wasm32"))]
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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

/// Build an unpredictable temp-file path alongside `path`, so an attacker who
/// knows a cache key (and thus the deterministic final path) cannot pre-create
/// a symlink at the name this write will actually use.
#[cfg(not(target_arch = "wasm32"))]
fn unique_temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("cache.json");
    path.with_file_name(format!("{file_name}.{}.{nanos}.{counter}.tmp", std::process::id()))
}

/// Write `data` to `final_path` via `tmp_path`, without ever following a
/// symlink that may already occupy either path.
///
/// - `create_new(true)` (`O_CREAT | O_EXCL`) makes the temp-file open fail
///   instead of following a pre-existing symlink at `tmp_path`.
/// - On unix the file is created with mode `0600` directly (no separate
///   `chmod` window), since cached bodies can carry session cookies or
///   authenticated content.
/// - `fs::rename` replaces whatever directory entry sits at `final_path`
///   (including a symlink) without dereferencing it, so the final publish
///   step is symlink-safe even if `final_path` itself was pre-planted.
#[cfg(not(target_arch = "wasm32"))]
fn write_cache_entry_to(tmp_path: &Path, final_path: &Path, data: &str) -> Result<(), CrawlError> {
    use std::io::Write as _;

    let mut open_options = std::fs::OpenOptions::new();
    open_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open_options.mode(0o600);
    }

    let mut file = open_options
        .open(tmp_path)
        .map_err(|e| CrawlError::Other(format!("cache temp file create error: {e}")))?;
    file.write_all(data.as_bytes())
        .map_err(|e| CrawlError::Other(format!("cache write error: {e}")))?;
    drop(file);

    std::fs::rename(tmp_path, final_path).map_err(|e| {
        let _ = std::fs::remove_file(tmp_path);
        CrawlError::Other(format!("cache rename error: {e}"))
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn write_cache_entry(path: &Path, data: &str) -> Result<(), CrawlError> {
    let tmp_path = unique_temp_path(path);
    write_cache_entry_to(&tmp_path, path, data)
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
            // ~keep No `exists()` pre-check: a concurrent eviction or TTL sweep unlinking the
            // ~keep entry between the two syscalls is an ordinary miss, and the pre-check turned
            // ~keep that race into a propagated error.
            let data = match std::fs::read_to_string(&path) {
                Ok(data) => data,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(CrawlError::Other(format!("cache read error: {e}"))),
            };
            let page: CachedPage = match serde_json::from_str(&data) {
                Ok(p) => p,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "discarding unparseable disk cache entry"
                    );
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
                    // ~keep Deliberately not unlinked. An entry past its TTL is exactly the
                    // input `get_stale` needs for a conditional request: its ETag can still
                    // earn a 304, which costs no body. Deleting it here would have made
                    // revalidation impossible, since `get` runs first. Expired entries are
                    // reclaimed by the `max_entries` sweep in `set`.
                    return Ok(None);
                }
            }

            Ok(Some(page))
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "disk cache read task failed; treating as a miss");
            Ok(None)
        })
    }

    /// Read an entry ignoring the TTL, so an expired one can still be revalidated.
    async fn get_stale(&self, key: &str) -> Result<Option<CachedPage>, CrawlError> {
        let path = self.cache_path(key);

        tokio::task::spawn_blocking(move || {
            let data = match std::fs::read_to_string(&path) {
                Ok(data) => data,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(CrawlError::Other(format!("cache read error: {e}"))),
            };
            match serde_json::from_str(&data) {
                Ok(page) => Ok(Some(page)),
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "discarding unparseable disk cache entry"
                    );
                    let _ = std::fs::remove_file(&path);
                    Ok(None)
                }
            }
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "disk cache stale read task failed; treating as a miss");
            Ok(None)
        })
    }

    async fn set(&self, key: &str, page: &CachedPage) -> Result<(), CrawlError> {
        let path = self.cache_path(key);
        let data = serde_json::to_string(page).map_err(|e| CrawlError::Other(format!("cache serialize error: {e}")))?;

        if data.len() > MAX_ENTRY_SIZE_BYTES {
            tracing::warn!(
                url = %page.url,
                entry_size_bytes = data.len(),
                max_entry_size_bytes = MAX_ENTRY_SIZE_BYTES,
                "skipping disk cache write: entry exceeds size limit"
            );
            return Ok(());
        }

        let max_entries = self.max_entries;
        let cache_dir = self.cache_dir.clone();

        tokio::task::spawn_blocking(move || {
            if max_entries > 0 {
                // ~keep A failed eviction scan must not skip the write: returning Ok(()) here
                // ~keep reported a successful cache write to every caller while storing nothing,
                // ~keep permanently and silently, for as long as the directory stayed unreadable.
                let entries: Vec<_> = match std::fs::read_dir(&cache_dir) {
                    Ok(dir) => dir
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                        .collect(),
                    Err(error) => {
                        tracing::warn!(
                            cache_dir = %cache_dir.display(),
                            %error,
                            "cache eviction scan failed; writing entry without evicting"
                        );
                        Vec::new()
                    }
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

            write_cache_entry(&path, &data)
        })
        .await
        .unwrap_or_else(|error| Err(CrawlError::Other(format!("cache write task failed: {error}"))))
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
            max_age_secs: None,
            must_revalidate: false,
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
    async fn test_disk_cache_rejects_oversized_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 0).unwrap();

        let mut page = make_page("http://example.com/huge");
        page.body = "x".repeat(MAX_ENTRY_SIZE_BYTES + 1);

        cache
            .set("http://example.com/huge", &page)
            .await
            .expect("oversized entry should be skipped, not error");

        let result = cache.get("http://example.com/huge").await.unwrap();
        assert!(result.is_none(), "oversized entry must not be written to disk");

        let count = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(
            count, 0,
            "no cache file should exist after an oversized write, found {count}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_disk_cache_file_has_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 0).unwrap();

        let page = make_page("http://example.com/secret");
        cache.set("http://example.com/secret", &page).await.unwrap();

        let path = cache.cache_path("http://example.com/secret");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "cache file must be owner-only readable/writable, got {mode:o}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_disk_cache_write_does_not_follow_preexisting_tmp_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path(), 3600, 0).unwrap();

        let url = "http://example.com/attack";
        let hash = DiskCache::cache_key(url);
        // ~keep The legacy (pre-fix) temp-file name was fully deterministic from the
        // ~keep cache key, so an attacker who knows the target URL could pre-plant a
        // ~keep symlink there before the crawl ever runs.
        let legacy_tmp_path = dir.path().join(format!("{hash}.tmp"));

        let canary_dir = tempfile::tempdir().unwrap();
        let canary_path = canary_dir.path().join("canary.txt");
        std::fs::write(&canary_path, "original-canary-contents").unwrap();
        std::os::unix::fs::symlink(&canary_path, &legacy_tmp_path).unwrap();

        let page = make_page(url);
        cache
            .set(url, &page)
            .await
            .expect("cache set should succeed despite a pre-existing symlink at the legacy tmp path");

        let canary_contents = std::fs::read_to_string(&canary_path).unwrap();
        assert_eq!(
            canary_contents, "original-canary-contents",
            "symlink target must not be written through"
        );

        let retrieved = cache
            .get(url)
            .await
            .unwrap()
            .expect("entry should be cached via a safe, unpredictable temp path");
        assert_eq!(retrieved.url, url);
    }

    #[cfg(unix)]
    #[test]
    fn test_write_cache_entry_to_rejects_existing_symlink_at_tmp_path() {
        let dir = tempfile::tempdir().unwrap();
        let canary_dir = tempfile::tempdir().unwrap();
        let canary_path = canary_dir.path().join("canary.txt");
        std::fs::write(&canary_path, "original").unwrap();

        let tmp_path = dir.path().join("entry.json.tmp");
        let final_path = dir.path().join("entry.json");
        std::os::unix::fs::symlink(&canary_path, &tmp_path).unwrap();

        let result = write_cache_entry_to(&tmp_path, &final_path, "malicious-payload");
        assert!(result.is_err(), "write must fail rather than follow the symlink");

        let canary_contents = std::fs::read_to_string(&canary_path).unwrap();
        assert_eq!(
            canary_contents, "original",
            "symlink target must not be written through"
        );
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
