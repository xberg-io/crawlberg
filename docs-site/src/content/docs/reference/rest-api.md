---
title: "REST API Reference"
---

Crawlberg exposes Firecrawl v1-compatible REST routes for web scraping, crawling, site mapping, batch scrape jobs, and document downloads. The API server is started via the CLI (`crawlberg serve`) or programmatically with `crawlberg::serve_api(config)` when the `api` feature is enabled.

All request and response bodies use JSON. Responses follow a consistent envelope format.

## Response Envelope

Every endpoint returns a JSON object with this structure:

```json
{
  "success": true,
  "data": { ... },
  "error": null
}
```

On failure:

```json
{
  "success": false,
  "data": null,
  "error": {
    "code": "BAD_REQUEST",
    "message": "url is required"
  }
}
```

## Server Configuration

| Setting          | Default   | Description                                      |
| ---------------- | --------- | ------------------------------------------------ |
| Host             | `0.0.0.0` | IP address to bind to                            |
| Port             | `3000`    | Port number                                      |
| Max request body | 10 MB     | Request body size limit                          |
| Request timeout  | 5 minutes | Global handler timeout (returns `408` on expiry) |

The server includes CORS (allow all origins), gzip compression, request ID propagation (`x-request-id`), and panic recovery middleware.

---

## Endpoints

### POST /v1/scrape

Scrape a single URL and extract content synchronously.

**Request Body:**

| Field             | Type       | Required | Default        | Description                                                             |
| ----------------- | ---------- | -------- | -------------- | ----------------------------------------------------------------------- |
| `url`             | `string`   | Yes      | --             | URL to scrape (must start with `http://` or `https://`, max 8192 chars) |
| `formats`         | `string[]` | No       | Engine config  | Accepted for compatibility; the handler does not override `content.output_format` today. |
| `onlyMainContent` | `boolean`  | No       | Engine config  | Accepted for compatibility; ignored by `POST /v1/scrape` today.         |
| `includeTags`     | `string[]` | No       | Engine config  | Accepted for compatibility; ignored by `POST /v1/scrape` today.         |
| `excludeTags`     | `string[]` | No       | Engine config  | Accepted for compatibility; ignored by `POST /v1/scrape` today.         |
| `timeout`         | `integer`  | No       | Engine config  | Accepted for compatibility; ignored by `POST /v1/scrape` today.         |

**Response:** `200 OK`

```json
{
  "success": true,
  "data": {
    "status_code": 200,
    "content_type": "text/html; charset=utf-8",
    "html": "<html>...</html>",
    "body_size": 12345,
    "metadata": { "title": "Example", "description": "..." },
    "links": [{ "url": "https://example.com/about", "text": "About", "link_type": "internal" }],
    "images": [{ "url": "https://example.com/logo.png", "alt": "Logo", "source": "img" }],
    "feeds": [],
    "json_ld": [],
    "markdown": { "content": "# Example\n\nPage content..." },
    "browser_used": false
  }
}
```

**Error Responses:** `400`, `404`, `500` (see [Errors](/reference/errors/))

---

### POST /v1/crawl

Start an asynchronous crawl job. Returns a job ID for polling.

**Request Body:**

| Field             | Type       | Required | Default        | Description                          |
| ----------------- | ---------- | -------- | -------------- | ------------------------------------ |
| `url`             | `string`   | Yes      | --             | Seed URL to start crawling from      |
| `maxDepth`        | `integer`  | No       | Engine default | Maximum link depth to follow         |
| `maxPages`        | `integer`  | No       | Engine default | Maximum number of pages to crawl     |
| `includePaths`    | `string[]` | No       | `[]`           | URL path patterns to include (regex) |
| `excludePaths`    | `string[]` | No       | `[]`           | URL path patterns to exclude (regex) |
| `onlyMainContent` | `boolean`  | No       | Engine config  | When `true`, sets `content.preprocessing_preset = "aggressive"` for this crawl job. |

**Response:** `202 Accepted`

```json
{
  "success": true,
  "id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**Error Responses:** `400`

---

### GET /v1/crawl/{id}

Poll the status of a crawl job.

**Path Parameters:**

| Parameter | Type            | Description                                 |
| --------- | --------------- | ------------------------------------------- |
| `id`      | `string` (UUID) | Job identifier returned by `POST /v1/crawl` |

**Response:** `200 OK`

```json
{
  "status": "completed",
  "total": 10,
  "completed": 10,
  "data": [
    {
      "url": "https://example.com",
      "normalized_url": "https://example.com/",
      "status_code": 200,
      "content_type": "text/html",
      "html": "...",
      "metadata": { ... },
      "depth": 0,
      "markdown": { "content": "..." }
    }
  ]
}
```

**Job Statuses:**

| Status        | Description                                               |
| ------------- | --------------------------------------------------------- |
| `pending`     | Job created, not yet started                              |
| `in_progress` | Crawl is running                                          |
| `completed`   | Crawl finished successfully; `data` contains page results |
| `failed`      | Crawl failed; `error` contains the message                |
| `cancelled`   | Job was cancelled via `DELETE`                            |

**Error Responses:** `400` (invalid UUID), `404` (job not found)

---

### DELETE /v1/crawl/{id}

Cancel a running crawl job.

**Path Parameters:**

| Parameter | Type            | Description    |
| --------- | --------------- | -------------- |
| `id`      | `string` (UUID) | Job identifier |

**Response:** `200 OK`

```json
{
  "success": true,
  "data": "cancelled"
}
```

**Error Responses:** `400` (invalid UUID), `404` (job not found or not cancellable)

---

### POST /v1/map

Discover all URLs on a website via sitemaps and link extraction. Synchronous.

**Request Body:**

| Field    | Type      | Required | Default  | Description                                           |
| -------- | --------- | -------- | -------- | ----------------------------------------------------- |
| `url`    | `string`  | Yes      | --       | URL to discover links from                            |
| `limit`  | `integer` | No       | No limit | Maximum number of URLs to return                      |
| `search` | `string`  | No       | --       | Case-insensitive substring filter for discovered URLs |

**Response:** `200 OK`

```json
{
  "success": true,
  "data": {
    "urls": [
      {
        "url": "https://example.com/",
        "lastmod": "2025-01-01",
        "changefreq": "daily",
        "priority": "1.0"
      },
      { "url": "https://example.com/about", "lastmod": null, "changefreq": null, "priority": null }
    ]
  }
}
```

**Error Responses:** `400`, `500`

---

### POST /v1/batch/scrape

Start an asynchronous batch scrape job for multiple URLs.

**Request Body:**

| Field             | Type       | Required | Default        | Description                        |
| ----------------- | ---------- | -------- | -------------- | ---------------------------------- |
| `urls`            | `string[]` | Yes      | --             | URLs to scrape (must not be empty) |
| `formats`         | `string[]` | No       | Engine config  | Accepted for compatibility; ignored by `POST /v1/batch/scrape` today. |
| `onlyMainContent` | `boolean`  | No       | Engine config  | Accepted for compatibility; ignored by `POST /v1/batch/scrape` today. |

**Response:** `202 Accepted`

```json
{
  "success": true,
  "id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**Error Responses:** `400` (empty URLs array)

---

### GET /v1/batch/scrape/{id}

Poll the status of a batch scrape job. Response format is the same as `GET /v1/crawl/{id}`.

**Path Parameters:**

| Parameter | Type            | Description          |
| --------- | --------------- | -------------------- |
| `id`      | `string` (UUID) | Batch job identifier |

**Response:** `200 OK` -- Same structure as crawl job status. Each item in `data` is either a `ScrapeResult` or an object with `url` and `error` fields.

**Error Responses:** `400`, `404`

---

### POST /v1/download

Download a document from a URL. Uses the scrape pipeline internally, which handles document downloads (PDF, DOCX, images, etc.) when `download_documents` is enabled.

**Request Body:**

| Field     | Type      | Required | Default | Description                    |
| --------- | --------- | -------- | ------- | ------------------------------ |
| `url`     | `string`  | Yes      | --      | URL to download from           |
| `maxSize` | `integer` | No       | Engine config | Accepted for compatibility; ignored by `POST /v1/download` today. Use `document_max_size` in the server config. |

**Response:** `200 OK` -- Returns the full `ScrapeResult`, which includes a `downloaded_document` field for non-HTML content.

**Error Responses:** `400`, `500`

---

### GET /health

Health check endpoint.

**Response:** `200 OK`

```json
{
  "status": "ok",
  "version": "0.3.0"
}
```

---

### GET /version

Version information.

**Response:** `200 OK`

```json
{
  "version": "0.3.0"
}
```

---

### GET /openapi.json

Returns the OpenAPI 3.0 schema for the API, generated from handler annotations via `utoipa`.

---

## Firecrawl Compatibility

The REST API uses Firecrawl v1 endpoint paths and `camelCase` JSON field naming in request bodies. Response bodies use Rust-native `snake_case` field naming. Key compatibility notes:

- Request fields use `camelCase`: `maxDepth`, `maxPages`, `onlyMainContent`, `includePaths`, `excludePaths`, `includeTags`, `excludeTags`, `maxSize`
- Async jobs (crawl, batch scrape) return a UUID `id` for polling, matching the Firecrawl pattern
- `POST /v1/crawl` applies `maxDepth`, `maxPages`, `onlyMainContent`, `includePaths`, and `excludePaths` by rebuilding an engine for that job.
- `POST /v1/map` applies `limit` and `search` after URL discovery.
- `formats`, `includeTags`, `excludeTags`, scrape `onlyMainContent`, scrape `timeout`, batch-scrape `formats`, batch-scrape `onlyMainContent`, and download `maxSize` are accepted for request compatibility but ignored by the current handlers.

## URL Validation

All endpoints that accept a URL enforce these rules:

- URL must not be empty
- URL must start with `http://` or `https://`
- URL must not exceed 8192 characters
