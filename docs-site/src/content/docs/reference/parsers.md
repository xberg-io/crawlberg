---
title: "Parsers"
---

## Parsers

Crawlberg exposes substrate-level parsers for robots.txt and sitemap.xml that are usable without constructing the crawl engine. These are **Rust-only** surfaces and are not exposed in language bindings.

Import from `crawlberg::robots` and `crawlberg::sitemap` to use them with your own HTTP fetcher and pipeline.

### Robots.txt Parsing

Parse robots.txt rules for a user-agent and check path compliance.

#### `parse_robots_txt(body: &str, user_agent: &str) -> RobotsRules`

Extract rules for a specific user-agent from a robots.txt file body.

Returns the most-specific matching block for the user-agent, falling back to the wildcard (`*`) block. The returned `RobotsRules` struct contains:

- **`allow`** — Explicit allow patterns (Vec\<String>)
- **`disallow`** — Explicit disallow patterns (Vec\<String>)
- **`crawl_delay`** — Crawl-delay directive in seconds (Option\<u64>), if present
- **`sitemaps`** — Sitemap URLs declared in the file (Vec\<String>)
- **`is_wildcard_block`** — `true` when the matched block is `User-agent: *` with `Disallow: /`

```rust
use crawlberg::robots::{parse_robots_txt, is_path_allowed};

let body = "User-agent: *\nDisallow: /private\nCrawl-delay: 2";
let rules = parse_robots_txt(body, "crawlberg");
assert!(!is_path_allowed("/private/secret", &rules));
assert!(is_path_allowed("/public", &rules));
assert_eq!(rules.crawl_delay, Some(2));
```

#### `is_path_allowed(path: &str, rules: &RobotsRules) -> bool`

Determine whether a URL path is allowed by the robots.txt rules.

Uses longest-match semantics: the longest matching allow or disallow rule wins. Returns `true` if the path is allowed (no disallow match, or allow match longer than disallow).

```rust
use crawlberg::robots::{parse_robots_txt, is_path_allowed};

let rules = parse_robots_txt("Disallow: /admin\nAllow: /admin/public", "bot");
assert!(is_path_allowed("/admin/public/file.html", &rules));  // allow wins
assert!(!is_path_allowed("/admin/secret", &rules));            // disallow wins
```

### Sitemap Parsing

Parse sitemap.xml and sitemap index documents without the crawl engine.

#### `parse_sitemap_xml(body: &str) -> Vec<SitemapUrl>`

Extract all URL entries from a sitemap XML document.

Each entry is a `SitemapUrl` struct with fields:

- **`url`** — The URL (String)
- **`lastmod`** — Last modification date, if present (Option\<String>)
- **`changefreq`** — Change frequency, if present (Option\<String>)
- **`priority`** — Priority, if present (Option\<String>)

```rust
use crawlberg::sitemap::parse_sitemap_xml;

let body = r#"<?xml version="1.0"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>https://example.com/a</loc>
    <lastmod>2025-01-01</lastmod>
    <priority>0.8</priority>
  </url>
  <url><loc>https://example.com/b</loc></url>
</urlset>"#;

let urls = parse_sitemap_xml(body);
assert_eq!(urls.len(), 2);
assert_eq!(urls[0].url, "https://example.com/a");
assert_eq!(urls[0].lastmod, Some("2025-01-01".to_string()));
```

#### `parse_sitemap_index(body: &str) -> Vec<String>`

Extract child sitemap URLs from a sitemap index document.

Returns the list of sitemap URLs referenced in the index.

```rust
use crawlberg::sitemap::{parse_sitemap_index, is_sitemap_index};

let body = r#"<?xml version="1.0"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/sitemap-1.xml</loc></sitemap>
  <sitemap><loc>https://example.com/sitemap-2.xml</loc></sitemap>
</sitemapindex>"#;

assert!(is_sitemap_index(body));
let child_urls = parse_sitemap_index(body);
assert_eq!(child_urls.len(), 2);
```

#### `is_sitemap_index(body: &str) -> bool`

Quickly check whether a document body looks like a sitemap index.

Returns `true` if the body contains `<sitemapindex` or `<sitemapindex>`.

```rust
use crawlberg::sitemap::is_sitemap_index;

let index_body = "<?xml><sitemapindex>...";
let regular_body = "<?xml><urlset>...";

assert!(is_sitemap_index(index_body));
assert!(!is_sitemap_index(regular_body));
```
