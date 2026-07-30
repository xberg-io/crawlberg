//! Typed output DTOs for MCP tools whose result is not a single core type.
//!
//! These back SEP-2106: the same struct is serialized into `structuredContent`
//! *and* used to derive the advertised `outputSchema`, so the two cannot drift.
//! `deny_unknown_fields` makes schemars emit `additionalProperties: false`, which
//! lets the drift tests reject any field the schema and serialization disagree on.

use rmcp::schemars;

use crate::types::{CrawlResult, ScrapeResult};

/// Output of the `get_version` tool.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct VersionOutput {
    /// The crawlberg library version.
    pub version: String,
}

/// Output of the `download` tool.
///
/// Two shapes share this struct: a downloaded document (`mime_type`, `size`,
/// `filename`, `content_hash`) or an HTML page that was not a downloadable
/// document (`content_type`, `status_code`, `body_size`, `note`). Unset fields
/// are omitted from both the serialization and — because they are `Option` —
/// left out of the schema's `required` set.
#[derive(Debug, Clone, Default, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DownloadOutput {
    /// The URL that was fetched.
    pub url: String,
    /// MIME type of the downloaded document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Size of the downloaded document in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    /// Filename of the downloaded document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// SHA-256 hex digest of the downloaded document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Content-Type when the URL returned HTML rather than a document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// HTTP status code when the URL returned HTML rather than a document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// Response body size when the URL returned HTML rather than a document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_size: Option<usize>,
    /// Explanatory note when the URL returned HTML rather than a document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One entry in a `batch_scrape` result.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchScrapeItem {
    /// The URL this entry scraped.
    pub url: String,
    /// Whether the scrape succeeded.
    pub ok: bool,
    /// The scrape result, present when `ok` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ScrapeResult>,
    /// The error message, present when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One entry in a `batch_crawl` result.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchCrawlItem {
    /// The seed URL this entry crawled.
    pub url: String,
    /// Whether the crawl succeeded.
    pub ok: bool,
    /// The crawl result, present when `ok` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<CrawlResult>,
    /// The error message, present when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
