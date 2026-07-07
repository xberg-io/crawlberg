---
title: "Changelog"
---

## Unreleased

## 0.3.0 - 2026-06-23

First stable release. crawlberg ships a Rust core with active bindings for
Python, TypeScript/Node, Ruby, PHP, Go, Java/JNI, C#, Elixir, WebAssembly,
Dart, Kotlin/Android, Swift, Zig, and C FFI, plus a CLI, an HTTP API, and an
MCP server.

### Added

- **Tiered dispatch engine.** The crawl engine chains HTTP → Bypass → Browser
  tiers driven by per-attempt signals rather than a single bypass
  short-circuit. Public `crawlberg::types::dispatch` surface: `Tier`,
  `EscalationStrategy`, `EscalationReason`, `AttemptOutcome`, `RetryDirective`,
  `RetryPolicy`, `WafSignal`, `WafClassifier`, `DomainStatePort`,
  `DomainRecommendation`, `EscalationBudget`, and `DispatchProfile` (dispatch
  enums are `#[non_exhaustive]`). `CrawlConfig::builder()` and
  `DispatchProfile::builder()` provide fluent construction.
- **WAF detection.** A TOML fingerprint corpus (`rules/waf_fingerprints.toml`,
  34 fingerprints) with an Aho-Corasick matcher, `TomlClassifier::watch()`
  hot-reload (debounced, atomic `ArcSwap`, Kubernetes ConfigMap-safe), and
  `EwmaDomainState` for per-domain block-rate tracking that promotes/demotes
  the starting tier.
- **SSRF defense.** New `crawlberg::net::ssrf` module — `SsrfPolicy`,
  `HostMatcher` (`Exact`/`Suffix`/`Cidr`), `SsrfError`, and async
  `validate_url`. `CrawlConfig::ssrf` plus builder methods
  `allow_private_networks(bool)` and `ssrf_allowlist_host(HostMatcher)`;
  `CrawlError::SsrfPolicyViolation`. Exposed as a settable DTO (`deny_private`,
  `max_redirects`) across every binding.
- **Browser pool injection.** `BrowserPool`/`BrowserPoolConfig` and
  `NativeBrowserExecutor`/`NativeBrowserExecutorConfig` are public;
  `CrawlEngineBuilder::with_browser_pool` / `with_native_executor` and
  `CrawlEngineHandle::from_engine` let consumers construct and `warm()` a pool
  once and reuse it across all crawl jobs.
- **Public substrate parsers.** `crawlberg::robots` and `crawlberg::sitemap`
  are public (`parse_robots_txt`, `is_path_allowed`, `RobotsRules`,
  `parse_sitemap_xml`, `parse_sitemap_index`, `is_sitemap_index`) — usable
  without spinning up the engine.
- **Pluggable proxy rotation.** `ProxyProvider` trait + `StaticProxyProvider`
  baseline, wired into the reqwest fetch path via
  `CrawlEngineBuilder::with_proxy_provider`; called per request and taking
  precedence over the static `CrawlConfig::proxy` value.
- **CLI.** `batch-scrape`, `batch-crawl`, `download`, `citations`, and
  `version` subcommands, bringing the CLI to 1:1 with the core and MCP
  surfaces.
- **MCP server.** Tools are 1:1 with the CLI (`batch_crawl`,
  `generate_citations`, …), each declaring `read_only`/`destructive`/
  `open_world` safety annotations, and are served over both stdio and rmcp
  Streamable HTTP at `/mcp` when the binary is built with the `api` + `mcp`
  features.
- **Observability.** OpenTelemetry counters
  `crawlberg_waf_fingerprint_matches_total` and
  `crawlberg_escalations_total`, plus property tests, cargo-fuzz targets, and
  Criterion benchmarks covering the WAF subsystem.

### Changed

- **Memory-bounded streaming crawl.** `crawl_stream` / `batch_crawl_stream`
  move each page into its `CrawlEvent::Page` and drop it instead of
  accumulating every page, bounding peak memory on large crawls (≈2.5 GB →
  ≈20 MB working set). `crawl()`'s batch result is unchanged.
- **Dispatch model.** `CrawlError::WafBlocked` is now a struct variant
  (`{ vendor, message }`); `DomainStatePort` moved to an observation model
  (`recommend`/`observe`); `SimpleRetryPolicy`'s off-by-one is fixed
  (`max_retries=3` yields 3 retries); `#[non_exhaustive]` added to
  `CrawlError`, `NetworkErrorKind`, and the dispatch enums so future variants
  are non-breaking.
- **Asset downloads** route through `http_fetch`, so every file fetch is
  subject to the SSRF policy.

### Fixed

- **Crawl loop materializes downloaded documents.** The `download_documents`
  flag was previously honored only by single-page `scrape()`; the crawl loop
  now builds `CrawlPageResult.downloaded_document` for linked PDFs/DOCX via a
  shared helper instead of fetching, flagging, and discarding the bytes.
- **SSRF rollout hardening.** Follow-up fixes to the SSRF refactor: redirect
  `final_url` is tracked again (per-hop re-validation moved into
  `follow_redirects`), within-batch URL dedup no longer races, crawl
  child-depth is incremented (restoring `max_depth` and `include_paths`
  semantics), and `CrawlConfig` JSON deserialization honors
  `CRAWLBERG_ALLOW_PRIVATE_NETWORK` through a `SsrfPolicy::from_env` serde
  default. Each is covered by a regression test.
- **MCP server exposed zero tools.** The handler was missing rmcp's
  `#[tool_handler]`, so `tools/list`/`tools/call` returned an empty list over
  both stdio and HTTP; it now delegates to the generated tool router.

### Security

- **SSRF defense, enabled by default.** `scrape()`, `crawl()`,
  `batch_crawl()`, sitemap fetch, robots.txt fetch, and asset download refuse
  URLs resolving to loopback (127.0.0.0/8), RFC1918 private networks,
  link-local (169.254.0.0/16), cloud metadata (0.0.0.0/8), multicast
  (224.0.0.0/4), IPv6 ULA (fc00::/7), IPv6 link-local (fe80::/10), IPv6
  multicast (ff00::/8), or any non-http(s) scheme. Includes DNS-rebinding
  mitigation (every resolved IP must pass the policy), redirect-chain
  re-validation (bounded by `ssrf.max_redirects`, default 5), and
  link-enqueue validation with bounded concurrency. Opt out via
  `CRAWLBERG_ALLOW_PRIVATE_NETWORK=1` or
  `CrawlConfig::allow_private_networks(true)`.

### Build

- Bindings, facades, READMEs, docs, stubs, and e2e suites are generated by
  alef (pinned at 0.26.6) across all 14 language targets.
- Publish-pipeline hardening: a native per-arch Docker matrix that drops QEMU
  emulation, Flutter-free Dart native builds for pub.dev, Swift artifactbundle
  checksum injection and Apple system-framework linking, and
  lockfile-preserving source publishes for the Elixir NIF, PHP extension, and
  Ruby gem.
