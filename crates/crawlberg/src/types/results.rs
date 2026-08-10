use std::borrow::Cow;
use std::collections::HashMap;

use ahash::AHashSet;
use serde::{Deserialize, Serialize};

use super::{
    CookieInfo, DownloadedAsset, ExtractionMeta, FeedInfo, ImageInfo, JsonLdEntry, LinkInfo, PageMetadata, ResponseMeta,
};

/// Browser-specific extras populated when the native browser backend was used.
///
/// Available on `ScrapeResult.browser` when `BrowserBackend::Native` handled the request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct BrowserExtras {
    /// Return value of `BrowserConfig.eval_script`, if provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_result: Option<serde_json::Value>,
    /// Network events captured during page navigation (only populated when
    /// `BrowserConfig.capture_network_events` is true).
    #[serde(default)]
    pub network_events: Vec<ResponseMeta>,
    /// All non-expired cookies present in the browser's cookie jar after
    /// navigation completes (includes both prior cookies and server Set-Cookie).
    #[serde(default)]
    pub cookies: Vec<CookieInfo>,
}

/// A downloaded non-HTML document (PDF, DOCX, image, code file, etc.).
///
/// When the crawler encounters non-HTML content and `download_documents` is
/// enabled, it downloads the raw bytes and populates this struct instead of
/// skipping the resource.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct DownloadedDocument {
    /// The URL the document was fetched from.
    pub url: String,
    /// The MIME type from the Content-Type header.
    pub mime_type: Cow<'static, str>,
    /// Raw document bytes. Skipped during JSON serialization.
    #[serde(skip_serializing)]
    #[cfg_attr(alef, alef(skip))]
    #[cfg_attr(feature = "mcp", schemars(skip))]
    pub content: Vec<u8>,
    /// Size of the document in bytes.
    pub size: usize,
    /// Filename extracted from Content-Disposition or URL path.
    pub filename: Option<Box<str>>,
    /// SHA-256 hex digest of the content.
    pub content_hash: Box<str>,
    /// Selected response headers.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<Box<str>, Box<str>>,
    /// True when `content` (or the file at `content_path`) was truncated to
    /// `document_max_size`; `size` still reports the original, untruncated length.
    #[serde(default)]
    pub truncated: bool,
    /// Filesystem path the document was streamed to when `document_output_dir` was
    /// set. `content` is empty in memory when this is populated.
    pub content_path: Option<String>,
    /// Base64-encoded copy of `content`, populated only when
    /// `document_content_encoding` was set to `Base64`.
    pub content_base64: Option<String>,
}

/// Result of executing a sequence of page interaction actions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct InteractionResult {
    /// Results from each executed action.
    pub action_results: Vec<ActionResult>,
    /// Final page HTML after all actions completed.
    pub final_html: String,
    /// Final page URL (may have changed due to navigation).
    pub final_url: String,
    /// Screenshot taken after all actions, if requested.
    #[serde(skip)]
    #[cfg_attr(alef, alef(skip))]
    #[cfg_attr(feature = "mcp", schemars(skip))]
    pub screenshot: Option<Vec<u8>>,
    /// Base64-encoded PNG screenshot taken after all actions.
    ///
    /// Populated only when a `PageAction::Screenshot` action actually ran, so
    /// callers that never request a screenshot do not pay the encoding cost.
    pub screenshot_base64: Option<String>,
}

/// Result from a single page action execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct ActionResult {
    /// Zero-based index of the action in the sequence.
    pub action_index: usize,
    /// The type of action that was executed.
    pub action_type: Cow<'static, str>,
    /// Whether the action completed successfully.
    pub success: bool,
    /// Action-specific return data (screenshot bytes, JS return value, scraped HTML).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Error message if the action failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The result of a single-page scrape operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ScrapeResult {
    /// The HTTP status code of the response.
    pub status_code: u16,
    /// The final URL after following all redirects.
    pub final_url: String,
    /// The Content-Type header value.
    pub content_type: String,
    /// The HTML body of the response.
    pub html: String,
    /// The size of the response body in bytes.
    pub body_size: usize,
    /// Extracted metadata from the page.
    pub metadata: PageMetadata,
    /// Links found on the page.
    pub links: Vec<LinkInfo>,
    /// Images found on the page.
    pub images: Vec<ImageInfo>,
    /// Feed links found on the page.
    pub feeds: Vec<FeedInfo>,
    /// JSON-LD entries found on the page.
    pub json_ld: Vec<JsonLdEntry>,
    /// Whether the URL is allowed by robots.txt.
    pub is_allowed: bool,
    /// The crawl delay from robots.txt, in seconds.
    pub crawl_delay: Option<u64>,
    /// Whether a noindex directive was detected.
    pub noindex_detected: bool,
    /// Whether a nofollow directive was detected.
    pub nofollow_detected: bool,
    /// The X-Robots-Tag header value, if present.
    pub x_robots_tag: Option<String>,
    /// Whether the content is a PDF.
    pub is_pdf: bool,
    /// Whether the page was skipped (binary or PDF content).
    pub was_skipped: bool,
    /// The detected character set encoding.
    pub detected_charset: Option<String>,
    /// Whether an authentication header was sent with the request.
    pub auth_header_sent: bool,
    /// Response metadata extracted from HTTP headers.
    pub response_meta: Option<ResponseMeta>,
    /// Downloaded assets from the page.
    pub assets: Vec<DownloadedAsset>,
    /// Whether the page content suggests JavaScript rendering is needed.
    pub js_render_hint: bool,
    /// Whether the browser fallback was used to fetch this page.
    pub browser_used: bool,
    /// Markdown conversion of the page content.
    pub markdown: Option<MarkdownResult>,
    /// Structured data extracted by LLM. Populated when extraction is configured.
    pub extracted_data: Option<serde_json::Value>,
    /// Metadata about the LLM extraction pass (cost, tokens, model).
    pub extraction_meta: Option<ExtractionMeta>,
    /// Screenshot of the page as PNG bytes. Populated when browser is used and capture_screenshot is enabled.
    #[serde(skip)]
    #[cfg_attr(alef, alef(skip))]
    #[cfg_attr(feature = "mcp", schemars(skip))]
    pub screenshot: Option<Vec<u8>>,
    /// Base64-encoded PNG screenshot of the page.
    ///
    /// Populated only when `CrawlConfig.capture_screenshot` was enabled for this
    /// request, so callers that never requested a screenshot do not pay the
    /// encoding cost.
    pub screenshot_base64: Option<String>,
    /// Downloaded non-HTML document (PDF, DOCX, image, code, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downloaded_document: Option<DownloadedDocument>,
    /// Browser-specific extras (eval result, network events, cookies). Only
    /// populated when `BrowserBackend::Native` was used for this request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserExtras>,
}

/// The result of crawling a single page during a crawl operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CrawlPageResult {
    /// The original URL of the page.
    pub url: String,
    /// The normalized URL of the page.
    pub normalized_url: String,
    /// The HTTP status code of the response.
    pub status_code: u16,
    /// The Content-Type header value.
    pub content_type: String,
    /// The HTML body of the response.
    pub html: String,
    /// The size of the response body in bytes.
    pub body_size: usize,
    /// Extracted metadata from the page.
    pub metadata: PageMetadata,
    /// Links found on the page.
    pub links: Vec<LinkInfo>,
    /// Images found on the page.
    pub images: Vec<ImageInfo>,
    /// Feed links found on the page.
    pub feeds: Vec<FeedInfo>,
    /// JSON-LD entries found on the page.
    pub json_ld: Vec<JsonLdEntry>,
    /// The depth of this page from the start URL.
    pub depth: usize,
    /// Whether this page is on the same domain as the start URL.
    pub stayed_on_domain: bool,
    /// Whether this page was skipped (binary or PDF content).
    pub was_skipped: bool,
    /// Whether the content is a PDF.
    pub is_pdf: bool,
    /// The detected character set encoding.
    pub detected_charset: Option<String>,
    /// Markdown conversion of the page content.
    pub markdown: Option<MarkdownResult>,
    /// Structured data extracted by LLM. Populated when extraction is configured.
    pub extracted_data: Option<serde_json::Value>,
    /// Metadata about the LLM extraction pass (cost, tokens, model).
    pub extraction_meta: Option<ExtractionMeta>,
    /// Downloaded non-HTML document (PDF, DOCX, image, code, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downloaded_document: Option<DownloadedDocument>,
    /// Whether the browser fallback was used to fetch this page.
    pub browser_used: bool,
}

/// The result of a multi-page crawl operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CrawlResult {
    /// The list of crawled pages.
    pub pages: Vec<CrawlPageResult>,
    /// The final URL after following redirects.
    pub final_url: String,
    /// The number of redirects followed.
    pub redirect_count: usize,
    /// Whether any page was skipped during crawling.
    pub was_skipped: bool,
    /// An error message, if the crawl encountered an issue.
    pub error: Option<String>,
    /// Cookies collected during the crawl.
    pub cookies: Vec<CookieInfo>,
    /// Whether all crawled pages stayed on the same domain as the start URL.
    pub stayed_on_domain: bool,
    /// Whether the browser fallback was used for any page in this crawl.
    pub browser_used: bool,
    /// Normalized URLs encountered during crawling (for deduplication counting).
    ///
    /// Deprecated: duplicates `CrawlPageResult.normalized_url`, which already
    /// reaches every binding. [`CrawlResult::unique_normalized_urls`] no longer
    /// reads this field; it is kept only to avoid a breaking field removal.
    #[deprecated(note = "use CrawlPageResult.normalized_url via CrawlResult::unique_normalized_urls instead")]
    #[serde(default, skip_serializing)]
    #[cfg_attr(alef, alef(skip))]
    #[cfg_attr(feature = "mcp", schemars(skip))]
    pub normalized_urls: Vec<String>,
}

impl CrawlResult {
    /// Create a new `CrawlResult` with the given fields.
    ///
    /// `normalized_urls` is accepted for caller compatibility but no longer
    /// stored: it duplicated `CrawlPageResult.normalized_url` on every page.
    /// See [`CrawlResult::unique_normalized_urls`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        pages: Vec<CrawlPageResult>,
        final_url: String,
        redirect_count: usize,
        was_skipped: bool,
        error: Option<String>,
        cookies: Vec<CookieInfo>,
        stayed_on_domain: bool,
        _normalized_urls: Vec<String>,
    ) -> Self {
        let browser_used = pages.iter().any(|p| p.browser_used);
        #[allow(deprecated)]
        Self {
            pages,
            final_url,
            redirect_count,
            was_skipped,
            error,
            cookies,
            stayed_on_domain,
            browser_used,
            normalized_urls: Vec::new(),
        }
    }

    /// Returns the count of unique normalized URLs encountered during crawling.
    ///
    /// Computed from `pages` (not the deprecated `normalized_urls` field) so it
    /// is correct across every binding that reconstructs `CrawlResult` from
    /// `pages` alone. In streaming mode `pages` is empty, so this returns 0 on
    /// the opaque-handle (C/Go/C#/Zig/Dart) path where it previously counted
    /// streamed pages — a known, accepted cost of making the other ten binding
    /// families correct.
    pub fn unique_normalized_urls(&self) -> usize {
        self.pages
            .iter()
            .map(|p| p.normalized_url.as_str())
            .collect::<AHashSet<_>>()
            .len()
    }
}

#[cfg(test)]
mod crawl_result_tests {
    use super::{CookieInfo, CrawlPageResult, CrawlResult};

    fn page_with_normalized_url(normalized_url: &str) -> CrawlPageResult {
        CrawlPageResult {
            normalized_url: normalized_url.to_owned(),
            ..CrawlPageResult::default()
        }
    }

    #[test]
    fn unique_normalized_urls_counts_distinct_pages_and_ignores_duplicates() {
        let result = CrawlResult::new(
            vec![
                page_with_normalized_url("https://example.com/a"),
                page_with_normalized_url("https://example.com/b"),
                page_with_normalized_url("https://example.com/a"),
            ],
            "https://example.com/".to_owned(),
            0,
            false,
            None,
            Vec::<CookieInfo>::new(),
            true,
            Vec::new(),
        );

        assert_eq!(
            result.unique_normalized_urls(),
            2,
            "must count distinct CrawlPageResult.normalized_url values, deduplicating the repeated page"
        );
    }

    #[test]
    fn unique_normalized_urls_is_zero_when_pages_is_empty() {
        let result = CrawlResult::new(
            Vec::new(),
            "https://example.com/".to_owned(),
            0,
            false,
            None,
            Vec::<CookieInfo>::new(),
            true,
            Vec::new(),
        );

        assert_eq!(
            result.unique_normalized_urls(),
            0,
            "an empty pages list (e.g. streaming mode) must report zero unique URLs, not panic"
        );
    }

    #[test]
    fn new_does_not_populate_the_deprecated_normalized_urls_field_even_when_given_input() {
        #[allow(deprecated)]
        let result = CrawlResult::new(
            vec![page_with_normalized_url("https://example.com/a")],
            "https://example.com/".to_owned(),
            0,
            false,
            None,
            Vec::<CookieInfo>::new(),
            true,
            vec!["https://example.com/a".to_owned(), "https://example.com/b".to_owned()],
        );

        #[allow(deprecated)]
        let normalized_urls = &result.normalized_urls;
        assert!(
            normalized_urls.is_empty(),
            "normalized_urls must stay empty regardless of what the caller passes in, got {normalized_urls:?}"
        );
    }
}

/// A URL entry from a sitemap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SitemapUrl {
    /// The URL.
    pub url: String,
    /// The last modification date, if present.
    pub lastmod: Option<String>,
    /// The change frequency, if present.
    pub changefreq: Option<String>,
    /// The priority, if present.
    pub priority: Option<String>,
}

/// The result of a map operation, containing discovered URLs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MapResult {
    /// The list of discovered URLs.
    pub urls: Vec<SitemapUrl>,
}

/// Rich markdown conversion result from HTML processing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MarkdownResult {
    /// Converted markdown text.
    pub content: String,
    /// Structured document tree with semantic nodes.
    pub document_structure: Option<serde_json::Value>,
    /// Extracted tables with structured cell data.
    pub tables: Vec<serde_json::Value>,
    /// Non-fatal processing warnings.
    pub warnings: Vec<String>,
    /// Whether citation conversion was applied and produced at least one reference.
    ///
    /// `true` when the markdown contained inline links that were converted to
    /// numbered citation references. The converted content (with `[N]` markers)
    /// is available in `content`; the full reference list is accessible via
    /// [`crate::citations::generate_citations`] if needed separately.
    pub citations: bool,
    /// Content-filtered markdown optimized for LLM consumption.
    pub fit_content: Option<String>,
}

/// Cached page data for HTTP response caching.
///
/// Used only by the `CrawlCache` storage-backend trait, which is not part of
/// the polyglot binding surface. Hidden from alef so bindings don't expose a
/// type they can never receive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(alef, alef(skip))]
pub struct CachedPage {
    /// Absolute URL of the cached page.
    pub url: String,
    /// HTTP status code returned at the time the page was cached.
    pub status_code: u16,
    /// `Content-Type` header captured from the original response.
    pub content_type: String,
    /// Raw response body stored verbatim in the cache.
    pub body: String,
    /// `ETag` header value, if any — used for conditional revalidation.
    pub etag: Option<String>,
    /// `Last-Modified` header value, if any — used for conditional revalidation.
    pub last_modified: Option<String>,
    /// Unix timestamp (seconds) when the entry was written to the cache.
    pub cached_at: u64,
}
