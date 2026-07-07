---
title: "Docker"
---

Crawlberg ships two Dockerfile variants: a full API server image and a minimal CLI image.
Both use multi-stage builds with Debian Bookworm for reproducible, small runtime images.

## Dockerfile variants

### API server (`docker/Dockerfile`)

The default Dockerfile builds the full API server with these features enabled:

- `api` -- REST API server (Firecrawl v1-compatible)
- `mcp` -- Model Context Protocol server
- `warc` -- WARC 1.1 archival output
- `ai` -- Deep research agent
- `telemetry-init` -- Optional OpenTelemetry/OTLP initialization helpers

The image exposes port 3000 and starts the API server by default.

### CLI (`docker/Dockerfile.cli`)

A lighter image with only the `warc` feature enabled. No server, no AI. Intended for
one-shot scrape/crawl/map commands.

## Building

### API server image

```bash
docker build -t crawlberg:latest -f docker/Dockerfile .
```

### CLI image

```bash
docker build -t crawlberg-cli:latest -f docker/Dockerfile.cli .
```

:::tip[BuildKit caches]
Both Dockerfiles use `--mount=type=cache` for the Cargo registry, git index, and
build target directory. Ensure BuildKit is enabled (`DOCKER_BUILDKIT=1`) for
significantly faster rebuilds.
:::

## Running

### API server

```bash
docker run -d \
  --name crawlberg \
  -p 3000:3000 \
  -e RUST_LOG=info \
  crawlberg:latest
```

The server binds to `0.0.0.0:3000` by default. Override with custom arguments:

```bash
docker run -d \
  -p 8080:8080 \
  crawlberg:latest \
  serve --host 0.0.0.0 --port 8080
```

### CLI commands

```bash
# Scrape a single URL
docker run --rm crawlberg-cli:latest scrape https://example.com

# Crawl with depth limit
docker run --rm crawlberg-cli:latest crawl https://example.com --depth 2 --format markdown

# Map a site
docker run --rm crawlberg-cli:latest map https://example.com --limit 50
```

### MCP server

```bash
docker run -i --rm crawlberg:latest mcp
```

The MCP server uses stdio transport, so the container must run with `-i` (interactive stdin).

## Environment variables

| Variable   | Default | Description                                                                               |
| ---------- | ------- | ----------------------------------------------------------------------------------------- |
| `RUST_LOG` | `info`  | Log level filter for Rust tracing subscribers (e.g. `debug`, `crawlberg=trace`). |

## Health checks

The API server image includes a Docker `HEALTHCHECK` directive:

```dockerfile
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD ["curl", "-f", "http://localhost:3000/health"]
```

The `/health` endpoint returns:

```json
{ "status": "ok", "version": "0.3.0" }
```

Orchestrators (Docker Compose, Kubernetes, Cloud Run) can use this to detect when the
service is ready.

## Security

Both images follow container security best practices:

- **Non-root user** -- The runtime stage creates a dedicated `crawlberg` user and group.
  The binary runs as this unprivileged user.
- **Minimal base** -- The runtime uses `debian:bookworm-slim` with only `ca-certificates`
  (and `curl` for health checks in the API image).
- **No secrets in image** -- Configuration is passed via environment variables or
  command-line arguments at runtime.

## Docker Compose example

```yaml
services:
  crawlberg:
    build:
      context: .
      dockerfile: docker/Dockerfile
    ports:
      - "3000:3000"
    environment:
      RUST_LOG: info
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: 30s
      timeout: 10s
      start_period: 5s
      retries: 3
    restart: unless-stopped
```

### With a browser sidecar

For browser-based crawling inside Docker, run Chrome as a sidecar:

```yaml
services:
  crawlberg:
    build:
      context: .
      dockerfile: docker/Dockerfile
    ports:
      - "3000:3000"
    environment:
      RUST_LOG: info
    depends_on:
      chrome:
        condition: service_healthy

  chrome:
    image: chromedp/headless-shell:latest
    ports:
      - "9222:9222"
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:9222/json/version"]
      interval: 10s
      timeout: 5s
      retries: 3
```

Then configure the browser endpoint to point to the Chrome sidecar:

```bash
# In CrawlConfig or via the API
browser.endpoint = "ws://chrome:9222/devtools/browser/..."
```

:::caution[CLI image has no browser]
The CLI image (`Dockerfile.cli`) does not include Chrome or Chromium.
For browser-based features, use the API server image with an external browser endpoint.
:::
