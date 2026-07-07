---
title: "Crawling"
---

Crawling follows links from a seed URL, building a collection of pages up to a configured depth and page limit. Crawlberg provides both collected and streaming interfaces, batch operations, and pluggable strategies for URL selection.

## Basic crawl

The simplest crawl fetches a single seed URL and follows its links:

```rust
use crawlberg::{CrawlConfig, create_engine, crawl};

let engine = create_engine(Some(CrawlConfig {
    max_depth: Some(2),
    max_pages: Some(50),
    ..Default::default()
}))?;

let result = crawl(&engine, "https://example.com").await?;

for page in &result.pages {
    println!("{} (depth {}): {} bytes", page.url, page.depth, page.body_size);
}
```

`CrawlResult` contains the full list of crawled pages, the final URL after redirect resolution, redirect count, collected cookies, and any error encountered.

## Depth and page limits

| Field       | Type            | Default                 | Description                                                                                                                                        |
| ----------- | --------------- | ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `max_depth` | `Option<usize>` | `None` (0 -- seed only) | Maximum number of link hops from the seed URL. `None` means depth 0, which fetches only the seed page.                                             |
| `max_pages` | `Option<usize>` | `None` (unlimited)      | Maximum number of pages to include in the result. The engine stops spawning fetch tasks once this limit is reached and aborts any in-flight tasks. |

:::caution[Depth 0 means seed only]
When `max_depth` is `None` or `Some(0)`, the engine fetches the seed URL but does not follow any links. Set `max_depth: Some(1)` to crawl one hop out.
:::

## Concurrent fetching

Control parallelism with `max_concurrent`:

```rust
CrawlConfig {
    max_concurrent: Some(5),
    ..Default::default()
}
```

| Field            | Type            | Default     | Description                                                                                                             |
| ---------------- | --------------- | ----------- | ----------------------------------------------------------------------------------------------------------------------- |
| `max_concurrent` | `Option<usize>` | `None` (10) | Maximum number of simultaneous HTTP requests. A tokio `Semaphore` enforces this limit across all in-flight fetch tasks. |

:::tip
The default of 10 concurrent requests is a good starting point. Lower it when crawling sites with strict rate limits; raise it for high-throughput internal crawls.
:::

A per-domain rate limit also applies: the engine enforces a 200 ms minimum interval between requests to the same domain, and automatically respects `Crawl-delay` directives from robots.txt when `respect_robots_txt` is enabled.

## Domain scoping

Keep the crawl within the seed domain or allow subdomains:

```rust
CrawlConfig {
    stay_on_domain: true,
    allow_subdomains: false, // only exact domain match
    ..Default::default()
}
```

| Field              | Type   | Default | Description                                                                                                                                             |
| ------------------ | ------ | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `stay_on_domain`   | `bool` | `false` | When `true`, only follow links whose host matches the seed URL's host.                                                                                  |
| `allow_subdomains` | `bool` | `false` | When `true` and `stay_on_domain` is `true`, also follow links to subdomains of the seed host (e.g., `blog.example.com` when the seed is `example.com`). |

## Path filtering with regex

Include or exclude URL paths using regex patterns:

```rust
CrawlConfig {
    include_paths: vec![r"^/docs/".to_string(), r"^/blog/".to_string()],
    exclude_paths: vec![r"/admin/".to_string(), r"\.pdf$".to_string()],
    ..Default::default()
}
```

| Field           | Type          | Default | Description                                                                                                                                                                          |
| --------------- | ------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `include_paths` | `Vec<String>` | `[]`    | Regex patterns matched against the URL path. When non-empty, only URLs matching at least one pattern are crawled. The depth-0 seed URL is always included regardless of this filter. |
| `exclude_paths` | `Vec<String>` | `[]`    | Regex patterns matched against the URL path. URLs matching any pattern are skipped. Exclude patterns take priority over include patterns.                                            |

The engine compiles these patterns once at the start of the crawl and validates them during `CrawlConfig::validate()`. Invalid regex patterns produce a `CrawlError::InvalidConfig` error.

## Batch crawling

Crawl multiple seed URLs concurrently, each following links independently:

```rust
use crawlberg::batch_crawl;

let results = batch_crawl(
    &engine,
    vec![
        "https://example.com".to_owned(),
        "https://other-site.org".to_owned(),
    ],
).await?;

for entry in &results.results {
    match (&entry.result, &entry.error) {
        (Some(crawl), _) => println!("{}: {} pages", entry.url, crawl.pages.len()),
        (None, Some(e)) => eprintln!("{}: {}", entry.url, e),
        _ => {}
    }
}
```

`batch_crawl` respects the same `max_concurrent` limit across all seed URLs. It returns `BatchCrawlResults`, an aggregate object with `results`, `total_count`, `completed_count`, and `failed_count`. Each entry in `results` is a `BatchCrawlResult` with either a populated `result` or an `error` message.

There is also `batch_scrape` for scraping multiple individual URLs without link following:

```rust
use crawlberg::batch_scrape;

let results = batch_scrape(
    &engine,
    vec![
        "https://example.com/page1".to_owned(),
        "https://example.com/page2".to_owned(),
    ],
).await?;
```

`batch_scrape` returns `BatchScrapeResults` with the same aggregate shape.

## Redirect handling

The engine resolves redirects before starting the crawl loop, following HTTP 3xx redirects, `Refresh` headers, and `<meta http-equiv="refresh">` tags. Redirect loops and excessive redirects are detected and reported.

| Field           | Type    | Default | Description                                                                      |
| --------------- | ------- | ------- | -------------------------------------------------------------------------------- |
| `max_redirects` | `usize` | `10`    | Maximum number of redirects to follow before reporting an error. Must be <= 100. |

## CrawlResult reference

| Field            | Type                   | Description                                                            |
| ---------------- | ---------------------- | ---------------------------------------------------------------------- |
| `pages`          | `Vec<CrawlPageResult>` | All successfully crawled pages.                                        |
| `final_url`      | `String`               | The URL after resolving initial redirects from the seed.               |
| `redirect_count` | `usize`                | Number of redirects followed during initial resolution.                |
| `was_skipped`    | `bool`                 | Whether any page was skipped (binary or PDF content).                  |
| `error`          | `Option<String>`       | Error message if the crawl encountered a fatal issue.                  |
| `cookies`        | `Vec<CookieInfo>`      | Cookies collected during the crawl (when `cookies_enabled` is `true`). |

## CrawlPageResult reference

Each page in the crawl result contains:

| Field              | Type                     | Description                                                   |
| ------------------ | ------------------------ | ------------------------------------------------------------- |
| `url`              | `String`                 | The original fetched URL.                                     |
| `normalized_url`   | `String`                 | URL after normalization (for deduplication).                  |
| `status_code`      | `u16`                    | HTTP response status code.                                    |
| `content_type`     | `String`                 | The Content-Type header value.                                |
| `html`             | `String`                 | The response body.                                            |
| `body_size`        | `usize`                  | Size of the response body in bytes.                           |
| `metadata`         | `PageMetadata`           | Extracted metadata (title, description, OG tags, etc.).       |
| `links`            | `Vec<LinkInfo>`          | Links found on the page.                                      |
| `images`           | `Vec<ImageInfo>`         | Images found on the page.                                     |
| `feeds`            | `Vec<FeedInfo>`          | RSS/Atom/JSON feed links.                                     |
| `json_ld`          | `Vec<JsonLdEntry>`       | JSON-LD structured data entries.                              |
| `depth`            | `usize`                  | Distance from the seed URL in link hops.                      |
| `stayed_on_domain` | `bool`                   | Whether this page is on the same domain as the seed.          |
| `markdown`         | `Option<MarkdownResult>` | Markdown conversion (always populated for HTML pages).        |
| `extracted_data`   | `Option<Value>`          | LLM-extracted structured data, when extraction is configured. |
| `extraction_meta`  | `Option<ExtractionMeta>` | LLM extraction cost and token metadata.                       |
