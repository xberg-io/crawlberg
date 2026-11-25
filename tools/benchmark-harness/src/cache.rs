//! HTML page cache — stores and retrieves pre-fetched pages for cached-mode runs.
//!
//! Cache layout on disk:
//! ```text
//! {cache_dir}/
//!   index.json           — URL -> metadata mapping
//!   responses/
//!     {sha256_hex}.json   — CachedResponse per URL
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Result;

/// Metadata record stored in the cache index for one URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// The original URL.
    pub url: String,
    /// SHA-256 hex digest of the URL, used as the response filename stem.
    pub url_hash: String,
    /// HTTP status code of the cached response.
    pub status_code: u16,
    /// Content-Type header value.
    pub content_type: String,
    /// Size of the response body in bytes.
    pub body_size: usize,
    /// ISO 8601 timestamp of when the entry was cached.
    pub cached_at: String,
}

/// A full HTTP response stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResponse {
    /// The original URL.
    pub url: String,
    /// HTTP status code.
    pub status_code: u16,
    /// Content-Type header value.
    pub content_type: String,
    /// All response headers.
    pub headers: HashMap<String, String>,
    /// Response body as a UTF-8 string.
    pub body: String,
}

/// Manages cached HTML responses for reproducible benchmarks.
///
/// Responses are stored lazily: `get` reads from disk on demand rather than
/// loading all entries into memory at startup.
pub struct HtmlCache {
    cache_dir: PathBuf,
    index: HashMap<String, CacheEntry>,
}

impl HtmlCache {
    /// Open or create a cache at the given directory.
    ///
    /// Creates `cache_dir` and its `responses/` subdirectory if they do not
    /// exist.  Loads `index.json` if present; starts with an empty index
    /// otherwise.
    pub fn open(cache_dir: &Path) -> Result<Self> {
        let responses_dir = cache_dir.join("responses");
        std::fs::create_dir_all(&responses_dir)?;

        let index_path = cache_dir.join("index.json");
        let index = if index_path.exists() {
            let raw = std::fs::read_to_string(&index_path)?;
            serde_json::from_str(&raw)?
        } else {
            HashMap::new()
        };

        Ok(Self {
            cache_dir: cache_dir.to_owned(),
            index,
        })
    }

    /// Return the cached response for `url`, or `None` if not present.
    ///
    /// Reads the response file from disk on each call; responses are not kept
    /// in memory between calls.
    pub fn get(&self, url: &str) -> Result<Option<CachedResponse>> {
        let Some(entry) = self.index.get(url) else {
            return Ok(None);
        };
        let response_path = self
            .cache_dir
            .join("responses")
            .join(format!("{}.json", entry.url_hash));
        let raw = std::fs::read_to_string(response_path)?;
        let response: CachedResponse = serde_json::from_str(&raw)?;
        Ok(Some(response))
    }

    /// Store a response in the cache and update the index on disk.
    pub fn insert(&mut self, response: &CachedResponse) -> Result<()> {
        let hash = url_hash(&response.url);
        let response_path = self.cache_dir.join("responses").join(format!("{hash}.json"));
        let json = serde_json::to_string_pretty(response)?;
        std::fs::write(response_path, json)?;

        let entry = CacheEntry {
            url: response.url.clone(),
            url_hash: hash.clone(),
            status_code: response.status_code,
            content_type: response.content_type.clone(),
            body_size: response.body.len(),
            cached_at: jiff::Timestamp::now().to_string(),
        };
        self.index.insert(response.url.clone(), entry);
        self.save_index()?;
        Ok(())
    }

    /// Persist the index to `{cache_dir}/index.json`.
    ///
    /// Uses a write-then-rename strategy so that a crash mid-write cannot
    /// leave `index.json` in a partially-written state.
    pub fn save_index(&self) -> Result<()> {
        let index_path = self.cache_dir.join("index.json");
        let tmp_path = index_path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(&self.index)?;
        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, &index_path)?;
        Ok(())
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Returns `true` if the cache contains no entries.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Fraction of `urls` that are present in the cache.
    ///
    /// Returns `1.0` if `urls` is empty.
    pub fn coverage(&self, urls: &[&str]) -> f64 {
        if urls.is_empty() {
            return 1.0;
        }
        let cached = urls.iter().filter(|u| self.index.contains_key(**u)).count();
        cached as f64 / urls.len() as f64
    }
}

/// Compute the SHA-256 hex digest of `url`, used as the cache key.
fn url_hash(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut acc, byte| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{byte:02x}");
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_directories() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("html_cache");
        let cache = HtmlCache::open(&cache_dir).unwrap();
        assert!(cache_dir.join("responses").exists());
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn insert_and_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = HtmlCache::open(dir.path()).unwrap();

        let response = CachedResponse {
            url: "https://example.com/".to_string(),
            status_code: 200,
            content_type: "text/html".to_string(),
            headers: HashMap::new(),
            body: "<html>hello</html>".to_string(),
        };

        cache.insert(&response).unwrap();
        assert_eq!(cache.len(), 1);

        let loaded = cache.get("https://example.com/").unwrap().unwrap();
        assert_eq!(loaded.status_code, 200);
        assert_eq!(loaded.body, "<html>hello</html>");
    }

    #[test]
    fn get_returns_none_for_missing_url() {
        let dir = tempfile::tempdir().unwrap();
        let cache = HtmlCache::open(dir.path()).unwrap();
        assert!(cache.get("https://example.com/missing").unwrap().is_none());
    }

    #[test]
    fn coverage_calculation() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = HtmlCache::open(dir.path()).unwrap();

        let response = CachedResponse {
            url: "https://a.com/".to_string(),
            status_code: 200,
            content_type: "text/html".to_string(),
            headers: HashMap::new(),
            body: String::new(),
        };
        cache.insert(&response).unwrap();

        let urls = ["https://a.com/", "https://b.com/"];
        let cov = cache.coverage(&urls);
        assert!((cov - 0.5).abs() < f64::EPSILON);
        assert!((cache.coverage(&[]) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn index_persists_across_open() {
        let dir = tempfile::tempdir().unwrap();

        {
            let mut cache = HtmlCache::open(dir.path()).unwrap();
            let response = CachedResponse {
                url: "https://persisted.com/".to_string(),
                status_code: 200,
                content_type: "text/html".to_string(),
                headers: HashMap::new(),
                body: "content".to_string(),
            };
            cache.insert(&response).unwrap();
        }

        let cache = HtmlCache::open(dir.path()).unwrap();
        assert_eq!(cache.len(), 1);
        let loaded = cache.get("https://persisted.com/").unwrap().unwrap();
        assert_eq!(loaded.body, "content");
    }

    #[test]
    fn url_hash_is_deterministic() {
        let h1 = url_hash("https://example.com/");
        let h2 = url_hash("https://example.com/");
        assert_eq!(h1, h2);

        let h3 = url_hash("https://other.com/");
        assert_ne!(h1, h3);
    }
}
