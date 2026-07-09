//! Firecrawl v1-compatible request and response types.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for `POST /v1/scrape`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ScrapeRequest {
    /// The URL to scrape.
    #[serde(default)]
    #[schema(example = "https://example.com")]
    pub url: String,
    /// Output formats to return (e.g. `["markdown", "html"]`).
    #[serde(default)]
    #[schema(example = json!(["markdown"]))]
    pub formats: Option<Vec<String>>,
    /// Whether to extract only the main content of the page.
    #[serde(default)]
    pub only_main_content: Option<bool>,
    /// CSS selectors to include.
    #[serde(default)]
    pub include_tags: Option<Vec<String>>,
    /// CSS selectors to exclude.
    #[serde(default)]
    pub exclude_tags: Option<Vec<String>>,
    /// Request timeout in milliseconds.
    #[serde(default)]
    #[schema(example = 30000)]
    pub timeout: Option<u64>,
}

/// Request body for `POST /v1/crawl`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CrawlRequest {
    /// The seed URL to start crawling from.
    #[serde(default)]
    #[schema(example = "https://example.com")]
    pub url: String,
    /// Maximum link depth to follow.
    #[serde(default)]
    #[schema(example = 2)]
    pub max_depth: Option<usize>,
    /// Maximum number of pages to crawl.
    #[serde(default)]
    #[schema(example = 100)]
    pub max_pages: Option<usize>,
    /// URL patterns to include (regex).
    #[serde(default)]
    pub include_paths: Option<Vec<String>>,
    /// URL patterns to exclude (regex).
    #[serde(default)]
    pub exclude_paths: Option<Vec<String>>,
    /// Whether to extract only the main content.
    #[serde(default)]
    pub only_main_content: Option<bool>,
}

/// Request body for `POST /v1/map`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MapRequest {
    /// The URL to discover links from.
    #[serde(default)]
    #[schema(example = "https://example.com")]
    pub url: String,
    /// Maximum number of URLs to return.
    #[serde(default)]
    #[schema(example = 100)]
    pub limit: Option<usize>,
    /// Filter URLs by search term.
    #[serde(default)]
    pub search: Option<String>,
}

/// Request body for `POST /v1/batch/scrape`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct BatchScrapeRequest {
    /// The URLs to scrape.
    #[schema(example = json!(["https://example.com", "https://example.org"]))]
    pub urls: Vec<String>,
    /// Output formats to return.
    #[serde(default)]
    pub formats: Option<Vec<String>>,
    /// Whether to extract only the main content.
    #[serde(default)]
    pub only_main_content: Option<bool>,
}

/// Request body for `POST /v1/download`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct DownloadRequest {
    /// The URL to download from.
    #[serde(default)]
    #[schema(example = "https://example.com/document.pdf")]
    pub url: String,
    /// Maximum download size in bytes.
    #[serde(default)]
    pub max_size: Option<usize>,
}

/// Structured error body with a machine-readable code and human-readable message.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Machine-readable error code (e.g. `"NOT_FOUND"`, `"RATE_LIMITED"`).
    #[schema(example = "BAD_REQUEST")]
    pub code: &'static str,
    /// Human-readable error message.
    #[schema(example = "url is required")]
    pub message: String,
}

/// Generic API response wrapper.
#[derive(Debug, Clone, Serialize)]
pub struct ApiResponse<T: Serialize> {
    /// Whether the request was successful.
    pub success: bool,
    /// Response data on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    /// Structured error on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl<T: Serialize> ApiResponse<T> {
    /// Create a successful response.
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
}

impl ApiResponse<()> {
    /// Create an error response with a code and message.
    pub fn err(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Response for async job creation (`POST /v1/crawl`, `POST /v1/batch/scrape`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JobCreatedResponse {
    /// Whether the request was accepted.
    #[schema(example = true)]
    pub success: bool,
    /// The job identifier for polling.
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    pub id: String,
}

/// Response for `GET /v1/crawl/{id}` and `GET /v1/batch/scrape/{id}`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JobStatusResponse {
    /// Job status: `"pending"`, `"in_progress"`, `"completed"`, `"failed"`, `"cancelled"`.
    #[schema(example = "completed")]
    pub status: String,
    /// Total pages expected (best-effort estimate).
    #[schema(example = 10)]
    pub total: usize,
    /// Pages completed so far.
    #[schema(example = 10)]
    pub completed: usize,
    /// Crawled page data (populated when status is `"completed"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<serde_json::Value>>,
    /// Error message (populated when status is `"failed"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for `GET /health`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Health status.
    #[schema(example = "ok")]
    pub status: &'static str,
    /// Crate version.
    #[schema(example = "0.1.0")]
    pub version: &'static str,
}

/// Response for `GET /version`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VersionResponse {
    /// Crate version string.
    #[schema(example = "0.1.0")]
    pub version: &'static str,
}
