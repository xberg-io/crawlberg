---
title: "Configuration"
---

All crawlberg operations are controlled through `CrawlConfig`. This guide covers every public field, its default value, and validation rules.

## Constructing an engine

Pass a `CrawlConfig` to `create_engine`, then call `scrape` / `crawl` / `map_urls` / `batch_scrape` / `batch_crawl` against the returned handle:

```rust
use crawlberg::{CrawlConfig, create_engine};

let engine = create_engine(Some(CrawlConfig {
    max_depth: Some(3),
    max_pages: Some(100),
    max_concurrent: Some(5),
    stay_on_domain: true,
    respect_robots_txt: true,
    ..Default::default()
}))?;
```

`create_engine` runs structural validation up front: depth / page / size bounds, regex paths, browser endpoint rules, and field-level constraints. URL-specific checks such as SSRF policy, redirects, robots, and network failures run per request.

## Convenience constructors

For most use cases, `create_engine` plus one of the top-level async functions is all you need:

```rust
use crawlberg::{CrawlConfig, create_engine, scrape, crawl, map_urls};

let engine = create_engine(Some(CrawlConfig {
    max_depth: Some(2),
    ..Default::default()
}))?;

let scrape_result = scrape(&engine, "https://example.com").await?;
let crawl_result = crawl(&engine, "https://example.com").await?;
let map_result = map_urls(&engine, "https://example.com").await?;
```

Passing `None` to `create_engine` uses `CrawlConfig::default()`.

For scraping or crawling multiple URLs in one go, use the batch variants. Each URL runs concurrently; failures are captured per-URL rather than bubbling up as an error:

```rust
use crawlberg::{create_engine, batch_scrape, batch_crawl};

let engine = create_engine(None)?;

let scrape_results = batch_scrape(&engine, vec![
    "https://example.com".into(),
    "https://example.org".into(),
]).await?;

let crawl_results = batch_crawl(&engine, vec![
    "https://example.com".into(),
    "https://example.org".into(),
]).await?;

// Each aggregate has .results plus .total_count, .completed_count, and .failed_count.
for r in &scrape_results.results {
    if let Some(err) = &r.error {
        eprintln!("{}: {}", r.url, err);
    }
}

println!("batch crawl completed: {}", crawl_results.completed_count);
```

## Config validation

`CrawlConfig::validate()` is called automatically during `CrawlEngineBuilder::build()`. It checks:

| Rule                                                               | Error                                                            |
| ------------------------------------------------------------------ | ---------------------------------------------------------------- |
| `max_concurrent` must not be 0                                     | `"max_concurrent must be > 0"`                                   |
| `max_pages` must not be 0                                          | `"max_pages must be > 0"`                                        |
| `max_redirects` must be <= 100                                     | `"max_redirects must be <= 100"`                                 |
| `request_timeout` must not be zero                                 | `"request_timeout must be > 0"`                                  |
| `browser.wait_selector` required when `browser.wait` is `Selector` | `"browser.wait_selector required when browser.wait is Selector"` |
| `browser.endpoint` must be `ws://` or `wss://`                     | `"browser.endpoint must start with ws:// or wss://"`             |
| `browser.endpoint` cannot be used with `BrowserBackend::Native`    | `"browser.endpoint is only supported by the chromiumoxide backend"` |
| All `include_paths` must be valid regex                            | `"invalid include_path regex '...': ..."`                        |
| All `exclude_paths` must be valid regex                            | `"invalid exclude_path regex '...': ..."`                        |
| All `retry_codes` must be 100-599                                  | `"invalid retry code: ..."`                                      |

You can also call `validate()` manually:

```rust
let config = CrawlConfig {
    max_concurrent: Some(0),
    ..Default::default()
};

match config.validate() {
    Ok(()) => println!("Valid"),
    Err(e) => eprintln!("Invalid: {}", e),
}
```

## Full field reference

### Crawl scope

| Field              | Type            | Default | Description                                                                                   |
| ------------------ | --------------- | ------- | --------------------------------------------------------------------------------------------- |
| `max_depth`        | `Option<usize>` | `None`  | Maximum crawl depth (link hops from seed). `None` means 0 (seed only).                        |
| `max_pages`        | `Option<usize>` | `None`  | Maximum pages to crawl. `None` means unlimited. Must be > 0 if set.                           |
| `stay_on_domain`   | `bool`          | `false` | Restrict crawling to the seed URL's domain.                                                   |
| `allow_subdomains` | `bool`          | `false` | Allow subdomains when `stay_on_domain` is true.                                               |
| `include_paths`    | `Vec<String>`   | `[]`    | Regex patterns -- only matching URL paths are crawled. Seed URL (depth 0) is always included. |
| `exclude_paths`    | `Vec<String>`   | `[]`    | Regex patterns -- matching URL paths are skipped.                                             |

### HTTP client

| Field             | Type                      | Default     | Description                                                               |
| ----------------- | ------------------------- | ----------- | ------------------------------------------------------------------------- |
| `max_concurrent`  | `Option<usize>`           | `None` (10) | Maximum concurrent requests.                                              |
| `request_timeout` | `Duration`                | 30 seconds  | Timeout for individual HTTP requests. Serialized as milliseconds in JSON. |
| `max_redirects`   | `usize`                   | `10`        | Maximum redirects to follow. Must be <= 100.                              |
| `retry_count`     | `usize`                   | `0`         | Number of retry attempts for failed requests.                             |
| `retry_codes`     | `Vec<u16>`                | `[]`        | HTTP status codes that trigger a retry. Each must be 100-599.             |
| `max_body_size`   | `Option<usize>`           | `None`      | Maximum response body size in bytes. Responses are truncated.             |
| `user_agent`      | `Option<String>`          | `None`      | Custom User-Agent string.                                                 |
| `user_agents`     | `Vec<String>`             | `[]`        | User-Agent strings for rotation. When non-empty, overrides `user_agent`.  |
| `custom_headers`  | `HashMap<String, String>` | `{}`        | Extra HTTP headers sent with every request.                               |
| `cookies_enabled` | `bool`                    | `false`     | Whether to collect and track cookies across requests.                     |

### Authentication

| Field  | Type                 | Default | Description                   |
| ------ | -------------------- | ------- | ----------------------------- |
| `auth` | `Option<AuthConfig>` | `None`  | Authentication configuration. |

`AuthConfig` variants:

```rust
// HTTP Basic authentication
AuthConfig::Basic { username: "user".into(), password: "pass".into() }

// Bearer token
AuthConfig::Bearer { token: "your-token".into() }

// Custom header
AuthConfig::Header { name: "X-API-Key".into(), value: "key-value".into() }
```

### Proxy

| Field   | Type                  | Default | Description          |
| ------- | --------------------- | ------- | -------------------- |
| `proxy` | `Option<ProxyConfig>` | `None`  | Static proxy configuration. |

```rust
ProxyConfig {
    url: "http://proxy:8080".to_string(), // or "socks5://proxy:1080"
    username: Some("user".to_string()),
    password: Some("pass".to_string()),
}
```

#### Dynamic proxy rotation (Rust)

**This feature is Rust-only. Language bindings configure the static `proxy` field only.**

For per-request proxy selection or custom rotation logic, implement or compose a `ProxyProvider` and inject it via `CrawlEngineBuilder::with_proxy_provider`. The provider's `next_proxy(&self, host: &str)` method is called per HTTP request with the target host. Returning `None` routes the request directly:

```rust
use std::sync::Arc;
use crawlberg::{ProxyConfig, StaticProxyProvider, CrawlEngineBuilder};

// Baseline: round-robin pool of proxies.
let pool = StaticProxyProvider::new(vec![
    ProxyConfig {
        url: "http://proxy1:8080".into(),
        username: None,
        password: None,
    },
    ProxyConfig {
        url: "http://proxy2:8080".into(),
        username: None,
        password: None,
    },
]);

let engine = CrawlEngineBuilder::new()
    .with_proxy_provider(Arc::new(pool))
    .build()?;
```

When both static `proxy` and an injected provider are set, the provider takes precedence for HTTP fetches. Browser-level proxies (`CrawlConfig::browser::proxy`) still read the static value; provider rotation applies only to the reqwest HTTP path.

### Robots and compliance

| Field                | Type   | Default | Description                                                                   |
| -------------------- | ------ | ------- | ----------------------------------------------------------------------------- |
| `respect_robots_txt` | `bool` | `false` | Fetch and honor robots.txt directives (allow/disallow, crawl-delay, sitemap). |

### Content processing

| Field                          | Type            | Default      | Description                                                                                                       |
| ------------------------------ | --------------- | ------------ | ----------------------------------------------------------------------------------------------------------------- |
| `content.preprocessing_preset` | `String`        | `"standard"` | HTML preprocessing strength: `"minimal"`, `"standard"`, or `"aggressive"` (aggressive strips chrome/boilerplate). |
| `remove_tags`                  | `Vec<String>`   | `[]`         | CSS selectors for elements to remove before processing (e.g., `"nav"`, `".sidebar"`).                             |
| `max_body_size`                | `Option<usize>` | `None`       | Truncate HTML bodies beyond this size in bytes. `None` keeps the full body.                                       |
| `content.output_format`        | `String`        | `"markdown"` | Render converted content as `"markdown"`, `"plain"`, or `"djot"`.                                                |
| `content.strip_tags`           | `Vec<String>`   | `["noscript"]` | Strip tag wrappers but keep their children during conversion.                                                   |
| `content.preserve_tags`        | `Vec<String>`   | `[]`         | Preserve matching HTML tags as raw HTML in the converted output.                                                  |
| `content.exclude_selectors`    | `Vec<String>`   | `[]`         | Drop matching elements and descendants during conversion.                                                         |
| `content.skip_images`          | `bool`          | `false`      | Skip image elements in converted output.                                                                         |
| `content.include_document_structure` | `bool`   | `true`       | Include the structured document tree in `MarkdownResult`.                                                        |

### URL discovery (map)

| Field        | Type             | Default | Description                                        |
| ------------ | ---------------- | ------- | -------------------------------------------------- |
| `map_limit`  | `Option<usize>`  | `None`  | Maximum URLs returned by the map operation.        |
| `map_search` | `Option<String>` | `None`  | Case-insensitive substring filter for map results. |

### Asset downloading

| Field             | Type                 | Default | Description                                                     |
| ----------------- | -------------------- | ------- | --------------------------------------------------------------- |
| `download_assets` | `bool`               | `false` | Download page assets (CSS, JS, images, etc.).                   |
| `asset_types`     | `Vec<AssetCategory>` | `[]`    | Filter for which asset categories to download. Empty means all. |
| `max_asset_size`  | `Option<usize>`      | `None`  | Maximum size per asset download in bytes.                       |

`AssetCategory` options: `Document`, `Image`, `Audio`, `Video`, `Font`, `Stylesheet`, `Script`, `Archive`, `Data`, `Other`.

### Document downloading

| Field                 | Type            | Default | Description                                                                      |
| --------------------- | --------------- | ------- | -------------------------------------------------------------------------------- |
| `download_documents`  | `bool`          | `true`  | Download non-HTML resources (PDF, DOCX, images, code files) instead of skipping. |
| `document_max_size`   | `Option<usize>` | 50 MB   | Maximum document download size in bytes.                                         |
| `document_mime_types` | `Vec<String>`   | `[]`    | MIME type allowlist. Empty uses built-in defaults.                               |

### Browser configuration

| Field                  | Type             | Default   | Description                                    |
| ---------------------- | ---------------- | --------- | ---------------------------------------------- |
| `browser`              | `BrowserConfig`  | See below | Headless browser fallback settings.            |
| `capture_screenshot`   | `bool`           | `false`   | Capture PNG screenshot when browser is used.   |
| `browser_profile`      | `Option<String>` | `None`    | Named browser profile for persistent sessions. |
| `save_browser_profile` | `bool`           | `false`   | Save browser profile changes on exit.          |

#### BrowserConfig fields

| Field                    | Type               | Default       | Description                                                            |
| ------------------------ | ------------------ | ------------- | ---------------------------------------------------------------------- |
| `mode`                   | `BrowserMode`      | `Auto`        | When to use the browser: `Auto`, `Always`, `Never`, or `Stealth`.      |
| `backend`                | `BrowserBackend`   | `Chromiumoxide` | Browser backend: `Chromiumoxide` or `Native`.                        |
| `endpoint`               | `Option<String>`   | `None`        | CDP WebSocket endpoint for an external Chromiumoxide browser instance. |
| `timeout`                | `Duration`         | 30 seconds    | Browser page load timeout.                                             |
| `wait`                   | `BrowserWait`      | `NetworkIdle` | Wait strategy after navigation: `NetworkIdle`, `Selector`, or `Fixed`. |
| `wait_selector`          | `Option<String>`   | `None`        | CSS selector to wait for (required when `wait` is `Selector`).         |
| `extra_wait`             | `Option<Duration>` | `None`        | Additional wait time after the wait condition is met.                  |
| `proxy`                  | `Option<ProxyConfig>` | `None`     | Browser-level HTTP/HTTPS proxy; native backend does not support SOCKS5. |
| `block_url_patterns`     | `Vec<String>`      | `[]`          | Native-backend URL block patterns.                                     |
| `eval_script`            | `Option<String>`   | `None`        | Script evaluated after navigation; native scrape stores the result.    |
| `robots_user_agent`      | `Option<String>`   | `None`        | Native backend user-agent for robots.txt fetches.                      |
| `capture_network_events` | `bool`             | `false`       | Native backend network-event capture into `BrowserExtras`.             |
| `session_affinity`       | `bool`             | `true`        | Reuse same-domain browser sessions when supported.                     |

### WARC output

| Field         | Type              | Default | Description                                             |
| ------------- | ----------------- | ------- | ------------------------------------------------------- |
| `warc_output` | `Option<PathBuf>` | `None`  | Path to write WARC output. `None` disables WARC output. |

## Serialization

`CrawlConfig` implements `Serialize` and `Deserialize` with `#[serde(deny_unknown_fields)]`. Duration fields are serialized as milliseconds:

```json
{
  "max_depth": 3,
  "max_pages": 100,
  "max_concurrent": 5,
  "stay_on_domain": true,
  "respect_robots_txt": true,
  "request_timeout": 30000,
  "max_redirects": 10,
  "retry_count": 2,
  "retry_codes": [429, 503],
  "include_paths": ["^/docs/"],
  "exclude_paths": ["/admin/"],
  "content": {
    "output_format": "markdown",
    "preprocessing_preset": "standard",
    "exclude_selectors": [".cookie-banner"]
  },
  "cookies_enabled": false,
  "download_assets": false,
  "download_documents": true,
  "document_max_size": 52428800,
  "capture_screenshot": false,
  "save_browser_profile": false,
  "browser": {
    "mode": "auto",
    "backend": "chromiumoxide",
    "timeout": 30000,
    "wait": "network_idle"
  }
}
```

:::tip[Loading config from a file]
Since `CrawlConfig` implements `Deserialize`, you can load it from JSON, TOML, or YAML using the appropriate serde crate.
:::

## Default values summary

| Field                          | Default                 |
| ------------------------------ | ----------------------- |
| `max_depth`                    | `None` (0 -- seed only) |
| `max_pages`                    | `None` (unlimited)      |
| `max_concurrent`               | `None` (10)             |
| `respect_robots_txt`           | `false`                 |
| `user_agent`                   | `None`                  |
| `stay_on_domain`               | `false`                 |
| `allow_subdomains`             | `false`                 |
| `request_timeout`              | 30 seconds              |
| `max_redirects`                | `10`                    |
| `retry_count`                  | `0`                     |
| `cookies_enabled`              | `false`                 |
| `content.preprocessing_preset` | `"standard"`            |
| `content.output_format`        | `"markdown"`            |
| `content.exclude_selectors`    | `[]`                    |
| `download_assets`              | `false`                 |
| `download_documents`           | `true`                  |
| `document_max_size`            | 50 MB                   |
| `capture_screenshot`           | `false`                 |
| `save_browser_profile`         | `false`                 |
| `browser.mode`                 | `Auto`                  |
| `browser.backend`              | `Chromiumoxide`         |
| `browser.timeout`              | 30 seconds              |
| `browser.wait`                 | `NetworkIdle`           |
| `browser.capture_network_events` | `false`               |
