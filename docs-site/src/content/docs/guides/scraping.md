---
title: "Scraping"
---

Scraping fetches a single URL and runs the full extraction pipeline: metadata, links, images, feeds, JSON-LD, robots.txt compliance, markdown conversion, and optionally LLM-powered structured extraction.

## Single-page scrape

```rust
use crawlberg::{CrawlConfig, create_engine, scrape};

let engine = create_engine(Some(CrawlConfig::default()))?;

let result = scrape(&engine, "https://example.com").await?;

println!("Status: {}", result.status_code);
println!("Title: {:?}", result.metadata.title);
println!("Links: {}", result.links.len());
println!("Body size: {} bytes", result.body_size);
```

The scrape request routes through the engine's Tower service stack, which applies per-domain rate limiting, HTTP response caching, and user-agent rotation before the actual HTTP fetch.

## Page interaction

Use `interact()` when a page must be changed before HTML is captured: click a button, type into an input, wait for a selector, run JavaScript, take a screenshot, or scrape the current DOM.

```rust
use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, PageAction, create_engine, interact,
};

let engine = create_engine(Some(CrawlConfig {
    browser: BrowserConfig {
        backend: BrowserBackend::Chromiumoxide,
        mode: BrowserMode::Always,
        ..BrowserConfig::default()
    },
    ..CrawlConfig::default()
}))?;

let result = interact(
    &engine,
    "https://example.com",
    vec![
        PageAction::Click {
            selector: "#show-more".to_string(),
        },
        PageAction::Wait {
            milliseconds: None,
            selector: Some("#expanded".to_string()),
        },
        PageAction::Scrape,
    ],
)
.await?;

println!("Final URL: {}", result.final_url);
println!("HTML bytes: {}", result.final_html.len());
```

`interact()` validates the action list before navigation and returns one `ActionResult` per action. Failed actions are recorded in the result and later actions still run; navigation/setup failures are returned as `CrawlError`.

Both browser backends support click, type, press, scroll, wait, screenshot, JavaScript execution, and scrape actions. Chromiumoxide screenshots are captured from Chrome. Native screenshots are deterministic PNG snapshots derived from the post-action HTML, so they are useful for inspection but are not pixel-perfect Chrome compositor captures.

## ScrapeResult fields

The `ScrapeResult` struct contains everything extracted from a single page:

### Core response data

| Field              | Type             | Description                                                             |
| ------------------ | ---------------- | ----------------------------------------------------------------------- |
| `status_code`      | `u16`            | HTTP response status code.                                              |
| `content_type`     | `String`         | The Content-Type header value.                                          |
| `html`             | `String`         | The response body (possibly truncated by `content.max_body_size`).      |
| `body_size`        | `usize`          | Size of the response body in bytes.                                     |
| `detected_charset` | `Option<String>` | Character encoding detected from Content-Type header or HTML meta tags. |
| `is_pdf`           | `bool`           | Whether the content was detected as PDF.                                |
| `was_skipped`      | `bool`           | Whether extraction was skipped (binary or PDF content).                 |

### Robots and directives

| Field               | Type             | Description                                                                                      |
| ------------------- | ---------------- | ------------------------------------------------------------------------------------------------ |
| `is_allowed`        | `bool`           | Whether the URL is permitted by robots.txt (always `true` when `respect_robots_txt` is `false`). |
| `crawl_delay`       | `Option<u64>`    | The Crawl-delay value from robots.txt, in seconds.                                               |
| `noindex_detected`  | `bool`           | Whether a `noindex` directive was found in meta robots or X-Robots-Tag.                          |
| `nofollow_detected` | `bool`           | Whether a `nofollow` directive was found in meta robots or X-Robots-Tag.                         |
| `x_robots_tag`      | `Option<String>` | The raw X-Robots-Tag header value, if present.                                                   |

### Extracted content

| Field      | Type                     | Description                                                                      |
| ---------- | ------------------------ | -------------------------------------------------------------------------------- |
| `metadata` | `PageMetadata`           | Rich metadata from meta tags, OG, Twitter, Dublin Core, and more.                |
| `links`    | `Vec<LinkInfo>`          | All links found on the page, classified by type.                                 |
| `images`   | `Vec<ImageInfo>`         | All images found, including OG and Twitter images.                               |
| `feeds`    | `Vec<FeedInfo>`          | RSS, Atom, and JSON Feed links.                                                  |
| `json_ld`  | `Vec<JsonLdEntry>`       | JSON-LD structured data entries.                                                 |
| `markdown` | `Option<MarkdownResult>` | Markdown conversion with document structure, tables, a citation-applied boolean, and fit content. |

`MarkdownResult.citations` is `true` when link-to-citation conversion ran and produced at least one reference. Call `generate_citations(markdown.content.as_str())` when you need the converted content plus the full reference list as `CitationResult.references`.

### Engine state

| Field                 | Type                         | Description                                                                      |
| --------------------- | ---------------------------- | -------------------------------------------------------------------------------- |
| `auth_header_sent`    | `bool`                       | Whether an authentication header was sent.                                       |
| `response_meta`       | `Option<ResponseMeta>`       | HTTP headers: ETag, Last-Modified, Cache-Control, Server, etc.                   |
| `assets`              | `Vec<DownloadedAsset>`       | Downloaded page assets (when `download_assets` is enabled).                      |
| `js_render_hint`      | `bool`                       | Whether the page content suggests JavaScript rendering is needed.                |
| `browser_used`        | `bool`                       | Whether the headless browser fallback was used.                                  |
| `screenshot`          | `Option<Vec<u8>>`            | PNG screenshot bytes (when browser is used and `capture_screenshot` is enabled). |
| `downloaded_document` | `Option<DownloadedDocument>` | Non-HTML document data (PDF, DOCX, etc.) when `download_documents` is enabled.   |

### LLM extraction

| Field             | Type                     | Description                                                                     |
| ----------------- | ------------------------ | ------------------------------------------------------------------------------- |
| `extracted_data`  | `Option<Value>`          | Structured JSON extracted by an LLM, when LLM extraction is configured.         |
| `extraction_meta` | `Option<ExtractionMeta>` | LLM cost tracking: estimated cost in USD, prompt/completion tokens, model name. |

## Metadata extraction

The `PageMetadata` struct extracts 40+ fields from HTML meta tags:

### Standard meta

- `title` -- from `<title>` element
- `description` -- from `<meta name="description">`
- `canonical_url` -- from `<link rel="canonical">`
- `keywords`, `author`, `viewport`, `theme_color`, `generator`, `robots`
- `html_lang`, `html_dir` -- from the `<html>` element's `lang` and `dir` attributes

### Open Graph

- `og_title`, `og_type`, `og_image`, `og_description`, `og_url`
- `og_site_name`, `og_locale`, `og_video`, `og_audio`, `og_locale_alternates`

### Twitter Card

- `twitter_card`, `twitter_title`, `twitter_description`, `twitter_image`
- `twitter_site`, `twitter_creator`

### Dublin Core

- `dc_title`, `dc_creator`, `dc_subject`, `dc_description`, `dc_publisher`
- `dc_date`, `dc_type`, `dc_format`, `dc_identifier`, `dc_language`, `dc_rights`

### Structured data

- `article` -- `ArticleMetadata` from `article:*` OG tags (published_time, modified_time, author, section, tags)
- `hreflangs` -- alternate language links
- `favicons` -- icon links with sizes and MIME types
- `headings` -- all h1-h6 elements with level and text
- `word_count` -- computed word count of the page body text

## Link extraction

Each `LinkInfo` includes:

| Field       | Type             | Description                                                      |
| ----------- | ---------------- | ---------------------------------------------------------------- |
| `url`       | `String`         | The resolved absolute URL.                                       |
| `text`      | `String`         | The visible link text.                                           |
| `link_type` | `LinkType`       | Classification: `Internal`, `External`, `Anchor`, or `Document`. |
| `rel`       | `Option<String>` | The `rel` attribute value.                                       |
| `nofollow`  | `bool`           | Whether the link has `rel="nofollow"`.                           |

## Image extraction

Each `ImageInfo` includes:

| Field    | Type             | Description                                                                      |
| -------- | ---------------- | -------------------------------------------------------------------------------- |
| `url`    | `String`         | The image URL.                                                                   |
| `alt`    | `Option<String>` | Alt text.                                                                        |
| `width`  | `Option<u32>`    | Width attribute.                                                                 |
| `height` | `Option<u32>`    | Height attribute.                                                                |
| `source` | `ImageSource`    | Where the image was found: `Img`, `PictureSource`, `OgImage`, or `TwitterImage`. |

## Feed extraction

Discovered RSS, Atom, and JSON Feed links:

| Field       | Type             | Description                           |
| ----------- | ---------------- | ------------------------------------- |
| `url`       | `String`         | The feed URL.                         |
| `title`     | `Option<String>` | The feed title from the link element. |
| `feed_type` | `FeedType`       | `Rss`, `Atom`, or `JsonFeed`.         |

## JSON-LD extraction

Each `JsonLdEntry` contains:

| Field         | Type             | Description                                         |
| ------------- | ---------------- | --------------------------------------------------- |
| `schema_type` | `String`         | The `@type` value (e.g., `"Article"`, `"Product"`). |
| `name`        | `Option<String>` | The `name` field, if present.                       |
| `raw`         | `String`         | The raw JSON-LD string for full access.             |

## Robots.txt compliance

When `respect_robots_txt` is set to `true`, the engine fetches and parses `robots.txt` before scraping:

```rust
CrawlConfig {
    respect_robots_txt: true,
    user_agent: Some("MyBot/1.0".to_string()),
    ..Default::default()
}
```

The scrape result includes `is_allowed`, `crawl_delay`, and any `noindex`/`nofollow` directives detected from both meta tags and X-Robots-Tag headers.

:::note
When `respect_robots_txt` is `false` (the default), `is_allowed` is always `true` and robots.txt is not fetched.
:::

## Aggressive content pruning

Strip navigation, sidebars, and boilerplate before extraction via the content preset:

```rust
use crawlberg::{CrawlConfig, ContentConfig};

CrawlConfig {
    content: ContentConfig {
        preprocessing_preset: "aggressive".to_owned(),
        ..Default::default()
    },
    ..Default::default()
}
```

`preprocessing_preset` accepts `"minimal"`, `"standard"` (default), or `"aggressive"`. The aggressive preset runs the main-content extractor before the metadata and link pipeline, so the resulting Markdown contains the primary content only.

## Removing specific tags

Strip specific elements by CSS selector before processing:

```rust
CrawlConfig {
    remove_tags: vec![
        "nav".to_string(),
        ".sidebar".to_string(),
        "#cookie-banner".to_string(),
    ],
    ..Default::default()
}
```

Tag removal runs before main content extraction and before the metadata pipeline.

## Response metadata

The `ResponseMeta` struct captures HTTP response headers:

| Field              | Type             | Description                       |
| ------------------ | ---------------- | --------------------------------- |
| `etag`             | `Option<String>` | ETag header for cache validation. |
| `last_modified`    | `Option<String>` | Last-Modified header.             |
| `cache_control`    | `Option<String>` | Cache-Control directives.         |
| `server`           | `Option<String>` | Server software identifier.       |
| `x_powered_by`     | `Option<String>` | X-Powered-By header.              |
| `content_language` | `Option<String>` | Content-Language header.          |
| `content_encoding` | `Option<String>` | Content-Encoding header.          |

## Authentication

Scrape pages behind authentication:

```rust
use crawlberg::AuthConfig;

CrawlConfig {
    auth: Some(AuthConfig::Bearer {
        token: "your-token".to_string(),
    }),
    ..Default::default()
}
```

Three authentication modes are supported:

| Mode     | Fields                 | Header sent                                     |
| -------- | ---------------------- | ----------------------------------------------- |
| `Basic`  | `username`, `password` | `Authorization: Basic <base64>`                 |
| `Bearer` | `token`                | `Authorization: Bearer <token>`                 |
| `Header` | `name`, `value`        | Custom header with the specified name and value |

## Document downloads

When `download_documents` is enabled (the default), the engine downloads non-HTML resources like PDFs, DOCX files, and images instead of skipping them:

```rust
CrawlConfig {
    download_documents: true,          // default
    document_max_size: Some(50 * 1024 * 1024), // 50 MB default
    document_mime_types: vec![],       // empty = built-in defaults
    ..Default::default()
}
```

Downloaded documents are available in the `downloaded_document` field as a `DownloadedDocument` with raw bytes, MIME type, filename, size, and a SHA-256 content hash.
