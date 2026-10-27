---
type: Reference
title: E2E Fixtures
description: "Crawlberg e2e fixture schema: one fixture per file under fixtures/<topic>/, mock HTTP responses, CrawlConfig input, call names and assertion types. Use when adding or editing a crawlberg e2e fixture."
paths:
  - "fixtures/**"
  - "e2e/**"
x-ai-rulez:
  kind: skill
  id: e2e-fixtures
  metadata:
    name: e2e-fixtures
---

# Crawlberg E2E Fixtures

- Schema: `fixtures/schema.json` (required `id`, `description`, `assertions`). One fixture object per file, `fixtures/<topic>/<id>.json`; the directory is the topic and usually matches `docs.topic` (`rate_limit/` fixtures use topic `rate-limit`). `category` is almost never set (only `content/` uses `category: "crawl"`).
- Fields: `docs {topic, stem, title}`, `tags`, `input`, `assertions`, and optionally `skip {languages, reason}`, `mock_response`, `call`, `preserve_input_urls`, `test_timeout_ms`.
- `input.mock_responses[]` serves the pages (`status_code`, `headers`, `body_inline`); `input.config` is a `CrawlConfig` (for example `auth`, `respect_robots_txt`).
- `call` defaults to `scrape`; other calls: `batch_scrape`, `crawl`, `map_urls`, `interact`, `batch_crawl`, `crawl_stream`, `batch_crawl_stream`.
- Assertion types in use: `equals`, `error`, `contains`, `not_contains`, `contains_all`, `not_empty`, `is_empty`, `is_true`, `greater_than`, `greater_than_or_equal`, `less_than`, `less_than_or_equal`. Each assertion is `{type, field, value}` (`values` for `contains_all`); `field` is a dotted path into the call result. An assertion can carry its own `skip {languages, kind, reason}` to skip just that check for a language limitation.
