use serde::{Deserialize, Serialize};

/// Article metadata extracted from `article:*` Open Graph tags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ArticleMetadata {
    /// The article publication time.
    pub published_time: Option<String>,
    /// The article modification time.
    pub modified_time: Option<String>,
    /// The article author.
    pub author: Option<String>,
    /// The article section.
    pub section: Option<String>,
    /// The article tags.
    pub tags: Vec<String>,
}

/// An hreflang alternate link entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HreflangEntry {
    /// The language code (e.g., "en", "fr", "x-default").
    pub lang: String,
    /// The URL for this language variant.
    pub url: String,
}

/// Information about a favicon or icon link.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FaviconInfo {
    /// The icon URL.
    pub url: String,
    /// The `rel` attribute (e.g., "icon", "apple-touch-icon").
    pub rel: String,
    /// The `sizes` attribute, if present.
    pub sizes: Option<String>,
    /// The MIME type, if present.
    pub mime_type: Option<String>,
}

/// A heading element extracted from the page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HeadingInfo {
    /// The heading level (1-6).
    pub level: u8,
    /// The heading text content.
    pub text: String,
}

/// Response metadata extracted from HTTP headers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResponseMeta {
    /// The ETag header value.
    pub etag: Option<String>,
    /// The Last-Modified header value.
    pub last_modified: Option<String>,
    /// The Cache-Control header value.
    pub cache_control: Option<String>,
    /// The Server header value.
    pub server: Option<String>,
    /// The X-Powered-By header value.
    pub x_powered_by: Option<String>,
    /// The Content-Language header value.
    pub content_language: Option<String>,
    /// The Content-Encoding header value.
    pub content_encoding: Option<String>,
}

/// Metadata extracted from an HTML page's `<meta>` tags and `<title>` element.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PageMetadata {
    /// The page title from the `<title>` element.
    pub title: Option<String>,
    /// The meta description.
    pub description: Option<String>,
    /// The canonical URL from `<link rel="canonical">`.
    pub canonical_url: Option<String>,
    /// Keywords from `<meta name="keywords">`.
    pub keywords: Option<String>,
    /// Author from `<meta name="author">`.
    pub author: Option<String>,
    /// Viewport content from `<meta name="viewport">`.
    pub viewport: Option<String>,
    /// Theme color from `<meta name="theme-color">`.
    pub theme_color: Option<String>,
    /// Generator from `<meta name="generator">`.
    pub generator: Option<String>,
    /// Robots content from `<meta name="robots">`.
    pub robots: Option<String>,
    /// The `lang` attribute from the `<html>` element.
    pub html_lang: Option<String>,
    /// The `dir` attribute from the `<html>` element.
    pub html_dir: Option<String>,
    /// Open Graph title.
    pub og_title: Option<String>,
    /// Open Graph type.
    pub og_type: Option<String>,
    /// Open Graph image URL.
    pub og_image: Option<String>,
    /// Open Graph description.
    pub og_description: Option<String>,
    /// Open Graph URL.
    pub og_url: Option<String>,
    /// Open Graph site name.
    pub og_site_name: Option<String>,
    /// Open Graph locale.
    pub og_locale: Option<String>,
    /// Open Graph video URL.
    pub og_video: Option<String>,
    /// Open Graph audio URL.
    pub og_audio: Option<String>,
    /// Open Graph locale alternates.
    pub og_locale_alternates: Option<Vec<String>>,
    /// Twitter card type.
    pub twitter_card: Option<String>,
    /// Twitter title.
    pub twitter_title: Option<String>,
    /// Twitter description.
    pub twitter_description: Option<String>,
    /// Twitter image URL.
    pub twitter_image: Option<String>,
    /// Twitter site handle.
    pub twitter_site: Option<String>,
    /// Twitter creator handle.
    pub twitter_creator: Option<String>,
    /// Dublin Core title.
    pub dc_title: Option<String>,
    /// Dublin Core creator.
    pub dc_creator: Option<String>,
    /// Dublin Core subject.
    pub dc_subject: Option<String>,
    /// Dublin Core description.
    pub dc_description: Option<String>,
    /// Dublin Core publisher.
    pub dc_publisher: Option<String>,
    /// Dublin Core date.
    pub dc_date: Option<String>,
    /// Dublin Core type.
    pub dc_type: Option<String>,
    /// Dublin Core format.
    pub dc_format: Option<String>,
    /// Dublin Core identifier.
    pub dc_identifier: Option<String>,
    /// Dublin Core language.
    pub dc_language: Option<String>,
    /// Dublin Core rights.
    pub dc_rights: Option<String>,
    /// Article metadata from `article:*` Open Graph tags.
    pub article: Option<ArticleMetadata>,
    /// Hreflang alternate links.
    pub hreflangs: Option<Vec<HreflangEntry>>,
    /// Favicon and icon links.
    pub favicons: Option<Vec<FaviconInfo>>,
    /// Heading elements (h1-h6).
    pub headings: Option<Vec<HeadingInfo>>,
    /// Computed word count of the page body text.
    pub word_count: Option<usize>,
}
