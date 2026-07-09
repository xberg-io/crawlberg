use serde::{Deserialize, Serialize};

// ~keep CrawlPageResult is only referenced inside CrawlEvent, which is not compiled on wasm32.
#[cfg(not(target_arch = "wasm32"))]
use super::CrawlPageResult;

/// The classification of a link.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkType {
    /// A link to the same domain.
    #[default]
    Internal,
    /// A link to a different domain.
    External,
    /// A fragment-only link (e.g., `#section`).
    Anchor,
    /// A link to a downloadable document (PDF, DOC, etc.).
    Document,
}

impl std::fmt::Display for LinkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal => write!(f, "internal"),
            Self::External => write!(f, "external"),
            Self::Anchor => write!(f, "anchor"),
            Self::Document => write!(f, "document"),
        }
    }
}

impl std::fmt::Debug for LinkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

/// Information about a link found on a page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkInfo {
    /// The resolved URL of the link.
    pub url: String,
    /// The visible text of the link.
    pub text: String,
    /// The classification of the link.
    pub link_type: LinkType,
    /// The `rel` attribute value, if present.
    pub rel: Option<String>,
    /// Whether the link has `rel="nofollow"`.
    pub nofollow: bool,
}

/// The source of an image reference.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageSource {
    /// An `<img>` tag.
    #[default]
    Img,
    /// A `<source>` tag inside `<picture>`.
    PictureSource,
    /// An `og:image` meta tag.
    #[serde(rename = "og:image")]
    OgImage,
    /// A `twitter:image` meta tag.
    #[serde(rename = "twitter:image")]
    TwitterImage,
}

impl std::fmt::Display for ImageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Img => write!(f, "img"),
            Self::PictureSource => write!(f, "picture_source"),
            Self::OgImage => write!(f, "og:image"),
            Self::TwitterImage => write!(f, "twitter:image"),
        }
    }
}

/// Information about an image found on a page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInfo {
    /// The image URL.
    pub url: String,
    /// The alt text, if present.
    pub alt: Option<String>,
    /// The width attribute, if present and parseable.
    pub width: Option<u32>,
    /// The height attribute, if present and parseable.
    pub height: Option<u32>,
    /// The source of the image reference.
    pub source: ImageSource,
}

/// The type of a feed (RSS, Atom, or JSON Feed).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedType {
    /// RSS feed.
    #[default]
    Rss,
    /// Atom feed.
    Atom,
    /// JSON Feed.
    JsonFeed,
}

/// Information about a feed link found on a page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedInfo {
    /// The feed URL.
    pub url: String,
    /// The feed title, if present.
    pub title: Option<String>,
    /// The type of feed.
    pub feed_type: FeedType,
}

/// A JSON-LD structured data entry found on a page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonLdEntry {
    /// The `@type` value from the JSON-LD object.
    pub schema_type: String,
    /// The `name` value, if present.
    pub name: Option<String>,
    /// The raw JSON-LD string.
    pub raw: String,
}

/// Information about an HTTP cookie received from a response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CookieInfo {
    /// The cookie name.
    pub name: String,
    /// The cookie value.
    pub value: String,
    /// The cookie domain, if specified.
    pub domain: Option<String>,
    /// The cookie path, if specified.
    pub path: Option<String>,
}

/// A downloaded asset from a page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadedAsset {
    /// The original URL of the asset.
    pub url: String,
    /// The SHA-256 content hash of the asset.
    pub content_hash: String,
    /// The MIME type from the Content-Type header.
    pub mime_type: Option<String>,
    /// The size of the asset in bytes.
    pub size: usize,
    /// The category of the asset.
    pub asset_category: AssetCategory,
    /// The HTML tag that referenced this asset (e.g., "link", "script", "img").
    pub html_tag: Option<String>,
}

/// The category of a downloaded asset.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssetCategory {
    /// A document file (PDF, DOC, etc.).
    Document,
    /// An image file.
    #[default]
    Image,
    /// An audio file.
    Audio,
    /// A video file.
    Video,
    /// A font file.
    Font,
    /// A CSS stylesheet.
    Stylesheet,
    /// A JavaScript file.
    Script,
    /// An archive file (ZIP, TAR, etc.).
    Archive,
    /// A data file (JSON, XML, CSV, etc.).
    Data,
    /// An unrecognized asset type.
    Other,
}

impl std::fmt::Display for AssetCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document => write!(f, "document"),
            Self::Image => write!(f, "image"),
            Self::Audio => write!(f, "audio"),
            Self::Video => write!(f, "video"),
            Self::Font => write!(f, "font"),
            Self::Stylesheet => write!(f, "stylesheet"),
            Self::Script => write!(f, "script"),
            Self::Archive => write!(f, "archive"),
            Self::Data => write!(f, "data"),
            Self::Other => write!(f, "other"),
        }
    }
}

impl std::fmt::Debug for AssetCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

/// An event emitted during a streaming crawl operation.
///
/// Not available on `wasm32` targets — streaming requires native concurrency
/// primitives (tokio channels, `JoinSet`) that are not supported on wasm32.
///
/// Delivered to bindings through each target's native streaming idiom.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CrawlEvent {
    /// A single page has been crawled.
    Page {
        /// The crawled page result.
        result: Box<CrawlPageResult>,
    },
    /// An error occurred while crawling a URL.
    Error {
        /// The URL that failed.
        url: String,
        /// The error message.
        error: String,
    },
    /// The crawl has completed.
    Complete {
        /// Total number of pages crawled.
        pages_crawled: usize,
    },
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for CrawlEvent {
    fn default() -> Self {
        Self::Complete { pages_crawled: 0 }
    }
}
