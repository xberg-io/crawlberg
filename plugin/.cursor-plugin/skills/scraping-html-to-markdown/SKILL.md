---
name: scraping-html-to-markdown
description: >-
  Use when the user wants a single page rendered as clean Markdown plus
  structured metadata. Covers `crawlberg scrape URL`, JSON vs Markdown
  output, what metadata is returned, and how to handle JS-heavy pages.
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:d19096a84822bf7125faf97980d3559571497f0434be15e3dc6b0d1d1ca54e1c
Source-Hash: blake3:0d2377f5b62acffb6c95f9bb6c38a225f3c13665bd68a08362a369a9f04405eb
Schema-Version: v1
-->

# Scraping HTML to Markdown

`crawlberg scrape <url>` is the right tool when the user has a single
page in mind. It returns Markdown plus a full structured payload (metadata,
links, images, JSON-LD, HTTP response info).

## Quick recipe

```bash
crawlberg scrape https://example.com/article --format markdown
```

JSON form (default) when downstream needs metadata:

```bash
crawlberg scrape https://example.com/article --format json
```

## Flag surface

| Flag                    | Default | Purpose                                                |
| ----------------------- | ------- | ------------------------------------------------------ |
| `--format`              | `json`  | `json` or `markdown`.                                  |
| `--timeout`             | `30000` | Per-request timeout in ms.                             |
| `--proxy`               | —       | HTTP, HTTPS, or SOCKS5 proxy URL.                      |
| `--user-agent`          | —       | Override request UA.                                   |
| `--respect-robots-txt`  | off     | Honour `robots.txt`.                                   |
| `--browser-mode`        | `auto`  | `auto`, `always`, `never` — see headless-fallback skill. |
| `--browser-endpoint`    | —       | External CDP `ws://` URL.                              |
| `--config`              | —       | Inline JSON or `@file.json` for full `CrawlConfig`.    |

## Output shape

### Markdown mode

Prints the rendered Markdown only. Use when piping to a file the user will
read, or when the result becomes LLM context downstream.

### JSON mode

Top-level `ScrapeResult` with:

- `final_url` (after redirects), `status_code`, `content_type`, `body_size`,
  `detected_charset` — all top-level fields.
- `markdown`: `{ content, fit_content, tables, warnings }` — `fit_content` is a
  pruned LLM-optimised variant; `tables` holds structured table data preserved
  separately from the Markdown text.
- `metadata`: Open Graph (flat `og_title`/`og_description`/`og_image`), Twitter
  Card, Dublin Core, article tags, headings (H1–H6), favicons, hreflang.
- `links`: a flat array of link objects, each with a `link_type` discriminator
  (`internal`, `external`, `anchor`, `document`) — filter with
  `.links[] | select(.link_type=="external")`.
- `images`: `<img>`, `<picture>`, `srcset`, `og:image`.
- `feeds`, `json_ld`: top-level arrays of discovered feeds and JSON-LD entries.
- `response_meta`: HTTP header metadata (server, etag, cache-control, etc.).

Read `result.markdown.content` for the Markdown string when scripting.

## Common pitfalls

### Empty or stub content

Static fetch returned a JS shell. Symptoms in JSON output:

- `markdown.content` is short or only contains nav/footer chrome.
- `markdown.warnings` mentions JS-render-required.
- `metadata.headings` is empty when the page clearly has headings.

Re-run with `--browser-mode always` and see the headless-fallback skill.

### WAF block

`Auto` mode detects 8 WAF vendors and retries through headless Chrome
automatically. If you forced `--browser-mode never`, the WAF response will
fall through. Check `.status_code` — 403/406/503 with WAF headers
(`server: cloudflare`, `x-amz-cf-id`, etc.) is the giveaway.

### Robots.txt blocking the fetch

If `--respect-robots-txt` is set and the path is disallowed, the scrape
returns an error rather than partial content. Drop the flag only on hosts
you own or have authorisation for.

### Wrong charset

Most pages declare UTF-8. Pages that lie about their charset can surface as
mojibake in `markdown.content`. crawlberg exposes no encoding-override option
(there is no `force_encoding`/charset field in `CrawlConfig`, and `--config`
rejects unknown keys), so an incorrectly declared charset is a server-side defect —
re-fetch the raw bytes and transcode them downstream if you hit it.

## Examples

### Scrape an article for downstream LLM context

```bash
crawlberg scrape https://blog.example.com/post-123 --format markdown \
  > /tmp/article.md
```

### Scrape with proxy and custom UA

```bash
crawlberg scrape https://example.com \
  --proxy http://proxy.internal:3128 \
  --user-agent "crawlberg (research@example.com)" \
  --format json
```

### Extract just the OG metadata

```bash
crawlberg scrape https://example.com --format json \
  | jq '.metadata | {title: .og_title, description: .og_description, image: .og_image}'
```

## When to reach for crawl or interact instead

- The user wants the whole site, not one page → `crawling-a-site` skill.
- The user needs to click, type, or scroll before extracting → use
  `crawlberg interact` with the action list.
- The user only wants the list of URLs → `crawlberg map`.
