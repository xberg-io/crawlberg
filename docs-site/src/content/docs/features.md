---
title: Features
description: Feature breakdown for Crawlberg 0.3
---

## Features

Crawlberg is a Rust-native web crawling engine. Every feature below is wired through the public surface of the Rust crate (`pub use` from the crate root), the REST/MCP/CLI server surfaces, or the generated binding surface unless explicitly marked as internal.

Rust uses the core crate. Generated packages cover Python, TypeScript (Node + WebAssembly), Go, Java, Kotlin Android, C#, Ruby, PHP, Elixir, Dart, Swift, Zig, and C FFI.

The public wrapper surface from `crawlberg::*` includes `create_engine`, `scrape`, `crawl`, `map_urls`, `interact`, `batch_scrape`, `batch_crawl`, and `generate_citations`. Native targets also expose `crawl_stream` / `batch_crawl_stream`; `serve_api` and `start_mcp_server` are gated by the `api` and `mcp` cargo features.

### Version Labels

| Version | Features introduced by that line |
| ------- | -------------------------------- |
| <span class="version-badge">v0.1</span> | Core `scrape` / `crawl` / `map_urls`, REST API, MCP server, Docker image, and CLI. |
| <span class="version-badge">v0.2</span> | `ContentConfig`, `output_format`, plain text / Djot output, selector filtering, and charset reporting. |
| <span class="version-badge">v0.3</span> | Native browser backend, `BrowserBackend`, `BrowserExtras`, network capture, C / Dart / Swift / Zig / Kotlin Android packages, aggregate batch wrappers, SSRF policy, dispatch/WAF hot reload, EWMA state, `interact`, `PageAction`, and streaming crawl APIs. |

---

### Core Crawling

| Feature                 | Description                                                                                               |
| ----------------------- | --------------------------------------------------------------------------------------------------------- |
| **Engine construction** <span class="version-badge">v0.1</span> | `create_engine(config)` builds an opaque `CrawlEngineHandle`; structural config validation runs before the engine is returned. |
| **Concurrent fetching** <span class="version-badge">v0.1</span> | Tokio `JoinSet` + `Semaphore` for parallel requests, bounded by `CrawlConfig::max_concurrent`. |
| **Sequential crawl** <span class="version-badge">v0.1</span> | `crawl(&engine, url)` follows links from a seed up to `max_depth` / `max_pages`. |
| **Batch operations** <span class="version-badge">v0.3</span> | `batch_crawl(&engine, urls)` and `batch_scrape(&engine, urls)` return aggregate `Batch*Results` structs with per-URL results and counts. |
| **URL discovery** <span class="version-badge">v0.1</span> | `map_urls(&engine, url)` returns a `MapResult` from sitemap parsing and link extraction. |
| **Streaming crawl** <span class="version-badge">v0.3</span> | Native targets expose `crawl_stream` and `batch_crawl_stream` event APIs. |
| **Page interaction** <span class="version-badge">v0.3</span> | `interact(&engine, url, actions)` runs validated `PageAction` values on one browser page. |
| **Redirect handling** <span class="version-badge">v0.1</span> | HTTP 3xx, `Refresh` header, and meta-refresh detection with loop detection (`max_redirects`, default 10). |

Four traversal strategies — BFS (default), DFS, BestFirst, and Adaptive — are implemented internally; the public surface always uses BFS. A configuration knob for strategy selection is on the roadmap.

---

### Metadata Extraction

Every scraped page yields a `PageMetadata` struct alongside separate collections for links, images, feeds, and structured data.

| Feature               | Description                                                              |
| --------------------- | ------------------------------------------------------------------------ |
| **Open Graph**        | `og:title`, `og:description`, `og:image`, `og:type`, `og:url`, and more. |
| **Twitter Card**      | `twitter:card`, `twitter:title`, `twitter:description`, `twitter:image`. |
| **Dublin Core**       | `dc.title`, `dc.creator`, `dc.date`, `dc.subject`.                       |
| **Article metadata**  | `article:published_time`, `article:author`, `article:section`.           |
| **JSON-LD**           | Full extraction from `<script type="application/ld+json">` blocks.       |
| **Link extraction**   | 4 categories: `Internal`, `External`, `Anchor`, `Document`.              |
| **Image extraction**  | `<img>`, `<picture>`, `og:image`, and `srcset` sources.                  |
| **Feed discovery**    | RSS, Atom, and JSON Feed `<link>` elements.                              |
| **Favicons**          | Extraction and canonicalisation of site icons.                           |
| **hreflang**          | Language and region variants for internationalised pages.                |
| **Headings**          | H1–H6 with hierarchy preservation.                                       |
| **Response metadata** | HTTP headers, content type, charset detection, body size.                |

---

### Markdown Conversion

HTML→Markdown conversion runs automatically on every page via [html-to-markdown](https://docs.html-to-markdown.xberg.io). Results land in the `MarkdownResult` struct attached to every page.

| Feature                  | Description                                                                                          |
| ------------------------ | ---------------------------------------------------------------------------------------------------- |
| **Always-on conversion** <span class="version-badge">v0.1</span> | Every HTML page result includes a `markdown` field with converted content. |
| **Output formats** <span class="version-badge">v0.2</span> | `content.output_format` accepts `"markdown"`, `"plain"`, or `"djot"`. |
| **Document structure** <span class="version-badge">v0.1</span> | Optional structured tree of semantic nodes alongside the rendered Markdown. |
| **Table extraction** <span class="version-badge">v0.1</span> | Structured table data preserved alongside Markdown output. |
| **Link-to-citations** <span class="version-badge">v0.3</span> | `MarkdownResult.citations` is `bool`; call `generate_citations(markdown)` for `CitationResult.references`. |
| **Fit Markdown** <span class="version-badge">v0.1</span> | Heuristic-based pruning and truncation optimised for LLM consumption (`MarkdownResult.fit_content`). |
| **Warnings** <span class="version-badge">v0.1</span> | Non-fatal processing warnings surfaced in `MarkdownResult.warnings`. |

---

### Browser Fallback

:::note[Feature gates]
`browser` enables the Chromiumoxide CDP backend. `browser-native` enables the in-process native backend. `interact` is a compatibility alias for browser-backed interaction.
:::

| Feature                 | Description                                                                                                                                 |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| **BrowserBackend** <span class="version-badge">v0.3</span> | `BrowserBackend::Chromiumoxide` uses Chrome/CDP; `BrowserBackend::Native` uses the in-process native backend. |
| **WAF detection** <span class="version-badge">v0.3</span> | Built-in TOML rules classify block signals and return the vendor when a fingerprint matches. |
| **Auto fallback** <span class="version-badge">v0.1</span> | In `Auto` mode, WAF-blocked or JS-render-required responses can retry through the configured browser backend. |
| **Wait strategies** <span class="version-badge">v0.1</span> | `NetworkIdle` (default), `Selector`, `Fixed`; plus optional `extra_wait` after the wait condition. |
| **CDP endpoint** <span class="version-badge">v0.1</span> | Point the Chromiumoxide backend at an already-running browser via `BrowserConfig::endpoint`. |
| **Native extras** <span class="version-badge">v0.3</span> | Native backend results can populate `BrowserExtras` with `eval_result`, `network_events`, and cookies. |
| **Persistent profiles** <span class="version-badge">v0.3</span> | Named profiles via `CrawlConfig::browser_profile` / `save_browser_profile`. Profile names are path-traversal checked. |
| **Screenshot capture** <span class="version-badge">v0.1</span> | Screenshot bytes are captured when `capture_screenshot` is enabled and the browser is used. |
| **JS-render detection** <span class="version-badge">v0.1</span> | SPA-shell and noscript-warning heuristics flag pages that need browser rendering. |

---

### Network and Caching

| Feature                      | Description                                                                  |
| ---------------------------- | ---------------------------------------------------------------------------- |
| **Per-domain rate limiting** | Default 200 ms delay per origin; configurable in `CrawlConfig`.              |
| **HTTP caching**             | ETag and Last-Modified conditional requests with an on-disk cache.           |
| **Proxy support**            | HTTP, HTTPS, and SOCKS5 via `ProxyConfig`.                                   |
| **User-Agent rotation**      | Configurable list rotated across requests.                                   |
| **Cookie handling**          | Tracking, deduplication, and persistence across requests.                    |
| **Authentication**           | Basic, Bearer, and custom-header authentication via `AuthConfig`.            |
| **Timeouts**                 | Per-request timeout (default 30 s); `max_redirects` default 10 (cap 100).    |
| **Retry logic**              | Configurable retry count with explicit status-code triggers (`retry_codes`). |
| **SSRF policy** <span class="version-badge">v0.3</span> | `SsrfPolicy` rejects private networks and unsupported schemes by default, with Rust-side override hooks. |
| **Dispatch state** <span class="version-badge">v0.3</span> | Dispatch profiles, WAF classifiers, hot-reloadable rules, and EWMA domain state drive retry/escalation policy. |
| **Body-size limits**         | Optional `CrawlConfig::max_body_size` to cap response payloads.              |

---

### Content Processing

| Feature                   | Description                                                                                                            |
| ------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| **Preprocessing presets** <span class="version-badge">v0.2</span> | `content.preprocessing_preset` accepts `"minimal"`, `"standard"` (default), or `"aggressive"`. |
| **Output format** <span class="version-badge">v0.2</span> | `content.output_format` selects Markdown, plain text, or Djot. |
| **Selector filtering** <span class="version-badge">v0.2</span> | `content.exclude_selectors`, `strip_tags`, and `preserve_tags` control conversion-time filtering. |
| **Charset reporting** <span class="version-badge">v0.2</span> | `detected_charset` records the charset detected from headers or HTML metadata. |
| **Tag removal** | `remove_tags` takes CSS selectors stripped before extraction. |
| **Path filtering** | `include_paths` and `exclude_paths` accept regex patterns; excludes take priority. |
| **Domain scoping** | `stay_on_domain` with optional `allow_subdomains`. |

---

### Document Downloads

| Feature                 | Description                                                                                |
| ----------------------- | ------------------------------------------------------------------------------------------ |
| **Non-HTML documents**  | Download PDFs, DOCX, images, and code files via `download_documents` (enabled by default). |
| **Asset downloads**     | CSS, JS, images via `download_assets` with `asset_types` category filtering.               |
| **Size limits**         | `document_max_size` (default 50 MB) and `max_asset_size` caps.                             |
| **MIME filtering**      | `document_mime_types` allowlist for permitted document types.                              |
| **Content hashing**     | SHA-256 digest computed for every downloaded document.                                     |
| **Filename extraction** | Parsed from `Content-Disposition` or the URL path.                                         |

---

### Compliance and Standards

| Feature                | Description                                                                   |
| ---------------------- | ----------------------------------------------------------------------------- |
| **robots.txt**         | RFC 9309 compliant with user-agent prefix matching and `Crawl-delay` support. |
| **Sitemap parsing**    | XML, gzip-compressed, and sitemap-index files.                                |
| **noindex / nofollow** | Detection of `<meta>` robots directives and `X-Robots-Tag` headers.           |
| **Charset detection**  | Automatic from HTTP headers and HTML meta tags.                               |
| **Config validation**  | `serde` with `deny_unknown_fields` — typos in config keys fail at parse time. |

---

### WARC Output

:::note[Feature gate]
Requires the `warc` feature: `crawlberg = { version = "0.3", features = ["warc"] }`
:::

| Feature           | Description                                                                                          |
| ----------------- | ---------------------------------------------------------------------------------------------------- |
| **WARC 1.1**      | Standards-compliant `warcinfo` + per-page `response` records, written to `CrawlConfig::warc_output`. |
| **Header safety** | Header names and values validated against CR/LF injection before being written.                      |

---

### REST API, MCP, and CLI

| Feature        | Description                                                                                                                                                         |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **REST API** <span class="version-badge">v0.1</span> | `serve_api(config).await` starts an Axum-based, [Firecrawl v1-compatible](/reference/rest-api/) HTTP API (feature `api`). OpenAPI schema is generated by `utoipa`. |
| **MCP server** <span class="version-badge">v0.1</span> | `start_mcp_server(...)` starts a Model Context Protocol server for AI-agent integration (feature `mcp`). |
| **CLI** <span class="version-badge">v0.1</span> | The `crawlberg` binary exposes `scrape`, `crawl`, `map`, and `serve` subcommands. |

---

### CLI quickstart

```bash
# Scrape a single page
crawlberg scrape https://example.com

# Crawl with depth limiting
crawlberg crawl https://example.com --depth 2 --max-pages 50 --format markdown

# Discover URLs via sitemap and link extraction
crawlberg map https://example.com --respect-robots-txt
```

CLI output formats: `json` (full `CrawlResult` / `ScrapeResult` / `MapResult`) and `markdown` (`MarkdownResult.content`). Citation references are generated by the citation conversion path, not by reading `MarkdownResult.citations` as a reference list.
