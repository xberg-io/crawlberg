---
title: "URL Discovery"
---

The map operation discovers all URLs on a website without downloading full page content. It combines sitemap parsing with HTML link extraction for comprehensive URL enumeration.

## Basic map operation

```rust
use crawlberg::{CrawlConfig, create_engine, map_urls};

let engine = create_engine(Some(CrawlConfig {
    respect_robots_txt: true,
    ..Default::default()
}))?;

let result = map_urls(&engine, "https://example.com").await?;

for entry in &result.urls {
    println!("{}", entry.url);
    if let Some(ref lastmod) = entry.lastmod {
        println!("  Last modified: {}", lastmod);
    }
}
```

## Discovery strategy

The map operation tries multiple strategies in order, returning results from the first that succeeds:

1. **Robots.txt sitemap directives** -- When `respect_robots_txt` is `true`, fetches `robots.txt` and follows any `Sitemap:` directives found. Supports sitemap index files that reference multiple child sitemaps.

2. **`/sitemap.xml` fallback** -- Attempts to fetch `/sitemap.xml` at the site root. Processes the response if it contains `<urlset>` or `<sitemapindex>` XML.

3. **Direct URL fetch** -- Fetches the provided URL directly and attempts to parse it as:
   - A gzip-compressed sitemap (detected by Content-Type, `.gz` extension, or gzip magic bytes)
   - A sitemap index XML file
   - A regular sitemap XML file
   - An HTML page (extracts all internal and external links)

4. **Empty result** -- If none of the above produces URLs, returns an empty `MapResult`.

## Sitemap parsing

The engine handles the full range of sitemap formats:

### Standard XML sitemaps

```xml
<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>https://example.com/page1</loc>
    <lastmod>2024-01-15</lastmod>
    <changefreq>weekly</changefreq>
    <priority>0.8</priority>
  </url>
</urlset>
```

### Sitemap index files

When a sitemap index is encountered, the engine recursively fetches all referenced child sitemaps:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap>
    <loc>https://example.com/sitemap-posts.xml</loc>
  </sitemap>
  <sitemap>
    <loc>https://example.com/sitemap-pages.xml.gz</loc>
  </sitemap>
</sitemapindex>
```

### Gzip-compressed sitemaps

Compressed sitemaps (`.xml.gz`) are automatically decompressed. Detection uses three signals:

- Content-Type containing `gzip` or `x-gzip`
- URL ending in `.gz`
- Gzip magic bytes (`0x1f 0x8b`) at the start of the response body

### HTML link extraction

When the URL returns an HTML page, the engine extracts all `<a>` links and classifies them:

- **Internal** and **Document** links are deduplicated by normalized URL and included with fragments stripped
- **External** links are also included and deduplicated

## SitemapUrl fields

Each discovered URL is returned as a `SitemapUrl`:

| Field        | Type             | Description                                                           |
| ------------ | ---------------- | --------------------------------------------------------------------- |
| `url`        | `String`         | The discovered URL.                                                   |
| `lastmod`    | `Option<String>` | Last modification date from the sitemap, if present.                  |
| `changefreq` | `Option<String>` | Change frequency hint from the sitemap (e.g., `"weekly"`, `"daily"`). |
| `priority`   | `Option<String>` | Priority value from the sitemap (e.g., `"0.8"`).                      |

:::note
The `lastmod`, `changefreq`, and `priority` fields are only populated when URLs are discovered through XML sitemaps. URLs discovered through HTML link extraction have these fields set to `None`.
:::

## Search filter

Filter discovered URLs by substring match:

```rust
CrawlConfig {
    map_search: Some("blog".to_string()),
    ..Default::default()
}
```

The search is case-insensitive and matches against the full URL string. Only URLs containing the search term are included in the result.

## Limit control

Cap the number of returned URLs:

```rust
CrawlConfig {
    map_limit: Some(100),
    ..Default::default()
}
```

The limit is applied after search and path filtering. When both `map_search` and `map_limit` are set, search runs first, then the result is truncated.

## Path exclusion

The `exclude_paths` regex patterns also apply to map results:

```rust
CrawlConfig {
    exclude_paths: vec![r"/admin/".to_string(), r"/api/".to_string()],
    ..Default::default()
}
```

Each pattern is matched against the URL's path component. URLs matching any exclude pattern are removed from the result.

:::caution
`include_paths` does not apply to map operations. Use `map_search` or `exclude_paths` for filtering map results.
:::

## Filter application order

Filters are applied in this order:

1. `exclude_paths` -- regex against URL path
2. `map_search` -- case-insensitive substring match on full URL
3. `map_limit` -- truncate to the specified count

## Configuration reference

| Field                | Type             | Default            | Description                                         |
| -------------------- | ---------------- | ------------------ | --------------------------------------------------- |
| `map_limit`          | `Option<usize>`  | `None` (unlimited) | Maximum number of URLs to return.                   |
| `map_search`         | `Option<String>` | `None`             | Case-insensitive substring filter on URLs.          |
| `exclude_paths`      | `Vec<String>`    | `[]`               | Regex patterns to exclude by URL path.              |
| `respect_robots_txt` | `bool`           | `false`            | Whether to check robots.txt for sitemap directives. |

## Combining with crawl

A common pattern is to discover URLs first, then selectively crawl or scrape them:

```rust
// Discover all URLs
let map_result = engine.map("https://example.com").await?;

// Filter to interesting pages
let blog_urls: Vec<&str> = map_result.urls
    .iter()
    .filter(|u| u.url.contains("/blog/"))
    .map(|u| u.url.as_str())
    .collect();

// Scrape them in batch
let scrape_results = engine.batch_scrape(&blog_urls).await;
```
