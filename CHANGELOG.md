# Changelog

All notable changes to crawlberg are documented here.

## [Unreleased]

### Added

- Advertise a typed `outputSchema` (SEP-2106) on every MCP tool, derived from the
  result types via `schemars` (gated behind the `mcp` feature). This completes the
  structured-output story: clients now get both the machine-readable
  `structuredContent` and a schema to validate it against. `download`,
  `get_version`, and the batch tools serialize dedicated DTOs so their schema and
  output share one source of truth. Drift tests assert every serialized field is a
  declared schema property and every required property is emitted, so the schema
  and `structuredContent` can never diverge.

### Changed

- Raw `println!`/`eprintln!`/`print!`/`eprint!`/`dbg!` are denied in production code across the whole
  workspace (clippy `print_stdout`/`print_stderr`/`dbg_macro`); `tracing` is the sole diagnostic
  surface, and CLI result output to stdout opts back in per call site
  (`#[expect(clippy::print_stdout)]`). Language bindings were regenerated with alef 0.48.11.
- **Breaking:** the `telemetry-init` Cargo feature is renamed to `otel` to match the org-wide
  observability feature name; update `--features telemetry-init` invocations to `--features otel`.
- **Breaking:** the `crawlberg` library is now emit-only — it installs no global subscriber or OTLP
  exporter. The subscriber/OTLP install (`init_otlp`, `TelemetryConfig`, `TelemetryGuard`,
  `TelemetryInitError`) and the console-logging module (`LogConfig`, `LogFormat`, `try_init`, `layer`)
  moved to `crawlberg-cli`; the library `logging` feature is removed. The library `otel` feature no
  longer pulls the exporter/subscriber stack — it only forwards `liter-llm/otel` so the `ai`
  integration's GenAI metrics compile in. crawlberg's own spans, semantic-convention attributes, and
  metric instruments remain always-on and flow into whatever exporter the consumer installs. The W3C
  helpers (`with_traceparent`, `current_traceparent`) are unchanged. Consumers that installed
  telemetry via the library should use `crawlberg-cli --features otel` (export is activated at runtime
  by `OTEL_EXPORTER_OTLP_ENDPOINT`) or install their own subscriber.
- `crawlberg-cli` gains an `otel` feature that installs the OTLP export pipeline for every command
  (including `serve`), gated at runtime by `OTEL_EXPORTER_OTLP_ENDPOINT`; the console subscriber is
  installed by default when OTLP is not configured. The server Docker image builds with
  `crawlberg-cli/otel`.
- Upgrade `html-to-markdown-rs` 3.9 → 3.10 and `liter-llm` 1.11 → 1.12. `liter-llm` 1.12 makes
  `tracing` an always-on dependency (its `tracing` Cargo feature is gone) and ships a real OTLP
  export path in its CLI; crawlberg's `otel` forwarding to `liter-llm/otel` (behind `ai`) is
  unaffected.

## [1.0.12] - 2026-07-30

### Added

- Leverage the rmcp 3.0 Tasks extension (SEP-2663): the MCP server advertises the
  `io.modelcontextprotocol/tasks` capability and, when a client both declares it
  and augments a `tools/call`, runs the tool as a pollable async task
  (`tasks/get` / `tasks/update` / `tasks/cancel`) instead of blocking. Task
  support is exercised end-to-end over the stdio transport; on the stateless HTTP
  transport, which cannot propagate per-request client capabilities, a
  task-augmented call degrades gracefully to inline execution.
- `crawlberg mcp --http [--host <h>] [--port <p>]` serves the MCP Streamable HTTP
  transport directly (stdio remains the default, so existing client manifests are
  unaffected). Requires the `mcp-http` feature.

### Changed

- MCP tool results now carry machine-readable `structuredContent` (SEP-2106)
  alongside the human-readable text block, so schema-aware clients get typed
  output regardless of the `format` parameter.
- The Streamable HTTP MCP transport is now stateless by default (SEP-2567):
  `legacy_session_mode` is disabled and `json_response` enabled, with a shared,
  `Arc`-backed task store so tasks remain observable across requests.
- Upgrade `base64` from 0.22 to 0.23, aligning with rmcp 3.0's requirement.

## [1.0.11] - 2026-07-29

### Changed

- Upgrade `rmcp` (and `rmcp-macros`) from 2.0 to 3.0. The MCP server, param, and
  error code is source-compatible with the new major, so no adjustments were
  needed; contract and HTTP transport tests pass unchanged.
- Update the remaining Rust dependencies within range (`schemars`,
  `tokio-stream`, `sse-stream`, `ref-cast`).
- Regenerate all language bindings on alef 0.48.8, which fixes the Swift e2e
  suite (optional `Vec<Named>` metadata fields such as `headings` are
  JSON-bridged to a `RustString` getter and are no longer emitted as
  uncompilable `.count` assertions) and adds a per-RID native runtime project
  for the C# meta+runtime split.

### Fixed

- Refresh the PHP e2e `composer.lock` so `guzzlehttp/guzzle` resolves to `^8.0`;
  the lock still pinned 7.x against the `^8.0` constraint, aborting
  `composer install` before the PHP e2e suite could run.

## [1.0.10] - 2026-07-27

### Changed

- Regenerate all language bindings on alef 0.48.4, which fixes Java (Maven)
  publishing by lowering the maven-enforcer version floor and fixes C# (NuGet)
  publishing by generating a `runtime.json` template rendered at pack time.
- Verify Rust dependencies against their latest incompatible versions; all were
  already current, so no dependency versions changed.

## [1.0.9] - 2026-07-26

### Changed

- Regenerate all language bindings on alef 0.48.2.
- Update dependencies to their latest compatible versions.

### Removed

- Remove unused Java PMD ruleset and stale linter configuration.

## [1.0.8] - 2026-07-20

### Fixed

- **wasm32 builds no longer fail compiling `mio`.** `reqwest` was declared with its
  default feature set (`default-tls`, `http2`, `system-proxy`), which enables
  `tokio/net` → `mio` at the Cargo-manifest level. `mio` has no wasm32 support, so any
  downstream wasm build that pulls crawlberg (e.g. `xberg-wasm`) failed to compile —
  even though reqwest's own code cfg-gates its native transport off wasm. `reqwest` is
  now `default-features = false` at the workspace level, with the native
  TLS/HTTP2/proxy features re-added only under
  `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` in the crates that need
  them (`crawlberg`, `crawlberg-browser`, `crawlberg-bypass`, and the internal
  `benchmark-harness` tool). Native behavior is unchanged; wasm builds get a
  fetch-backed reqwest with no tokio/mio.

## [1.0.7] - 2026-07-19

### Fixed

- **Elixir NIF now builds and publishes.** 1.0.6 could not publish the Elixir
  package — the generated streaming-start NIF cloned the `Arc<RwLock<Handle>>`
  and called a core stream method that does not exist on it (`E0599`), failing
  all NIF builds. Regenerated with alef 0.38.0, the streaming NIF read-locks and
  clones the inner handle first, matching the non-streaming path.
- **Elixir `create_engine/1` no longer double-encodes its config.** The generated
  binding unconditionally re-encoded its argument, so the documented
  `Jason.encode!(%CrawlConfig{})` string form was JSON-encoded twice (serde
  rejected the string) and `create_engine(nil)` became `"null"`. alef 0.38.0
  forwards `nil` and pre-encoded strings as-is, encoding only native maps.
- **Dart `freezed` dev-dependency pinned back to `^3.2.5`.** The 1.0.6 release
  carried a `4.0.0-dev.3` prerelease that requires a newer Dart SDK than CI
  provides; reverted so `dart pub get` resolves the stable release.
  (`packages/dart/pubspec.yaml`)
- **Swift e2e length assertions on JSON-bridged metadata collections compile
  again.** `metadata.headings` / `hreflangs` / `favicons` are `Option<Vec<T>>`
  fields that swift-bridge exposes as a scalar `RustString` (no `.count`), so the
  generated `.length` assertions emitted uncompilable `.count`. alef 0.38.0 skips
  these, matching the other C-ABI backends.

### Build

- Bindings, stubs, READMEs, docs, and e2e suites regenerated with alef 0.38.0
  (up from 0.34.4).

## [1.0.6] - 2026-07-19

### Fixed

- **`map()` / `map_urls()` no longer materialize the entire sitemap tree before
  applying `map_limit`.** The limit previously bounded only the returned slice,
  not peak memory: a large sitemap-index host could drive the process into
  multiple GB and be OOM-killed even with a small `map_limit` set. `map_limit`
  and the `exclude_paths` / `map_search` filters are now compiled once and
  threaded through the sitemap fetch loop — entries are filtered as they are
  parsed, and both child-sitemap fetching and per-child parsing stop once the
  limit is reached. Peak memory is bounded to roughly the limit plus a single
  child sitemap. (`crates/crawlberg/src/map.rs`,
  `crates/crawlberg/src/sitemap.rs`) Closes #33.

### Build

- Refreshed in-major dependencies (`deno_core` 0.408, `uuid` 1.24) and lock
  files.
- Internal maintenance: pruned stale TODO markers, closed remaining todo gaps,
  and added the ai-rulez Poly commit hooks.

## [1.0.5] - 2026-07-09

### Security

- **Per-hop SSRF re-validation on the headless-browser tier.** Closes the known
  limitation noted in 1.0.4: real headless Chrome follows 3xx redirects and
  client-side navigations internally, so only the seed URL was checked. Browser
  fetches now enable CDP Fetch interception for the duration of each navigation
  and validate every request URL (initial navigation, redirects, and
  subresources) against the SSRF policy before Chrome connects. Blocked requests
  are failed with `BlockedByClient`; a blocked main-frame request surfaces as a
  precise `CrawlError::SsrfPolicyViolation` rather than a generic navigation
  error. This brings the chromiumoxide backend to parity with the native
  backend, which already re-validates each redirect hop.
  (`crates/crawlberg/src/browser.rs`)

### Build

- Bindings, stubs, READMEs, docs, and e2e suites regenerated with alef 0.34.4
  (up from 0.31.1). The 0.34.4 scaffold formats generated files in place instead
  of excluding them from poly, and refreshes the `.gitattributes`/`.pubignore`
  scaffolding.

## [1.0.4] - 2026-07-09

### Security

- **SSRF validation on the headless-browser tier.** The browser fallback
  (reached directly via `BrowserMode::Always`/`Stealth`, or via dispatch
  escalation to `Tier::Browser`) navigated `page.goto(url)` without the SSRF
  check the HTTP tier already enforced, so a seed or escalated URL could reach
  loopback, RFC1918, link-local, or cloud-metadata addresses through a real
  browser. The target is now validated against `CrawlConfig::ssrf` — the same
  `deny_private` policy and DNS resolution as the HTTP tier — before any
  navigation. (`crates/crawlberg/src/browser.rs`)

  Known limitation: in-browser redirects and client-side navigations are not
  yet re-validated per hop (that requires CDP request interception); the
  pre-navigation check plus `deny_private` cover the direct and
  DNS-rebinding-on-the-seed vectors.

## [1.0.3] - 2026-07-04

Maintenance release. Migrated pre-commit hooks to poly + mago (dropping prek,
phpstan, and php-cs-fixer), made the `update`/`upgrade` tasks resilient to
per-language failures, and regenerated bindings. Version-only bump synced
across all manifests.

## [1.0.2] - 2026-07-02

Maintenance release. Migrated the toolchain to poly via the shared reusable
validate workflow, upgraded binding dependencies, and regenerated bindings.
Version-only bump synced across all manifests.

## [1.0.1] - 2026-06-29

Maintenance release. Version-only bump synced across all manifests; `.gitignore`
ai-rulez block reorganized.

## [1.0.0] - 2026-06-27

First stable release. Promotes 1.0.0-rc.2; version-only bump synced across all manifests.

## [1.0.0-rc.2] - 2026-06-27

Release candidate 2. Maintenance release with version bump.

## [1.0.0-rc.1] - 2026-06-26

### Changed

- **Renamed the project from `kreuzcrawl` to `crawlberg`.** The crate (`crawlberg`), every
  per-language package, the C FFI symbol prefix (`kcrawl_*` → `cberg_*`), the Go module
  (`github.com/xberg-io/crawlberg`), and the docs domain (`docs.crawlberg.xberg.io`) follow.
- **Rebranded the `kreuzberg` namespace to `xberg`.** npm scope `@kreuzberg` → `@xberg-io`, JVM/Maven
  groupId `dev.kreuzberg` → `io.xberg`, ecosystem links and badges move to `github.com/xberg-io/xberg`
  and the `Xberg.dev` brand, and `KREUZBERG_*` env vars become `CRAWLBERG_*`. The legal entity name
  (`Kreuzberg, Inc.`) is unchanged.

### Fixed

- **Swift publish now creates the `release/swift/<version>` branch carrying the substituted
  XCFramework checksum.** The alef-generated Swift e2e/test-app pins
  `.package(url: …, branch: "release/swift/<version>")`, but the publish workflow only force-moved
  the `v<version>` tag and never created that branch, so SwiftPM could not resolve the package. The
  checksummed commit is now also pushed to `refs/heads/release/swift/<version>`.
  (`.github/workflows/publish.yaml`)

## [0.3.0] - 2026-06-23

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
