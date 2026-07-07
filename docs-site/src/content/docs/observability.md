---
title: "OpenTelemetry Observability"
---

Crawlberg emits W3C-standard traces and metrics via OpenTelemetry, aligned with semantic conventions and xberg-enterprise's existing naming. Integrate your observability backend (Jaeger, Prometheus, Grafana, Datadog) to track crawl performance, identify bottlenecks, and diagnose WAF issues.

## Why OpenTelemetry

OpenTelemetry provides a vendor-neutral API for instrumenting applications. Crawlberg emits distributed traces (spans with W3C TraceContext headers) and metrics (counters, histograms, up-down counters) compatible with the [W3C TraceContext](https://www.w3.org/TR/trace-context/) specification. This enables end-to-end observability: trace a single request through your service layer into crawlberg's engine, see exactly where time is spent, and correlate with WAF signals across your infrastructure.

## Quick start with `init_otlp`

Enable the `telemetry-init` feature and call the one-line helper to attach both tracer and meter providers:

```rust
use crawlberg::telemetry::{TelemetryConfig, init_otlp};
use crawlberg::{batch_crawl, create_engine};

let _guard = init_otlp(TelemetryConfig {
    service_name: "my-crawler".into(),
    service_version: Some(env!("CARGO_PKG_VERSION").into()),
    otlp_endpoint: "http://localhost:4317".into(),
    resource_attrs: vec![
        ("deployment.environment".into(), "staging".into()),
        ("service.namespace".into(), "web-team".into()),
    ],
})?;

// Now run your crawl — spans and metrics will export to the collector
let engine = create_engine(None)?;
let results = batch_crawl(&engine, seeds).await?;
// Guard flushes on drop
drop(_guard);
```

The `TelemetryGuard` flushes all pending spans and metrics on drop, ensuring clean shutdown.

## Bring-your-own SDK

If your application already initializes OpenTelemetry (as in xberg-enterprise's `crates/observability/src/telemetry.rs:135-152`), crawlberg automatically emits spans and metrics to the global tracer and meter without any init call. Dependency versions must align:

| Crate | Version |
|-------|---------|
| `opentelemetry` | `0.32` (features: `trace`, `metrics`) |
| `opentelemetry_sdk` | `0.32` (features: `rt-tokio`) |
| `opentelemetry-otlp` | `0.32` (features: `grpc-tonic`, `trace`, `metrics`) |
| `opentelemetry-semantic-conventions` | `0.32` |
| `tracing-opentelemetry` | `0.33` |
| `tracing-subscriber` | `0.3.23` |

Mixing major versions will break propagator injection and cause spans to drop. Verify your `Cargo.lock` matches the table above.

## W3C TraceContext propagation

Crawlberg preserves W3C TraceContext headers across HTTP requests, making it easy to correlate crawl activity with upstream services. Rust callers can use:

- `with_traceparent(traceparent: &str, callback: impl Fn() -> R) -> R` — execute a callback with the given trace context active, so child spans become descendants.
- `current_traceparent() -> Option<String>` — extract the current trace context as a W3C `traceparent` header value (format: `00-<trace_id>-<span_id>-<flags>`).

Generated language bindings do not expose these helpers today because `with_traceparent` requires a Rust callback. Propagate trace context at the host-service layer with that language's OpenTelemetry SDK, and pass request headers normally into services that wrap crawlberg.

```rust
use crawlberg::telemetry::{current_traceparent, with_traceparent};

let incoming = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
let _traceparent = with_traceparent(incoming, || {
    current_traceparent()
});
```

## Span catalogue

Every span is emitted via `tracing::info_span!` and automatically bridged to OpenTelemetry by the `tracing_opentelemetry::layer()`. Attribute keys use W3C semantic conventions where available (from `opentelemetry_semantic_conventions`), or crawlberg-specific extensions (prefixed `crawl.`).

| Span Name | Site | Attributes |
|-----------|------|-----------|
| `crawl.engine.start` | `CrawlEngine::build` entry | `crawl.seed_count`, `crawl.max_depth`, `crawl.max_pages`, `crawl.strategy`, `crawl.browser_mode` |
| `crawl.engine.batch` | `batch_crawl()` entry | `crawl.seed_count` |
| `crawl.loop.iteration` | Crawl loop dequeue | `crawl.depth`, `crawl.frontier_size`, `crawl.pages_completed` |
| `crawl.page.discover` | Link extraction (per discovered URL) | `url.full`, `url.domain`, `crawl.parent_url`, `crawl.depth`, `crawl.link_type` |
| `crawl.page.fetch` | Tower HTTP layer | `http.request.method`, `url.full`, `server.address`, `http.response.status_code`, `http.response.body_size`, `crawl.tier`, `crawl.final_url`, `crawl.mime_type` |
| `crawl.browser.session` | Browser fetch entry (chromiumoxide or native) | `crawl.browser.backend`, `crawl.browser.session_id`, `crawl.pages_rendered` |
| `crawl.robots.check` | robots.txt predicate | `url.domain`, `crawl.host`, `crawl.allowed` |
| `crawl.document.download` | Document materialization | `url.full`, `crawl.mime_type`, `crawl.size_bytes` |

## Metric catalogue

Crawlberg emits 10 OTel instruments via `crawlberg::telemetry::metrics::registry()`. Names align with xberg-enterprise's existing emission points, so cloud can consolidate duplicate metrics in a follow-up.

| Metric Name | Instrument | Labels | Description |
|-------------|-----------|--------|-------------|
| `crawl_pages_total` | Counter (u64) | `status` ∈ `{ok, http_error, timeout, blocked}` | Pages fetched, partitioned by terminal status |
| `crawl_documents_discovered_total` | Counter (u64) | `mime_type` | Non-HTML documents discovered (PDF, DOCX, etc.) |
| `crawl_robots_blocked_total` | Counter (u64) | — | Requests rejected by robots.txt |
| `crawl_waf_blocks_total` | Counter (u64) | `vendor` ∈ `{cloudflare, datadome, …}` | WAF challenges detected, per vendor |
| `crawl_backend_escalations_total` | Counter (u64) | `from_tier`, `to_tier`, `reason` | Tier escalations (e.g., HTTP→Browser) |
| `crawl_bypass_requests_total` | Counter (u64) | `vendor`, `mode` ∈ `{managed, byo}` | Requests routed through bypass provider |
| `crawl_bypass_failures_total` | Counter (u64) | `vendor`, `reason` | Bypass provider failures |
| `crawl_duration_seconds` | Histogram (f64) | `output_mode`, `status` | End-to-end crawl duration |
| `crawl_pages_duration_seconds` | Histogram (f64) | `host` | Per-page fetch duration |
| `crawl_browser_sessions_active` | Up-down Counter (i64) | — | Active headless-browser sessions |

## Grafana panel example

Copy this JSON into a Grafana dashboard to visualize crawl success rate:

```json
{
  "title": "Crawl Success Rate",
  "targets": [
    {
      "expr": "rate(crawl_pages_total{status=\"ok\"}[5m])",
      "legendFormat": "OK (pages/sec)",
      "refId": "A"
    },
    {
      "expr": "rate(crawl_pages_total{status=\"http_error\"}[5m])",
      "legendFormat": "HTTP Errors",
      "refId": "B"
    },
    {
      "expr": "rate(crawl_pages_total{status=\"timeout\"}[5m])",
      "legendFormat": "Timeouts",
      "refId": "C"
    },
    {
      "expr": "rate(crawl_pages_total{status=\"blocked\"}[5m])",
      "legendFormat": "WAF Blocked",
      "refId": "D"
    }
  ],
  "type": "graphPanel",
  "yaxes": [
    {
      "format": "short",
      "label": "Pages per second"
    }
  ]
}
```

Stack this with `crawl_pages_duration_seconds_bucket` (latency percentiles) to correlate performance with success rate.

## Cloud alignment note

Xberg-cloud currently emits ~12 duplicate `crawl_*` metrics and one manual `crawl.engine.batch` span at `services/worker/src/observability/metrics.rs:131-272` and `crawl_handler.rs:281-286`. Once crawlberg's observability lands, cloud will delete its duplicate emitters in a follow-up PR. No user-facing changes — cloud's observability dashboards and alerts continue to work unchanged.
