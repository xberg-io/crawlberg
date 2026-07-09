//! Fixture loading and filtering.
//!
//! Discovers, parses, and optionally shards [`ScrapeFixture`](crate::types::ScrapeFixture)
//! entries from JSON fixture files on disk.

use crate::{Result, error::Error, types::ScrapeFixture};
use regex::Regex;
use serde::Deserialize;
use std::{fs, path::Path};

/// HuggingFace datasets-server row wrapper, used when loading the HF API format.
#[derive(Debug, Deserialize)]
pub(crate) struct HfRowWrapper {
    pub(crate) row: ScrapeFixtureRaw,
}

/// HuggingFace datasets-server response envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct HfResponse {
    pub(crate) rows: Vec<HfRowWrapper>,
    pub(crate) num_rows_total: Option<u64>,
}

/// Raw HuggingFace fixture where `id` is an integer rather than a string.
#[derive(Debug, Deserialize)]
pub(crate) struct ScrapeFixtureRaw {
    pub(crate) id: serde_json::Value,
    pub(crate) url: String,
    pub(crate) truth_text: Option<String>,
    pub(crate) lie_text: Option<String>,
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) split: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) expected_status: Option<u16>,
    #[serde(default)]
    pub(crate) verify_selectors: Vec<String>,
    #[serde(default)]
    pub(crate) verify_text: Vec<String>,
    #[serde(default)]
    pub(crate) category: Option<String>,
}

impl From<ScrapeFixtureRaw> for ScrapeFixture {
    fn from(raw: ScrapeFixtureRaw) -> Self {
        let id = match raw.id {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        ScrapeFixture {
            id,
            url: raw.url,
            truth_text: raw.truth_text,
            lie_text: raw.lie_text,
            error: raw.error,
            split: raw.split,
            tags: raw.tags,
            expected_status: raw.expected_status,
            verify_selectors: raw.verify_selectors,
            verify_text: raw.verify_text,
            category: raw.category,
        }
    }
}

/// Loads and filters benchmark fixtures from JSON files on disk.
///
/// Supports two JSON formats:
/// - Direct array: `[{"id": "1", "url": "...", ...}]`
/// - HuggingFace rows wrapper: `{"rows": [{"row": {"id": 1, "url": "...", ...}}]}`
pub struct FixtureManager {
    entries: Vec<ScrapeFixture>,
}

impl FixtureManager {
    /// Create an empty fixture manager.
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Load fixtures from a single JSON file.
    ///
    /// Accepts either a direct array of [`ScrapeFixture`] objects or the
    /// HuggingFace datasets-server envelope format. Returns the number of
    /// fixtures added from this file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Fixture`] if the file cannot be read or parsed.
    pub fn load(&mut self, path: &Path) -> Result<usize> {
        let content = fs::read_to_string(path)
            .map_err(|error| Error::Fixture(format!("failed to read {}: {error}", path.display())))?;

        let value: serde_json::Value = serde_json::from_str(&content)
            .map_err(|error| Error::Fixture(format!("failed to parse JSON from {}: {error}", path.display())))?;

        let fixtures: Vec<ScrapeFixture> = if value.is_array() {
            serde_json::from_value::<Vec<ScrapeFixtureRaw>>(value)
                .map_err(|error| {
                    Error::Fixture(format!(
                        "failed to deserialize fixture array from {}: {error}",
                        path.display()
                    ))
                })?
                .into_iter()
                .map(ScrapeFixture::from)
                .collect()
        } else if value.get("rows").is_some() {
            serde_json::from_value::<HfResponse>(value)
                .map_err(|error| {
                    Error::Fixture(format!(
                        "failed to deserialize HF rows format from {}: {error}",
                        path.display()
                    ))
                })?
                .rows
                .into_iter()
                .map(|wrapper| ScrapeFixture::from(wrapper.row))
                .collect()
        } else {
            return Err(Error::Fixture(format!(
                "unrecognized fixture format in {}: expected JSON array or HF rows object",
                path.display()
            )));
        };

        let count = fixtures.len();
        self.entries.extend(fixtures);
        Ok(count)
    }

    /// Load fixtures from all `.json` files found recursively under `path`.
    ///
    /// Files are processed in sorted order for deterministic results. Returns
    /// the total number of fixtures loaded across all files.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Fixture`] if any file cannot be read or the directory
    /// cannot be traversed.
    pub fn load_directory(&mut self, path: &Path) -> Result<usize> {
        let mut json_files = collect_json_files(path)?;
        json_files.sort();

        let mut total = 0;
        for file in &json_files {
            total += self.load(file)?;
        }
        Ok(total)
    }

    /// Retain only fixtures whose `split` field matches `split`.
    ///
    /// Fixtures with no `split` set are always dropped when filtering is active.
    pub fn filter_split(&mut self, split: &str) {
        self.entries.retain(|entry| entry.split.as_deref() == Some(split));
    }

    /// Retain only fixtures whose URL matches the given regex `pattern`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Fixture`] if `pattern` is not a valid regular expression.
    pub fn filter_url(&mut self, pattern: &str) -> Result<()> {
        let regex = Regex::new(pattern)
            .map_err(|error| Error::Fixture(format!("invalid URL filter regex {pattern:?}: {error}")))?;
        self.entries.retain(|entry| regex.is_match(&entry.url));
        Ok(())
    }

    /// Retain only fixtures that have at least one tag in `tags`.
    pub fn filter_tags(&mut self, tags: &[String]) {
        self.entries
            .retain(|entry| entry.tags.iter().any(|tag| tags.contains(tag)));
    }

    /// Partition fixtures for parallel execution.
    ///
    /// Fixtures are sorted by `id` for a deterministic order, then shard
    /// `index` (0-based) out of `total` shards is retained — every entry at
    /// position `i` where `i % total == index`.
    ///
    /// # Panics
    ///
    /// Does not panic; when `total` is 0 the method is a no-op to avoid
    /// division by zero.
    pub fn apply_shard(&mut self, index: usize, total: usize) {
        if total == 0 {
            return;
        }
        self.entries
            .sort_by(|a, b| match (a.id.parse::<u64>(), b.id.parse::<u64>()) {
                (Ok(ai), Ok(bi)) => ai.cmp(&bi),
                _ => a.id.cmp(&b.id),
            });
        self.entries = self
            .entries
            .drain(..)
            .enumerate()
            .filter(|(position, _)| position % total == index)
            .map(|(_, entry)| entry)
            .collect();
    }

    /// Return a slice of all loaded fixtures.
    pub fn entries(&self) -> &[ScrapeFixture] {
        &self.entries
    }

    /// Return the number of loaded fixtures.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` if no fixtures have been loaded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for FixtureManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Recursively collect all `.json` file paths under `dir`.
fn collect_json_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    collect_json_files_inner(dir, &mut files)?;
    Ok(files)
}

fn collect_json_files_inner(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let read = fs::read_dir(dir)
        .map_err(|error| Error::Fixture(format!("failed to read directory {}: {error}", dir.display())))?;

    for entry in read {
        let entry = entry
            .map_err(|error| Error::Fixture(format!("failed to read directory entry in {}: {error}", dir.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| Error::Fixture(format!("failed to stat {}: {error}", path.display())))?;

        if file_type.is_dir() {
            collect_json_files_inner(&path, files)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp_json(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    #[test]
    fn test_load_direct_array() {
        let json = r#"[
            {"id": "abc", "url": "https://example.com", "truth_text": "hello", "lie_text": null, "error": null}
        ]"#;
        let file = write_temp_json(json);
        let mut manager = FixtureManager::new();
        let count = manager.load(file.path()).unwrap();
        assert_eq!(count, 1);
        assert_eq!(manager.entries()[0].id, "abc");
        assert_eq!(manager.entries()[0].url, "https://example.com");
    }

    #[test]
    fn test_load_hf_rows_format() {
        let json = r#"{
            "rows": [
                {"row_idx": 0, "row": {"id": 42, "url": "https://hf.co", "truth_text": "hi", "lie_text": null, "error": null}},
                {"row_idx": 1, "row": {"id": 43, "url": "https://hf.co/2", "truth_text": null, "lie_text": null, "error": null}}
            ],
            "num_rows_total": 2
        }"#;
        let file = write_temp_json(json);
        let mut manager = FixtureManager::new();
        let count = manager.load(file.path()).unwrap();
        assert_eq!(count, 2);
        assert_eq!(manager.entries()[0].id, "42");
        assert_eq!(manager.entries()[1].id, "43");
    }

    #[test]
    fn test_load_invalid_format_returns_error() {
        let json = r#"{"not_rows": true}"#;
        let file = write_temp_json(json);
        let mut manager = FixtureManager::new();
        assert!(manager.load(file.path()).is_err());
    }

    #[test]
    fn test_filter_split() {
        let json = r#"[
            {"id": "1", "url": "https://a.com", "split": "train"},
            {"id": "2", "url": "https://b.com", "split": "test"},
            {"id": "3", "url": "https://c.com"}
        ]"#;
        let file = write_temp_json(json);
        let mut manager = FixtureManager::new();
        manager.load(file.path()).unwrap();
        manager.filter_split("train");
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.entries()[0].id, "1");
    }

    #[test]
    fn test_filter_url() {
        let json = r#"[
            {"id": "1", "url": "https://example.com/page"},
            {"id": "2", "url": "https://other.org/page"}
        ]"#;
        let file = write_temp_json(json);
        let mut manager = FixtureManager::new();
        manager.load(file.path()).unwrap();
        manager.filter_url("example\\.com").unwrap();
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.entries()[0].id, "1");
    }

    #[test]
    fn test_filter_url_invalid_regex_returns_error() {
        let mut manager = FixtureManager::new();
        assert!(manager.filter_url("[invalid").is_err());
    }

    #[test]
    fn test_filter_tags() {
        let json = r#"[
            {"id": "1", "url": "https://a.com", "tags": ["js", "spa"]},
            {"id": "2", "url": "https://b.com", "tags": ["static"]},
            {"id": "3", "url": "https://c.com", "tags": []}
        ]"#;
        let file = write_temp_json(json);
        let mut manager = FixtureManager::new();
        manager.load(file.path()).unwrap();
        manager.filter_tags(&[String::from("js")]);
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.entries()[0].id, "1");
    }

    #[test]
    fn test_apply_shard() {
        let json = r#"[
            {"id": "a", "url": "https://a.com"},
            {"id": "b", "url": "https://b.com"},
            {"id": "c", "url": "https://c.com"},
            {"id": "d", "url": "https://d.com"}
        ]"#;
        let file = write_temp_json(json);
        let mut manager = FixtureManager::new();
        manager.load(file.path()).unwrap();
        manager.apply_shard(0, 2);
        assert_eq!(manager.len(), 2);
        let ids: Vec<&str> = manager.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn test_apply_shard_zero_total_is_noop() {
        let json = r#"[{"id": "1", "url": "https://a.com"}]"#;
        let file = write_temp_json(json);
        let mut manager = FixtureManager::new();
        manager.load(file.path()).unwrap();
        manager.apply_shard(0, 0);
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_load_directory() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("sub");
        fs::create_dir(&subdir).unwrap();

        fs::write(dir.path().join("a.json"), r#"[{"id": "1", "url": "https://a.com"}]"#).unwrap();
        fs::write(
            subdir.join("b.json"),
            r#"[{"id": "2", "url": "https://b.com"}, {"id": "3", "url": "https://c.com"}]"#,
        )
        .unwrap();
        fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();

        let mut manager = FixtureManager::new();
        let count = manager.load_directory(dir.path()).unwrap();
        assert_eq!(count, 3);
        assert_eq!(manager.len(), 3);
    }
}
