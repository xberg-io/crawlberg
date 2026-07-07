---
title: "MCP Server"
---

Crawlberg exposes its crawling capabilities as an
[MCP (Model Context Protocol)](https://modelcontextprotocol.io/) server, allowing AI
assistants to scrape, crawl, and map websites through tool calls. The MCP feature is
gated behind `mcp`.

## Starting the MCP server

### CLI

```bash
crawlberg mcp
```

The server starts on **stdio transport** -- it reads JSON-RPC messages from stdin and
writes responses to stdout. Diagnostic messages go to stderr.

### Programmatic

```rust
use crawlberg::mcp::start_mcp_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    start_mcp_server().await?;
    Ok(())
}
```

With custom configuration:

```rust
use crawlberg::mcp::start_mcp_server_with_config;
use crawlberg::CrawlConfig;

let config = CrawlConfig {
    respect_robots_txt: true,
    stay_on_domain: true,
    ..Default::default()
};

start_mcp_server_with_config(config).await?;
```

### Streamable HTTP transport

Run the REST API server with the MCP server mounted over Streamable HTTP:

```bash
crawlberg serve --host 127.0.0.1 --port 3000
```

The MCP server is available at `http://127.0.0.1:3000/mcp`. Connect an MCP client to this endpoint.

:::note
The binary must be built with both `api` and `mcp` features. The distributed CLI binary includes both.
:::

The HTTP transport is mounted outside the REST API middleware stack, so request-timeout and compression layers do not interfere with MCP Server-Sent Events.

#### Example: connect an HTTP-capable MCP client

MCP clients that support the Streamable HTTP transport take the endpoint URL directly. A typical client config entry:

```json
{
  "mcpServers": {
    "crawlberg-http": {
      "url": "http://127.0.0.1:3000/mcp"
    }
  }
}
```

#### Programmatic Rust usage

Use `streamable_http_service()` to create a Streamable HTTP server type:

```rust
use crawlberg::{streamable_http_service, CrawlConfig};

let config = CrawlConfig::default();
let service = streamable_http_service(config);
// Mount the service to your HTTP server at `/mcp`
```

## Available tools

The server exposes nine tools for web scraping, crawling, and site mapping.

### `scrape`

Scrape a single URL and extract content as markdown or JSON.

| Parameter     | Type    | Required | Description                                           |
| ------------- | ------- | -------- | ----------------------------------------------------- |
| `url`         | string  | yes      | URL to scrape (http/https only).                      |
| `format`      | string  | no       | `"markdown"` (default) or `"json"`.                   |
| `use_browser` | boolean | no       | Force browser rendering (requires `browser` feature). |

Example:

```json
{
  "url": "https://example.com",
  "format": "markdown"
}
```

#### `crawl`

Crawl a website following links up to a configured depth.

| Parameter        | Type    | Required | Description                           |
| ---------------- | ------- | -------- | ------------------------------------- |
| `url`            | string  | yes      | Starting URL.                         |
| `max_depth`      | integer | no       | Maximum link depth (max 100).         |
| `max_pages`      | integer | no       | Maximum pages to crawl (1--100,000).  |
| `format`         | string  | no       | `"markdown"` (default) or `"json"`.   |
| `stay_on_domain` | boolean | no       | Restrict crawling to the same domain. |

Example:

```json
{
  "url": "https://docs.rs/tokio",
  "max_depth": 2,
  "max_pages": 50,
  "stay_on_domain": true
}
```

#### `map`

Discover all pages on a website via links and sitemaps.

| Parameter            | Type    | Required | Description                               |
| -------------------- | ------- | -------- | ----------------------------------------- |
| `url`                | string  | yes      | Website URL.                              |
| `limit`              | integer | no       | Maximum URLs to return.                   |
| `search`             | string  | no       | Case-insensitive substring filter.        |
| `respect_robots_txt` | boolean | no       | Whether to respect robots.txt directives. |
| `format`             | string  | no       | `"markdown"` (default) or `"json"`.       |

Example:

```json
{
  "url": "https://example.com",
  "limit": 200,
  "search": "api"
}
```

#### `batch_scrape`

Scrape multiple URLs concurrently.

| Parameter     | Type             | Required | Description                         |
| ------------- | ---------------- | -------- | ----------------------------------- |
| `urls`        | array of strings | yes      | URLs to scrape (must not be empty). |
| `format`      | string           | no       | `"markdown"` (default) or `"json"`. |
| `concurrency` | integer          | no       | Maximum concurrent requests.        |

Example:

```json
{
  "urls": ["https://example.com", "https://example.org"],
  "format": "json",
  "concurrency": 5
}
```

#### `batch_crawl`

Crawl multiple seed URLs concurrently.

| Parameter        | Type             | Required | Description                           |
| ---------------- | ---------------- | -------- | ------------------------------------- |
| `urls`           | array of strings | yes      | Seed URLs to crawl (must not be empty). |
| `max_depth`      | integer          | no       | Maximum link depth (max 100).         |
| `max_pages`      | integer          | no       | Maximum pages to crawl (1--100,000).  |
| `format`         | string           | no       | `"markdown"` (default) or `"json"`.   |
| `stay_on_domain` | boolean          | no       | Restrict crawling to the same domain. |
| `concurrency`    | integer          | no       | Maximum concurrent seed crawls.      |

#### `download`

Download a document from a URL and return metadata.

| Parameter  | Type    | Required | Description                     |
| ---------- | ------- | -------- | ------------------------------- |
| `url`      | string  | yes      | Document URL.                   |
| `max_size` | integer | no       | Maximum document size in bytes. |

Returns JSON with `url`, `mime_type`, `size`, `filename`, and `content_hash` for
documents, or page metadata if the URL returns HTML.

#### `interact`

Execute browser actions on a page and return per-action results plus the final DOM.

| Parameter | Type             | Required | Description                                  |
| --------- | ---------------- | -------- | -------------------------------------------- |
| `url`     | string           | yes      | URL to navigate to before executing actions  |
| `actions` | array of objects | yes      | Page actions such as click, wait, and scrape |

#### `generate_citations`

Convert markdown links to numbered citations.

| Parameter  | Type   | Required | Description                       |
| ---------- | ------ | -------- | --------------------------------- |
| `markdown` | string | yes      | Markdown text with inline links. |

Returns markdown with inline `[link](url)` syntax converted to `[1]` with
`[1]: url` references at the end.

#### `get_version`

Return the crawlberg library version. Takes no parameters.

Example response:

```json
{
  "version": "0.3.0"
}
```

## Integration with AI assistants

### Claude Desktop / Claude Code

Add to your MCP configuration (`claude_desktop_config.json` or `.mcp.json`):

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

### Cursor

In Cursor settings, add an MCP server:

- **Name:** crawlberg
- **Command:** `crawlberg mcp`
- **Transport:** stdio

### Windsurf

Add to your Windsurf MCP configuration:

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

### Docker

```json
{
  "mcpServers": {
    "crawlberg": {
      "command": "docker",
      "args": ["run", "-i", "--rm", "crawlberg:latest", "mcp"]
    }
  }
}
```

## Transport

The MCP server uses **stdio transport** exclusively. It reads newline-delimited JSON-RPC
messages from stdin and writes responses to stdout. This is the standard transport for
local MCP servers.

The server announces itself with:

- **Name:** `crawlberg-mcp`
- **Version:** The crate version from `Cargo.toml`
- **Title:** Crawlberg Web Crawling MCP Server
- **Capabilities:** Tools

The server instructions tell clients:

> Scrape, crawl, and map websites. Use 'scrape' for single pages, 'crawl' for following
> links across a site, 'map' for discovering all URLs, and 'batch_scrape' for processing
> multiple URLs concurrently. Use format: 'json' for structured output or 'markdown'
> (default) for human-readable content.

## Output formats

All content-returning tools support two output formats:

- **markdown** (default) -- Human-readable output with page titles, metadata summary,
  and markdown-converted content. Ideal for AI assistant consumption.
- **json** -- Pretty-printed JSON with all fields from the `ScrapeResult` or `CrawlResult`
  structs. Use when you need structured data for programmatic processing.

## Error handling

Tool errors are mapped to MCP error responses:

| Crawl error                          | MCP error                                        |
| ------------------------------------ | ------------------------------------------------ |
| Invalid URL                          | `invalid_params`                                 |
| Invalid config (bad max_depth, etc.) | `invalid_params`                                 |
| WAF blocked                          | `internal_error` (Blocked by WAF/bot protection) |
| Timeout                              | `internal_error` (Request timed out)             |
| Network error                        | `internal_error`                                 |

All errors include descriptive messages to help the AI assistant understand what went wrong.
