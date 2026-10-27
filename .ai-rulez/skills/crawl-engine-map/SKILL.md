---
type: Reference
title: Crawl Engine Map
description: Module map of the crawlberg core crate (engine, HTTP, tower layers, HTML pipeline, feature-gated browser/ai/mcp/api/warc modules, types). Use when finding where crawl logic lives in crates/crawlberg.
paths:
  - "crates/crawlberg/**"
x-ai-rulez:
  kind: skill
  id: crawl-engine-map
  metadata:
    name: crawl-engine-map
---

Paths are relative to `crates/crawlberg/src/`.

- Crawl engine: `engine/` — `crawl_loop.rs` (main loop), `builder.rs` (configuration), `batch.rs` (batch crawling); it drives the URL frontier, fetching and extraction.
- HTTP: `http.rs`; robots.txt via `robots.rs`, sitemap discovery via `sitemap.rs`.
- Tower layers in `tower/`: `rate_limit.rs`, `cache.rs`, `ua_rotation.rs`, `tracing_layer.rs`.
- HTML pipeline: parse with `tl`, then `pruning.rs`, `normalize.rs`, `markdown.rs` (via `html-to-markdown-rs`). Extraction in `html/` (links, metadata, JSON-LD, feeds, images).
- Entry points: `scrape.rs` (single page), `map.rs` (URL discovery). WAF detection: `waf/`.
- `browser` feature: chromiumoxide in `browser.rs`, `browser_pool.rs`, `browser_profile.rs`, `browser_detect.rs`; page actions in `interact/`; fingerprint patches in `stealth.rs`; SSRF re-validation of browser requests in `ssrf_intercept.rs`/`ssrf_intercept/`.
- `browser-native` feature: `native_browser.rs` via the `crawlberg-browser` crate.
- `ai` feature: `research/` (`agent.rs`, `planner.rs`, `synthesizer.rs`) using `liter-llm` and `minijinja` prompt templates.
- `mcp` feature: `mcp/` exposes crawling as tool calls. `api` feature: `api/` serves HTTP endpoints via axum. `warc` feature: `warc.rs`.
- Types: `types/config.rs`, `types/results.rs`, `types/discovery.rs`, `types/metadata.rs`.
- Crawling logic stays in the Rust core; binding crates (`crates/crawlberg-{py,node,php,wasm,ffi,jni}`, shared `crawlberg-bindings-common`) and `crawlberg-cli` are thin wrappers.
