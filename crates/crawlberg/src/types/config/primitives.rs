//! Scalar config vocabulary: extraction metadata, the mode/strategy enums, and the
//! `Duration`-as-milliseconds serde adapters the config structs share.

use serde::{Deserialize, Serialize};

/// Metadata about an LLM extraction pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExtractionMeta {
    /// Estimated cost of the LLM call in USD.
    pub cost: Option<f64>,
    /// Number of prompt (input) tokens consumed.
    pub prompt_tokens: Option<u64>,
    /// Number of completion (output) tokens generated.
    pub completion_tokens: Option<u64>,
    /// The model identifier used for extraction.
    pub model: Option<String>,
    /// Number of content chunks sent to the LLM.
    pub chunks_processed: usize,
}

/// When to use the headless browser fallback.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMode {
    /// Automatically detect when JS rendering is needed and fall back to browser.
    #[default]
    Auto,
    /// Always use the browser for every request.
    Always,
    /// Never use the browser fallback.
    Never,
    /// Always use the browser with all stealth surfaces enabled.
    ///
    /// Behaves like [`Always`](BrowserMode::Always) for escalation purposes
    /// (every request is routed through the browser tier), but additionally
    /// enables:
    ///
    /// - browser JavaScript stealth patches
    /// - native-backend TLS fingerprint spoofing
    /// - stealth-aware default user-agent when no explicit UA is set
    /// - 1920×1080 viewport override
    ///
    /// Use this instead of setting the now-removed `BrowserConfig.stealth`
    /// boolean field.
    Stealth,
}

/// Wait strategy for browser page rendering.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserWait {
    /// Wait until network activity is idle.
    #[default]
    NetworkIdle,
    /// Wait for a specific CSS selector to appear in the DOM.
    Selector,
    /// Wait for a fixed duration after navigation.
    Fixed,
}

/// Browser backend used for JavaScript rendering.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserBackend {
    /// Existing Chromium/CDP backend powered by chromiumoxide.
    #[default]
    Chromiumoxide,
    /// Crawlberg-owned native browser backend derived from Obscura.
    Native,
}

/// Opt-in encoding applied to a downloaded document's bytes for callers who need the
/// content available in a serializable field rather than reading it from disk.
///
/// `None` (the `CrawlConfig.document_content_encoding` default) produces neither — unlike
/// screenshots, base64-encoding a document by default would duplicate an already
/// up-to-`document_max_size` buffer (50 MB default) in memory per document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentContentEncoding {
    /// Populate `DownloadedDocument.content_base64` with a base64-encoded copy.
    Base64,
}

/// Traversal order for a crawl.
///
/// Selects both the queue discipline and the selection strategy, because global order is a
/// property of the frontier: the engine hands its bounded selection window to the strategy, so
/// a strategy alone can only reorder URLs that have already been dequeued.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CrawlStrategyKind {
    /// Breadth-first: a FIFO frontier visits every URL at one depth before the next.
    #[default]
    Bfs,
    /// Depth-first: a LIFO frontier descends into a page's children before its siblings.
    Dfs,
    /// Highest-priority-first within the selection window, scored by `CrawlStrategy::score_url`.
    BestFirst,
    /// Like `BestFirst`, but stops once newly crawled pages stop contributing new terms.
    Adaptive,
}

/// Content filter applied to each crawled page before it reaches the result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ContentFilterKind {
    /// Keep only pages scoring at or above `bm25_threshold` for `bm25_query`.
    Bm25,
}

pub(crate) mod duration_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_millis().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

pub(crate) mod option_duration_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        d.map(|d| d.as_millis() as u64).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let ms: Option<u64> = Option::deserialize(d)?;
        Ok(ms.map(Duration::from_millis))
    }
}
