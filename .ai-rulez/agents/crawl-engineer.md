---
name: crawl-engineer
description: Web crawling pipeline development
model: sonnet
---

When working on the crawling system:

- The crawl engine lives in `crates/crawlberg/src/engine/` with `crawl_loop.rs` (main crawl loop), `builder.rs` (engine configuration), and `batch.rs` (batch crawling). The engine orchestrates URL frontier management, HTTP fetching, and content extraction.
- HTTP client and request handling is in `crates/crawlberg/src/http.rs`. Respect robots.txt rules via `robots.rs` and sitemap discovery via `sitemap.rs`.
- Rate limiting, caching, user-agent rotation, and tracing middleware live in `crates/crawlberg/src/tower/` as Tower service layers (`rate_limit.rs`, `cache.rs`, `ua_rotation.rs`, `tracing_layer.rs`).
- HTML processing pipeline: parsing via `tl` crate, pruning (`pruning.rs`), normalization (`normalize.rs`), and markdown conversion (`markdown.rs`) using `html-to-markdown-rs`.
- Browser-based crawling (feature-gated behind `browser`) uses chromiumoxide in `browser.rs`, `browser_pool.rs`, `browser_profile.rs`, and `browser_detect.rs`. Interactive page actions live in `interact/`.
- AI-powered research agent (feature-gated behind `ai`) in `research/` with `agent.rs`, `planner.rs`, and `synthesizer.rs` using `liter-llm` for LLM calls and `minijinja` for prompt templates.
- MCP server integration (feature-gated behind `mcp`) in `mcp/` exposes crawling as tool calls.
- REST API (feature-gated behind `api`) in `api/` provides HTTP endpoints via axum.
- WARC archive output (feature-gated behind `warc`) in `warc.rs`.
- Configuration types in `types/config.rs`, crawl results in `types/results.rs`, URL discovery in `types/discovery.rs`, page metadata in `types/metadata.rs`.
- Bindings: Python (`crawlberg-py`), Node.js (`crawlberg-node`), PHP (`crawlberg-php`), WASM (`crawlberg-wasm`), C FFI (`crawlberg-ffi`), with shared binding utilities in `crawlberg-bindings-common`.
- CLI entry point in `crawlberg-cli`.
- All crawling logic stays in Rust core. Bindings are thin wrappers only.
