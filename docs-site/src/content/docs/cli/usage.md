---
title: "CLI Usage"
---

The `crawlberg` CLI provides commands for scraping, crawling, site mapping, and running the API and MCP servers.

```text
crawlberg <COMMAND> [OPTIONS]
```

## Commands

### scrape

Scrape a single URL and extract metadata.

```text
crawlberg scrape <URL> [OPTIONS]
```

**Arguments:**

| Argument | Required | Description   |
| -------- | -------- | ------------- |
| `URL`    | Yes      | URL to scrape |

**Options:**

| Flag                          | Default | Description                                    |
| ----------------------------- | ------- | ---------------------------------------------- |
| `--format <FORMAT>`           | `json`  | Output format: `json` or `markdown`            |
| `--proxy <URL>`               | --      | Proxy URL                                      |
| `--user-agent <STRING>`       | --      | Custom user agent                              |
| `--timeout <MS>`              | `30000` | Request timeout in milliseconds                |
| `--respect-robots-txt`        | `false` | Respect robots.txt                             |
| `--browser-mode <MODE>`       | `auto`  | Browser mode: `auto`, `always`, or `never`     |
| `--browser-endpoint <WS_URL>` | --      | CDP WebSocket endpoint for an external browser |

**Examples:**

```bash
# Scrape a page as JSON (default)
crawlberg scrape https://example.com

# Scrape as markdown
crawlberg scrape https://example.com --format markdown

# Scrape through a proxy with custom timeout
crawlberg scrape https://example.com --proxy http://proxy:8080 --timeout 60000

# Force browser rendering for a JS-heavy page
crawlberg scrape https://quotes.toscrape.com/js/ --browser-mode always --format markdown

# Connect to an external browser via CDP
crawlberg scrape https://example.com --browser-endpoint ws://127.0.0.1:9222/devtools/browser/...
```

**Output:**

- `json` format: Prints the full `ScrapeResult` as pretty-printed JSON to stdout.
- `markdown` format: Prints the markdown content to stdout. If no markdown content is available, prints an error to stderr.

---

### crawl

Crawl a website following links.

```text
crawlberg crawl <URL>... [OPTIONS]
```

**Arguments:**

| Argument | Required          | Description                                              |
| -------- | ----------------- | -------------------------------------------------------- |
| `URL`    | Yes (one or more) | Seed URL(s) to crawl. Multiple URLs triggers batch mode. |

**Options:**

| Flag                          | Short | Default | Description                                       |
| ----------------------------- | ----- | ------- | ------------------------------------------------- |
| `--depth <N>`                 | `-d`  | `2`     | Maximum crawl depth                               |
| `--max-pages <N>`             | `-n`  | --      | Maximum pages to crawl                            |
| `--concurrent <N>`            | `-c`  | `10`    | Maximum concurrent requests                       |
| `--rate-limit <MS>`           | --    | `200`   | Rate limit delay between requests per domain (ms) |
| `--format <FORMAT>`           | --    | `json`  | Output format: `json` or `markdown`               |
| `--proxy <URL>`               | --    | --      | Proxy URL                                         |
| `--user-agent <STRING>`       | --    | --      | Custom user agent                                 |
| `--timeout <MS>`              | --    | `30000` | Request timeout in milliseconds                   |
| `--respect-robots-txt`        | --    | `false` | Respect robots.txt                                |
| `--stay-on-domain`            | --    | `false` | Stay on the same domain                           |
| `--browser-mode <MODE>`       | --    | `auto`  | Browser mode: `auto`, `always`, or `never`        |
| `--browser-endpoint <WS_URL>` | --    | --      | CDP WebSocket endpoint for an external browser    |

**Examples:**

```bash
# Crawl with default settings (depth 2, 10 concurrent)
crawlberg crawl https://example.com

# Crawl deeper with more concurrency
crawlberg crawl https://example.com -d 5 -c 20 --max-pages 500

# Crawl and output as markdown
crawlberg crawl https://example.com --format markdown --stay-on-domain

# Batch crawl multiple seed URLs
crawlberg crawl https://example.com https://example.org -d 1

# Force browser rendering during crawl
crawlberg crawl https://quotes.toscrape.com/js/ --browser-mode always --format markdown
```

**Output:**

- Single URL, `json` format: Prints the `CrawlResult` as pretty-printed JSON.
- Single URL, `markdown` format: Prints each page separated by `---` with URL header.
- Multiple URLs, `json` format: Prints an array of `{ seed_url, result }` objects.
- Multiple URLs, `markdown` format: Prints each page with seed URL and page URL headers.

---

### map

Discover all URLs on a website via sitemaps and link extraction.

```text
crawlberg map <URL> [OPTIONS]
```

**Arguments:**

| Argument | Required | Description |
| -------- | -------- | ----------- |
| `URL`    | Yes      | URL to map  |

**Options:**

| Flag                   | Default | Description              |
| ---------------------- | ------- | ------------------------ |
| `--limit <N>`          | --      | Maximum URLs to return   |
| `--search <STRING>`    | --      | Filter URLs by substring |
| `--respect-robots-txt` | `false` | Respect robots.txt       |

**Examples:**

```bash
# Discover all URLs
crawlberg map https://example.com

# Limit results and filter
crawlberg map https://example.com --limit 50 --search "/docs/"
```

**Output:** Prints one URL per line to stdout.

---

### serve

Start the REST API server. Requires the `api` feature.

```text
crawlberg serve [OPTIONS]
```

**Options:**

| Flag               | Default   | Description             |
| ------------------ | --------- | ----------------------- |
| `--host <ADDRESS>` | `0.0.0.0` | Host address to bind to |
| `--port <PORT>`    | `3000`    | Port to listen on       |

**Examples:**

```bash
# Start on default port
crawlberg serve

# Start on custom host and port
crawlberg serve --host 127.0.0.1 --port 8080
```

The server prints a startup message to stderr and runs until interrupted.

---

### mcp

Start the MCP server using stdio transport. Requires the `mcp` feature.

```text
crawlberg mcp
```

No options. The server communicates via stdin/stdout using the MCP protocol. Startup messages are printed to stderr.

**Usage with Claude Desktop:**

Add to `claude_desktop_config.json`:

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

---

## Exit Codes

| Code | Meaning                                                  |
| ---- | -------------------------------------------------------- |
| `0`  | Success                                                  |
| `1`  | Error (scrape/crawl/map failure, server startup failure) |

Errors are printed to stderr. Successful output goes to stdout.
