---
title: "MCP Tools Reference"
---

Crawlberg implements a [Model Context Protocol](https://modelcontextprotocol.io/) (MCP) server that exposes web scraping, crawling, and mapping capabilities as tools for LLM agents. The server is available over stdio transport (via `crawlberg mcp`) or Streamable HTTP at `/mcp` when the REST API server runs (`crawlberg serve`). Both transports expose the same nine tools; connect your MCP client to either endpoint.

## Server Info

| Field        | Value                              |
| ------------ | ---------------------------------- |
| Name         | `crawlberg-mcp`                   |
| Title        | Crawlberg Web Crawling MCP Server |
| Transport    | stdio or Streamable HTTP at `/mcp` |
| Capabilities | Tools                              |

## Tools

### scrape

Scrape a single URL and extract content as markdown or JSON.

**Annotations:** `read_only_hint = true`, `idempotent_hint = true`

**Parameters:**

| Parameter     | Type      | Required | Default      | Description                                                                |
| ------------- | --------- | -------- | ------------ | -------------------------------------------------------------------------- |
| `url`         | `string`  | Yes      | --           | URL to scrape (must start with `http://` or `https://`)                    |
| `format`      | `string`  | No       | `"markdown"` | Output format: `"markdown"` or `"json"`                                    |
| `use_browser` | `boolean` | No       | `false`      | Force browser rendering instead of HTTP fetch (requires `browser` feature) |

**Returns:** Text content containing the page content in the requested format. Markdown format includes the converted page content. JSON format includes the full `ScrapeResult` as structured JSON.

---

### crawl

Crawl a website following links up to a configured depth.

**Annotations:** `read_only_hint = true`

**Parameters:**

| Parameter        | Type      | Required | Default        | Description                                      |
| ---------------- | --------- | -------- | -------------- | ------------------------------------------------ |
| `url`            | `string`  | Yes      | --             | Starting URL for the crawl                       |
| `max_depth`      | `integer` | No       | Engine default | Maximum link depth from the start URL (max: 100) |
| `max_pages`      | `integer` | No       | Engine default | Maximum number of pages to crawl (1--100,000)    |
| `format`         | `string`  | No       | `"markdown"`   | Output format: `"markdown"` or `"json"`          |
| `stay_on_domain` | `boolean` | No       | Engine default | Restrict crawling to the same domain             |

**Returns:** Text content with results for all discovered pages. In markdown format, each page is separated by `---`. In JSON format, each page is a structured object.

**Validation:**

- `max_depth` must be <= 100
- `max_pages` must be between 1 and 100,000

---

### map

Discover all pages on a website via links and sitemaps.

**Annotations:** `read_only_hint = true`, `idempotent_hint = true`

**Parameters:**

| Parameter            | Type      | Required | Default        | Description                                           |
| -------------------- | --------- | -------- | -------------- | ----------------------------------------------------- |
| `url`                | `string`  | Yes      | --             | URL of the website to map                             |
| `limit`              | `integer` | No       | No limit       | Maximum number of URLs to return                      |
| `search`             | `string`  | No       | --             | Case-insensitive substring filter for discovered URLs |
| `respect_robots_txt` | `boolean` | No       | Engine default | Whether to respect robots.txt directives              |

**Returns:** Text content with the list of discovered URLs and their sitemap metadata (last modified, change frequency, priority).

---

### batch_scrape

Scrape multiple URLs concurrently.

**Annotations:** `read_only_hint = true`

**Parameters:**

| Parameter     | Type       | Required | Default        | Description                                |
| ------------- | ---------- | -------- | -------------- | ------------------------------------------ |
| `urls`        | `string[]` | Yes      | --             | List of URLs to scrape (must not be empty) |
| `format`      | `string`   | No       | `"markdown"`   | Output format: `"markdown"` or `"json"`    |
| `concurrency` | `integer`  | No       | Engine default | Maximum number of concurrent requests      |

**Returns:** Text content with results for each URL, separated by `---`. Failed URLs include an error message.

---

### batch_crawl

Crawl multiple seed URLs concurrently.

**Annotations:** `read_only_hint = true`

**Parameters:**

| Parameter        | Type       | Required | Default        | Description                                  |
| ---------------- | ---------- | -------- | -------------- | -------------------------------------------- |
| `urls`           | `string[]` | Yes      | --             | Seed URLs to crawl (must not be empty)       |
| `max_depth`      | `integer`  | No       | Engine default | Maximum link depth from each seed (max: 100) |
| `max_pages`      | `integer`  | No       | Engine default | Maximum pages to crawl per seed (1--100,000) |
| `format`         | `string`   | No       | `"markdown"`   | Output format: `"markdown"` or `"json"`      |
| `stay_on_domain` | `boolean`  | No       | Engine default | Restrict crawling to same domain per seed    |
| `concurrency`    | `integer`  | No       | Engine default | Maximum number of concurrent seed crawls     |

**Returns:** Text content with results for all discovered pages across all seeds.

---

### download

Download a document from a URL.

**Annotations:** `read_only_hint = true`

**Parameters:**

| Parameter  | Type      | Required | Default | Description                    |
| ---------- | --------- | -------- | ------- | ------------------------------ |
| `url`      | `string`  | Yes      | --      | URL to download                |
| `max_size` | `integer` | No       | 50 MB   | Maximum document size in bytes |

**Returns:** JSON text with document metadata:

```json
{
  "url": "https://example.com/doc.pdf",
  "mime_type": "application/pdf",
  "size": 102400,
  "filename": "doc.pdf",
  "content_hash": "abc123..."
}
```

If the URL returns HTML instead of a downloadable document, the response includes a `note` field explaining this.

---

### interact

Execute browser actions on a page.

**Parameters:**

| Parameter | Type       | Required | Description                                 |
| --------- | ---------- | -------- | ------------------------------------------- |
| `url`     | `string`   | Yes      | URL to navigate to before executing actions |
| `actions` | `object[]` | Yes      | Sequence of page actions                    |

**Returns:** JSON text containing the `InteractionResult`, including per-action results, final HTML, and final URL. Browser backend availability determines runtime behavior.

---

### generate_citations

Convert markdown links to numbered citations.

**Annotations:** `read_only_hint = true`, `idempotent_hint = true`

**Parameters:**

| Parameter  | Type     | Required | Description                       |
| ---------- | -------- | -------- | --------------------------------- |
| `markdown` | `string` | Yes      | Markdown text with inline links.  |

**Returns:** Markdown text with inline `[link](url)` syntax converted to `[1]` with `[1]: url` references appended.

---

### get_version

Get the current crawlberg library version.

**Annotations:** `read_only_hint = true`, `idempotent_hint = true`

**Parameters:** None (empty object `{}`).

**Returns:** JSON text:

```json
{
  "version": "0.3.0"
}
```

---

## Error Handling

MCP tool errors are mapped from `CrawlError` variants:

| CrawlError Variant | MCP Error Code            | Description                                                 |
| ------------------ | ------------------------- | ----------------------------------------------------------- |
| `InvalidConfig`    | `-32602` (INVALID_PARAMS) | Invalid configuration parameter                             |
| All other variants | `-32603` (INTERNAL_ERROR) | Network, browser, or server errors with descriptive context |

Error messages preserve the original context to aid debugging.

## Usage with Claude Desktop

Add to your `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "crawlberg": {
      "command": "crawlberg",
      "args": ["mcp"]
    }
  }
}
```
