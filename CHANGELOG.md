# Changelog

All notable changes to crawlberg are documented here.

## [Unreleased]

### Fixed

- **Two e2e assertions were tautologies rather than URL leaks, and tested nothing.**
  `links_protocol_relative` exists to prove that `<a href="//cdn.example.com/resource">` inherits
  the page's scheme, but asserted only that some link URL contains `//` — true of every absolute
  URL, and satisfied by the fixture's one ordinary `https://example.com/normal` link without the
  protocol-relative pair being resolved at all. It now asserts both resolved forms,
  `http://cdn.example.com/resource` and `http://images.example.com/photo.jpg`, which a passthrough
  of the raw href cannot produce. `strategy_best_first_seed` asserted that `pages[0].url` contains
  `/` — true of every URL, including the `/page1` and `/page2` results the fixture exists to rule
  out. Its seed is the mock origin root and carries no path, so it now asserts `not_contains`
  `/page`, which fails if any non-seed page is crawled first. Both were confirmed to fail under
  mutation before being accepted.

- **The URL being scraped decided its own network error classification.** `network_error_kind`
  keyword-scanned a string built from `reqwest::Error`'s `Display`, which embeds the request URL,
  so every keyword the scan looks for — `dns`, `ssl`, `tls`, `certificate`, `handshake`, `resolve`,
  `lookup`, `timeout`, `proxy`, `connect` — could be supplied by the path or hostname being fetched
  rather than by the failure. Measured against a refused TCP connection: `/blog/dns-explained`
  reported `dns:`, `/blog/ssl-explained` and `/blog/certificate-pinning` reported `ssl:`, and
  `/blog/timeout-tuning` reported `timeout:` — all four were `connection refused (os error 61)`.
  The scan now runs over `chain_without_request_url`, which `3afbde890` had applied only inside the
  data-loss predicate. The one classification this changes in the fixture suite is
  `error_invalid_proxy`: it was tagged `[network:proxy]` purely because its path spells "proxy",
  while its actual chain is `tcp connect error ... connection refused`, and it is now tagged
  `[network:connection]`. The `CrawlError` variant is `Connection` either way — `NetworkErrorKind::Proxy`
  has always mapped to `connection_with_source` — so the fixture's `connection` assertion is
  unchanged and still correct.

  A refused proxy CONNECT names no proxy anywhere in its chain, so `[network:proxy]` was in practice
  reachable only through the request URL. The unit test that accepted either tag is replaced by one
  that pins `[network:connection]`, alongside a table-driven test that walks every scanned keyword.

- **Two more e2e assertions matched a substring of their own request URL.** Same defect class as
  `error_unsupported_scheme`: every classified network error embeds the URL, and the URL embeds the
  fixture id, so an assertion whose expected substring also occurs in the id passes on the address
  rather than the behaviour. `error_data_loss_truncated` asserted `data_loss` while actually
  returning `connection: [network:connection] ... /fixtures/error_data_loss_truncated` — its mock
  route declares a `content-length` its body does not satisfy, which panics hyper 1.9.0 in the
  generated mock server, and the test passed 5/5 anyway. It now asserts `data_loss:`, a prefix no
  URL path segment can carry. `error_empty_batch_urls` asserted `urls`, which its own id supplies;
  the fixture was long ago repurposed to a 404 case (`mock_responses` is empty and the description
  says so), so it now asserts `not_found`, matching what it actually exercises.

  `error_data_loss_truncated` is consequently red, and stays red: like its sibling
  `error_partial_response` it needs a mock server that can emit an unknown-length or truncated
  body, and both alef-generated harnesses build every response as a known-length `Body::from`.
  A red test that names a real gap is the correct state; loosening the assertion would only
  restore the false pass.

- **`CrawlError::DataLoss` was unreachable for the case it names, and network classification
  keyed off the request URL.** `classify_reqwest_error` only reached its data-loss branch under
  `NetworkErrorKind::Other`, but hyper renders a truncated body's `IncompleteMessage` as
  "connection closed before message completed", so `network_error_kind`'s generic
  `contains("connection")` arm claimed every truncated body first — a response cut short against
  its declared `content-length` came back as a plain connection failure. Worse, the string those
  heuristics scan is built from `reqwest::Error`'s `Display`, which embeds the request URL, so a
  path such as `/blog/dns-explained` or `/fixtures/error_data_loss_truncated` decided its own
  classification. The data-loss predicate now runs for `Connection` as well as `Other`, and it
  matches against the chain with the request URL removed. Covered by
  `truncated_body_produces_data_loss_prefix` (a raw socket that under-delivers its
  `content-length`) and `a_url_spelling_truncated_is_not_a_data_loss` (a refused connection whose
  path spells the keyword).

- **The PHP e2e format hook pointed at a php-cs-fixer that no checkout has.**
  `[crates.e2e.format].php` invoked `../../vendor/bin/php-cs-fixer`, but `vendor/` is gitignored and
  the repo-root `composer.json` never declared `friendsofphp/php-cs-fixer` — only the lock file did.
  So no fresh checkout (every CI runner, and this one) could resolve the binary, and alef's format
  hook silently no-ops on a missing command: regeneration rewrote all 31 generated PHP files with
  alef's raw, over-indented template output and reported nothing. Declared
  `friendsofphp/php-cs-fixer` in `require-dev` (pinning to the v3.95.1 the lock already carried, so
  no dependency churn) and rewrote the hook to `composer install` first and invoke the tool through
  `composer exec`, which resolves from the repo-root manifest regardless of cwd. The php e2e job
  already installs `composer`, so this now resolves in CI rather than only on a developer machine.

- **Four SSRF/scheme fixtures asserted against the mock server address instead of the address under
  test.** `validation_ssrf_loopback_denied`, `validation_ssrf_ipv4_mapped_ipv6_denied`,
  `validation_ssrf_nat64_loopback_denied` and `error_unsupported_scheme` each declare an `input.url`
  that *is* the subject of the assertion, but every backend's `mock_url` argument discarded it and
  substituted the per-fixture mock server address. All three SSRF fixtures therefore exercised the
  same trivial IPv4-loopback case, and the IPv4-mapped-IPv6 (`::ffff:127.0.0.1`) and NAT64
  (`64:ff9b::7f00:1`) normalization paths in `crates/crawlberg/src/net/ssrf.rs` had no e2e coverage
  in any of the 16 generated language suites. Set `preserve_input_urls: true` on those four fixtures
  so the declared addresses reach the call verbatim.
- **`error_unsupported_scheme` asserted on a substring of its own fixture id.** With the mock server
  URL substituted, the error text contained `.../fixtures/error_unsupported_scheme`, so
  `contains("unsupported")` matched regardless of what actually failed. With the real
  `gopher://invalid.example.com/` URL, crawlberg returns
  `ssrf_policy_violation: gopher://invalid.example.com/ - disallowed scheme: gopher`, not an
  `Unsupported` error — the old assertion does not hold. Tightened the assertion to
  `disallowed scheme: gopher`, which names the rejection the fixture exists to cover.
- **`cargo test -p crawlberg` could not compile.** `crates/crawlberg/tests/test_interact.rs` matched
  on `CrawlError::unsupported(message)` — the macro-generated constructor function, not the enum
  variant — which is E0164 (`fn` calls are not allowed in patterns). Since `browser-chromiumoxide`
  is off by default, the `#[cfg(not(feature = ...))]` test was always compiled and always failed the
  build. Replaced with the `matches!(..., Err(CrawlError::Unsupported { message, .. }) if ...)` idiom
  used elsewhere in the file.
- **Redirect-cycle detection missed the first return to a bare-origin seed URL.** In
  `follow_redirects` (`crates/crawlberg/src/engine/crawl_loop.rs`), the cycle-detection `seen` set
  was seeded with the caller's raw URL string, while every subsequent hop key came from
  `resolve_redirect`'s WHATWG-serialized `Url::join` output. A chain seeded at a bare origin (e.g.
  `http://host:port`, no trailing slash) that redirects back to `/` produced a hop key of
  `http://host:port/` — never equal to the raw seed — so the cycle was missed on its first return
  and only caught one hop later (`redirect_count == 2` instead of `1`). Added
  `canonical_redirect_key`, used for the seed and for every hop's `contains`/`insert` pair across
  all three redirect mechanisms (3xx `Location`, `Refresh` header, `<meta http-equiv="refresh">`).
- Skipped `redirect_loop` and `redirect_max_exceeded` for the wasm binding
  (`fixtures/redirect/redirect_loop.json`, `fixtures/redirect/redirect_max_exceeded.json`): wasm's
  `fetch` follows redirects transparently with no manual hop tracking, so `max_redirects` is never
  enforced there and a genuine cycle exhausts the browser's own redirect budget and errors instead
  of stopping at one hop.

### Changed

- Bumped the declared `liter-llm` minimum from `1.17` to `1.18` (resolved: 1.18.0) now that the
  in-flight limiter is used, and refreshed `html-to-markdown-rs` to 3.11.4 in the lockfile.
  liter-llm 1.18.0 is a breaking change shipped as a minor — `EmbeddingProvider::embed` takes
  `&EmbeddingInput` rather than `&str`, and `VectorMetadata` gained an `image_url` field — but
  crawlberg references none of those three surfaces, so the upgrade is source-compatible here.

- Added `[crates.e2e.snippets]` (`output = "docs-site/src/snippets/generated"`) to `alef.toml`,
  matching the config shape tree-sitter-language-pack and html-to-markdown use for the alef
  doc-snippet migration. `[workspace.docs.snippets].dirs` still points at the flat, hand-written
  `docs-site/src/snippets` tree, deliberately not yet repointed at `generated/`: `alef e2e generate`
  cannot write any snippet today because 263 of 264 `docs`-tagged fixtures leak `MOCK_SERVER_URL`
  mock-harness scaffolding into their would-be snippet body and alef's mock-harness guard aborts
  the whole batch before writing anything. Unlike tslp/h2m, nearly every crawlberg e2e call takes
  a URL, so this needs `preserve_input_urls` + a `$mock_url` placeholder added across the fixture
  set — verified correct in isolation against `fixtures/engine/engine_scrape_basic.json`, but not
  applied repo-wide pending its own review. The 14 hand-written `getting-started/basic_usage.md`
  snippets are unchanged.

## [1.3.3] - 2026-08-22

### Fixed

- **CI Lint's `Validate (poly)` job runs again.** `poly lint .` never reached a crawlberg finding:
  golangci-lint v2.12.2 (the reusable workflow's default) vendors `honnef.co/go/tools` v0.7.0,
  whose IR builder panics building the Go 1.27 stdlib with
  `buildir: package "poll": unexpected expr: *ast.KeyValueExpr`. Pin v2.13.1, which handles 1.27.

- **CI Lint's `Alef snippets` job can pass.** It ran `alef snippets check --strict`, and `--strict`
  fails the run on every Skip, Unavailable *and* Downgraded result — the exact state `alef.toml`
  documents `strict = false` for while six languages are still annotated `snippet:syntax-only`.
  The command-line flag force-enabled what the config deliberately disables, so the job was
  unpassable. Dropped from the workflow.

- **The Dart snippet validates at `compile` again.** It reported
  `Target of URI doesn't exist: 'package:crawlberg/crawlberg.dart'` because no session resolved the
  local package. A `[workspace.docs.snippets.sessions.dart]` session (cwd `packages/dart`, manifest
  `pubspec.yaml`, `dart pub get` before, explicit `PUB_CACHE`) fixes it; the `snippet:skip`
  annotation added while the session was deferred is gone. It was deferred because any semantic
  `alef.toml` edit rotates the global inputs hash, so it had to land with a full regeneration.

- **CI Rust's `alef verify --exit-code` gate passes.** Unmasked once `deps:check` stopped killing
  the job, it failed on drift that had accumulated unseen: 39 alef-owned files carried no
  provenance marker (`frozen`), and five carried one but are no longer emitted (`orphaned`).

- **alef no longer claims the three hand-written SSRF e2e suites.** Their self-label read
  "These are NOT generated by alef", and alef's ownership predicate
  (`core::hash::content_has_alef_marker`) is a case-insensitive substring match for
  `generated by alef` over the first 10 lines, with no negation handling. All three were therefore
  claimed and reported permanently stale *and* orphaned, while alef could never stamp them.
  Reworded the label; the tests themselves are untouched.

- **Removed two orphaned generated files.** `InvalidInputException.java` survived the deletion of
  its Rust error variant and exists in no other binding, and `crates/crawlberg-py/src/pyproject.toml`
  is a stale copy of `packages/python/pyproject.toml` whose `manifest-path` does not even resolve
  from its own directory. The latter's dead `[workspace.sync].extra_paths` entry is gone too.

- **CI Rust's `Validate Rust` job gets past its first command.** `task rust:lint:check` runs
  `deps:check` first, which hard-fails when `cargo-machete` is missing; nothing installed it, so
  the job died before `cargo fmt`, `clippy` or the fuzz/config checks ever ran.

- **`e2e/dart/test/metadata_test.dart` compiles again.** `favicons` and `hreflangs` on
  `PageMetadata` are `Option<Vec<_>>` in the Rust core, so the generated Dart bindings expose them
  as nullable `List<FaviconInfo>?` / `List<HreflangEntry>?`. The test called `.any(...)` on them
  directly instead of the already-established `?.length`/`!` pattern used elsewhere in the same
  file, which Dart's null safety rejects at compile time. Changed both call sites to `!.any(...)`.

## [1.3.2] - 2026-08-21

### Fixed

- **The release actually publishes.** v1.3.1 was tagged and released but published nothing to any
  registry: the `Validate versions` gate failed on stale `Cargo.lock` files under `e2e/rust`,
  `fuzz` and `packages/ruby/ext/crawlberg_rb/native`, which skipped the crates.io publish job.
  Every language-package build behind it then failed with
  `failed to select a version for the requirement ^1.3.1`, because the publish preparation was
  retrying against a registry version that had never been pushed. Use this version instead of
  v1.3.1, which carries no artifacts anywhere.

- **`pnpm install` succeeds in the WASM and Node test suites again.** The last dependency upgrade
  moved `vitest` to ^4.1.10 (and `@types/node` to ^26) in package.json without regenerating
  `pnpm-lock.yaml`, so CI — where `frozen-lockfile` is on by default — refused to install:
  `specifiers in the lockfile don't match specifiers in package.json`. The lockfiles under
  `e2e/wasm`, `test_apps/wasm` and `test_apps/node` are regenerated.

## [1.3.1] - 2026-08-21

### Changed

- **The engine now drives the crawl through the configured `Frontier`.** `CrawlEngineBuilder::frontier` previously
  accepted any implementation and then ignored the queue half of it: URLs lived in a `Vec` local to the crawl loop,
  so `push`, `pop`, `pop_batch`, `len`, and `is_empty` never ran and a persistent or distributed frontier had no
  effect on the crawl. Discovered links are now pushed to the frontier, and the loop refills a bounded local window
  from `pop_batch`.
- **Global traversal order is now a property of the frontier, not the strategy.** The engine passes its selection
  window — at most `max_concurrent` entries — to `CrawlStrategy::select_next`, so a strategy reorders only what has
  already been popped. `InMemoryFrontier` is FIFO and yields a breadth-first crawl; the new `LifoFrontier` yields a
  depth-first one. `DfsStrategy` alone no longer produces a globally depth-first crawl, and `BestFirstStrategy`
  now picks the highest priority within the window rather than the global maximum. With the default
  `score_url` (inverse depth) that is not an observable difference; with a custom one, visit order changes.
- `crawl.frontier_size` counts the selection window plus the entries pushed to the frontier and not yet popped.
  The meaning — URLs known to be pending — and the value in the default configuration are unchanged.
- A panic inside SSRF validation now fails the crawl instead of being downgraded to a warning and skipping the link.
- Generated bindings regenerated on alef 0.62.8, and alef pinned to 0.62.8.

- All Rust dependencies taken to their latest versions (`cargo upgrade --incompatible` followed by
  `cargo update`): 87 packages changed, two added, six removed, none downgraded. Notable major
  bumps: `ctor` 0.10 to 1.0, `napi` 3.8 to 3.12, `minijinja` 2.19 to 2.24, `diplomat` 0.15 to 0.16,
  `rmcp` 3.0 to 3.1. `cbindgen` 0.29.2 to 0.29.4 changes generated C enum emission to guard on C23.

### Added

- `CrawlConfig::crawl_strategy` (`bfs`, `dfs`, `best_first`, `adaptive`). The strategy
  implementations have always existed but no binding could select one, so every crawl ran the
  breadth-first default. Selecting `dfs` pairs `DfsStrategy` with a LIFO frontier, because
  traversal order is a property of the queue and `DfsStrategy` over a FIFO frontier is not
  depth-first.
- `CrawlConfig::content_filter` (`bm25`) with `bm25_query` and `bm25_threshold`, and a
  `Bm25Filter` export. The filter existed but was not re-exported and no config could reach it,
  so every crawl ran unfiltered. A `bm25` filter without a query is now a config error rather
  than a filter that silently keeps every page.
- `LifoFrontier`, an in-memory frontier that pops the most recently pushed entry, for depth-first crawls.
- `Serialize`/`Deserialize` on `FrontierEntry`, so a frontier backed by a database, a file, or a message queue can
  encode the entry `push` receives instead of maintaining a mirror struct that silently drops newly added fields
  (#40).

### Fixed

- The default crawl is genuinely breadth-first. The engine removed the strategy-selected entry with
  `Vec::swap_remove`, which moves the last element into the vacated slot; since `BfsStrategy` always selects index 0,
  index 0 held the newest URL after the first removal. A seed linking to `a`, `b`, and `c` was crawled as seed, `a`,
  `c`, and `b` was never fetched under a `max_pages` budget (#39).
- Discovered links reach the queue in document order. They were enqueued from a `JoinSet` drained in SSRF-validation
  completion order, leaving sibling order nondeterministic and breadth-first traversal unreproducible (#39).
- A URL selected immediately before the page budget was exhausted is returned to the frontier instead of being
  silently dropped.

- Four e2e fixtures asserted fields that do not exist on the result type, so alef refused to
  generate the suite. `redirect_loop`, `redirect_max_exceeded` and `redirect_to_404` asserted
  `is_error`, and `rate_limit_basic_delay` asserted `rate_limit.min_duration_ms`; both are
  call-level properties rather than response fields. The redirect fixtures now assert real fields
  (`redirect_count`, `pages[0].status_code`, and `error` for the 404 case), and the rate-limit
  fixture carries an explicit `not_representable` marker alongside a real `pages_crawled` check.
  `redirect_loop`'s mock was also wrong: its start URL returned an unrelated 200 while the actual
  redirect cycle sat on unreachable paths, so the fixture never exercised loop detection at all.

- `packages/ruby/ext/crawlberg_rb/native/Cargo.toml` and `e2e/rust/Cargo.toml` now follow the
  project version. Both are alef-owned but were never reached by the version sync, so each release
  left them pinned to the previous version.

### Security

- `h2` advanced to 0.4.18, resolving RUSTSEC-2026-0258 (unbounded empty DATA frames: a peer could
  queue empty frames without limit, risking unbounded memory use or a panic on length overflow).
  Low severity.
- The wasm crawl loop deduplicates through the frontier rather than a loop-local `HashSet`, so a persistent frontier
  no longer re-enqueues URLs it had already crawled. It also no longer discards `mark_seen` failures.
- URLs still being fetched when a crawl stops early are returned to the frontier. They are marked seen at discovery,
  so a persistent frontier that never got them back would blacklist them permanently — never crawled, with no error
  raised and no failure counted.
- A crawl no longer ends on a single short `pop_batch` when the frontier still reports work. Queue-backed frontiers
  legitimately under-deliver (SQS short polling returns 0-N messages from a non-empty queue); the loop now confirms
  with `Frontier::is_empty` before finishing, at most once per completed fetch.
- The `strategy` and `filter` e2e fixtures assert something again. Their `crawl_strategy`/`content_filter` inputs
  named no real config field, so both bfs and dfs fixtures ran the same default strategy and every bm25 fixture ran
  unfiltered; the ordering assertions on top of that were emitted as skipped comments in all 16 languages. The
  `metadata` suite additionally failed to compile once its `article.*`/`response_headers.*` mappings went live,
  because those fields are `Option` and were not declared as such.

## [1.3.0] - 2026-08-13

This release contains a source-breaking change to `CrawlError`. It is a minor bump rather than a major one, so
`cargo update` will pull it into an existing `crawlberg = "1"` dependency — pin to `=1.2.1` if you are not ready to
adapt. Only the Rust crates ship in this release; the language bindings stay on 1.2.1 until their generator is fixed.

### Changed

- **Breaking.** The 17 message-only `CrawlError` variants are now struct variants carrying `{ message, source }`, and
  `SsrfPolicyViolation` gains a `source`. `CrawlError::Timeout(text)` becomes
  `CrawlError::Timeout { message: text, source: None }`; matches and constructions must be updated. Every `#[error]`
  format string is byte-identical to 1.2.1, so `Display` output — and anything keyed on it, including the
  `[network:<tag>]` prefix and the 500/503/504 suffix matchers — is unchanged.
- `Error::source()` now yields the originating error on every variant instead of `None`. This is what makes
  `downcast_ref::<reqwest::Error>()` work again, recovering `is_connect()`, `is_timeout()`, and `.url()` from the
  underlying failure. The source is `Arc`-backed because `CrawlError: Clone` is load-bearing in the retry path.
- `html-to-markdown-rs` moves to 3.11. The full suite passes unchanged, so this release carries no markdown
  output drift.

### Added

- The HTTP cache honours the response's own `Cache-Control` instead of storing any 2xx for a flat TTL. `no-store`,
  `private`, `no-cache`, and `max-age` are respected, with `s-maxage` taking precedence. A crawl cache is shared —
  one entry is replayed to whoever asks next — so storing a `private` or `no-store` response could hand one tenant's
  content to another.
- Conditional revalidation, making good on the `etag` and `last_modified` doc comments that previously promised it.
  A stale-but-validatable entry now earns a 304 for the cost of one bodiless round trip. `DiskCache` no longer unlinks
  a TTL-expired entry, since that entry is exactly what a conditional request needs; the `max_entries` sweep still
  reclaims it.
- `CrawlCache::get_stale`, defaulted to `Ok(None)` so implementations outside this crate keep compiling and simply
  decline revalidation.

### Security

- Closed a DNS-rebinding TOCTOU in SSRF enforcement. `validate_url` resolved the host and checked every answer, then
  hyper resolved it again to open the connection — so the addresses checked were never the addresses connected to. A
  host with `TTL=0` could answer the validation lookup publicly and the connect lookup with a loopback or
  cloud-metadata address. The check now runs inside the resolution hyper actually uses. It is skipped when a proxy is
  configured, because hyper then resolves the proxy host and client-side pinning is impossible through a proxy anyway.
- Configured credentials are now scoped to the origin host across redirects. Redirects are followed manually under
  `redirect::Policy::none()`, so reqwest's own cross-host credential stripping never ran, and every hop reattached
  `config.auth` unconditionally — an open redirect off an authenticated origin handed the caller's `Authorization`
  header to the redirect target. Both redirect drivers were affected. Hostless or unparseable hop URLs fail closed;
  scheme and port are deliberately not compared, since an http→https upgrade does not change the party the
  credentials were issued to.
- The default deny-private SSRF policy now covers RFC 6598 shared address space (`100.64.0.0/10`, which carries
  Alibaba Cloud's metadata endpoint at `100.100.100.200` and Tailscale/CGNAT node addresses) and the IPv6
  unspecified address `::`, the analogue of the already-denied `0.0.0.0/8`.
- A `ProxyProvider` returning an unparseable URL no longer connects directly with no trace. `Proxy::custom` can only
  answer `Some`/`None` and `None` means direct, so failing closed is unreachable from inside it — the bypass is now
  logged instead. The URL itself is deliberately not logged, because the redaction helper returns its input unchanged
  when the input does not parse, which is exactly this branch.
- An unset `max_body_size` is capped at 100 MiB. reqwest is built with gzip and brotli and `Response::chunk` yields
  decompressed bytes, so no cap let a few hundred compressed bytes expand to gigabytes in memory before any
  downstream truncation ran. Enforced at the read site rather than in `CrawlConfig::default`, so a config
  deserialized from JSON or built by a binding that omits the field cannot bypass it. Reading above the ceiling is
  now an explicit opt-in.
- Sitemap index walks are bounded by total fetches, not just depth and per-tier breadth. Those bound the tree's
  shape, not its size: 100 children per tier across 10 tiers is 100^10 fetches, and `map_limit` does not help
  because it bounds URLs returned, so a tree whose leaves are empty or filtered never reaches it and keeps fetching.

### Fixed

- A byte-order mark now outranks the `Content-Type` charset, as the WHATWG sniffing algorithm requires. When the two
  disagreed the body was silently corrupted — a stale `charset=utf-8` header on a real UTF-16 body replaced every
  non-ASCII character with U+FFFD across html, metadata, links, and markdown, with no error raised.
- robots.txt user-agent groups match in one direction only, as RFC 9309 specifies. Accepting the reverse let the UA
  `crawlberg` claim a group written for a more specific bot such as `crawlberg-news`, silently substituting that
  bot's rules for the `*` block meant for us.
- `DiskCache::set` no longer reports success for writes that never happened. It returned `Ok(())` before writing
  whenever the eviction scan's `read_dir` failed, so a cache directory deleted at runtime made every subsequent write
  a silent no-op for the life of the process. The scan now degrades to writing without evicting. Related: a
  concurrent eviction between `exists()` and `read_to_string()` is an ordinary miss rather than an error, and a
  panicking write task propagates instead of reporting success.
- Browser pool teardown is guarded against runtime-less drops and leaks. `tokio::spawn` panics with no active
  runtime, and `PooledPage`/`PooledSession` cross an FFI boundary into host GC and finalizer threads, so a late drop
  could abort the embedding process; both `Drop` impls now spawn only via `Handle::try_current()`. Discarding the
  handler-shutdown timeout also leaked one CDP handler loop per relaunch.
- The wasm crawl loop no longer traps at engine construction. `Instant::now()` compiles for
  `wasm32-unknown-unknown` but its backend traps with `unreachable` at runtime, and `PerDomainThrottle::new()` called
  it from `CrawlEngineBuilder::build()` — so every wasm scrape and crawl died there. The published
  `@xberg-io/crawlberg-wasm` was broken for real users, not only in tests.
- The wasm crawl loop honours `max_links_per_page` instead of a hardcoded 10,000 cap, and matches native on URL
  dedup and link counting. Its dedup key omitted the `//` path collapse, so the two targets disagreed on which URLs
  were duplicates, and its link cap counted raw anchors rather than enqueued links, so a page whose first N anchors
  were external or already seen discovered nothing on wasm and everything on native.

## [1.2.1] - 2026-08-11

**1.2.0 did not publish completely — use this release instead.** Its publish run failed partway: `crawlberg` never
reached crates.io (it stayed at 1.1.4), and the kotlin-android and WASM packages were never published. Only
`crawlberg-browser` 1.2.0 made it to crates.io. Everything listed under 1.2.0 below ships here.

### Fixed

- The crate now compiles under default features and for `wasm32-unknown-unknown`. `interact`'s screenshot encoder was
  compiled unconditionally while all of its call sites are behind a browser feature, and a `PathBuf` import was unused
  on wasm32. Under `-D warnings` both were hard errors, which broke the kotlin-android native builds, the WASM package
  build, and `cargo publish`'s tarball verification — the latter is why 1.2.0 never reached crates.io.

### Performance

- Response bodies and headers are no longer cloned for hooks that are not configured. The per-attempt
  `HttpResponse` handed to the WAF classifier and antibot strategy (two full-body copies plus a header-map deep copy)
  is now built only when one of them is actually present, and the retry loop's fallback response is moved rather than
  cloned.
- The WAF classifier is built once per process instead of once per response. It previously re-parsed the embedded
  fingerprint corpus and rebuilt its matcher set on every robots.txt, asset, sitemap, and wasm page fetch.
- `http_fetch` walks the response header map at most once instead of up to three times per response.

## [1.2.0] - 2026-08-11

### Added

- `SsrfPolicy.allowlist` (`HostMatcher`) is now exposed to every language binding via a binding-safe tagged
  representation (`exact` / `suffix` / `cidr`). Allowlist entries permit access regardless of the default denylist.
  Closes #37.
- `CrawlConfig.ssrf_deny_private_explicit` lets a caller pin `ssrf.deny_private` to an explicit value so it is no
  longer consulted from `CRAWLBERG_ALLOW_PRIVATE_NETWORK`, removing the ambiguity between a caller who means
  `deny_private: true` and a binding whose struct default happens to land there.
- `CrawlConfig.max_links_per_page` bounds how many links are enqueued from a single page. Links past the cap are
  dropped and a warning is logged.
- `CrawlConfig.document_output_dir` writes downloaded document bytes to disk (`<dir>/<content_hash>.<ext>`) and drops
  them from the result, populating `DownloadedDocument.content_path` instead of `content`. No effect on wasm32 (no
  filesystem).
- `CrawlConfig.document_content_encoding` (new `DocumentContentEncoding` enum) opts a downloaded document's bytes
  into `DownloadedDocument.content_base64` for bindings that need an in-memory, serializable copy. Off by default:
  base64-encoding a document by default would duplicate an already up-to-`document_max_size` buffer (50 MB default)
  in memory per document.
- `CrawlConfig.capture_screenshot` (scrape-only, chromiumoxide-only) captures a base64-encoded PNG screenshot of the
  page. `CrawlConfig.browser_profile` (chromiumoxide-only) selects a named browser profile for persistent sessions
  (cookies, localStorage).

### Changed

- JS evaluation paths (`ExecuteJs` interactions and `eval_script`) now run under a timeout, so a hung script can no
  longer permanently burn a worker slot or hang the isolate.
- Credentials are redacted before reaching tracing spans, SSRF-violation error messages, and `Debug` output —
  `ProxyConfig` and `AuthConfig` no longer leak `user:pass@` in errors or logs.
- Idle per-domain rate-limiter and EWMA domain state now expire on a TTL instead of accumulating unboundedly for
  long-running processes that crawl many distinct domains.
- Document persistence now writes via `tokio::fs` instead of blocking `std::fs` on the async document-download path.
- Bindings regenerated on alef 0.60.0.

### Fixed

- E2E fixtures use Alef's canonical `brew` language identifier, allowing strict fixture-driven generation to proceed.
- Dart: the native loader downloads and caches the library again on a cold cache. It only read
  the versioned cache and then threw a `StateError`, even though `nativeDownloadAndCacheLibrary()`
  was defined and exported for exactly that case. The loader also now searches for the
  `_dart`-suffixed cdylib that is actually built, opens every candidate by absolute path (a
  hardened runtime rejects a relative `dlopen`), and names the real environment variable in its
  error message instead of printing the identifier `$nativeLibDirEnv` literally. Fixed upstream in
  alef 0.55.6.

  Behavior change: an unresolvable native now throws a descriptive `StateError` naming the asset
  URL and the download command, where it previously returned `null` and let flutter_rust_bridge
  attempt its own relative-path `dlopen`.

## [1.1.4] - 2026-08-05

### Fixed

- The Dart package resolves its native library from its own installed location
  rather than a path derived from the crate name, so loading works from any
  working directory and under hardened runtimes (alef 0.54.x).
- CI runs poly's whole-project lint phase. It was skipped entirely, so
  `golangci-lint`, `rubocop`, `steep`, `dart-analyze`, `credo` and `checkstyle`
  ran in the git hooks only and CI never saw them.
- The Rust unit-test script no longer hides failures. A single
  `if ! { cmd1; cmd2; } | tee log` suppressed `set -e` and collapsed the exit
  status onto the last command, so ten failing test binaries reported green for
  weeks. Each cargo invocation is now checked via its own `PIPESTATUS`.

### Changed

- Regenerated all language bindings on alef 0.55.0.

## [1.1.3] - 2026-08-04

### Changed

- Regenerated all language bindings on alef 0.51.2 and updated dependencies.

### Fixed

- Ruby: the gem no longer publishes its generated types into the global `Object`
  namespace (the `Parser` collision with the `parser` gem); generated types stay
  namespaced under `Crawlberg` (tree-sitter-language-pack #173, via alef 0.51.1).

## [1.1.2] - 2026-08-01

### Added

- `cargo binstall crawlberg-cli` support — prebuilt CLI binaries can now be installed
  directly from GitHub Releases without compiling from source. Adds
  `[package.metadata.binstall]` to the CLI crate plus a release-time `verify-binstall`
  CI job that installs via `cargo binstall` and smoke-tests the binary across the target
  matrix.

### Changed

- Updated dependencies.

## [1.1.0] - 2026-07-31

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

### Fixed

- The publish workflow no longer leaves the Homebrew formula pointing at a stale bottle when a
  release republishes the CLI.

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
