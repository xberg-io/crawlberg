# Changelog

All notable changes to crawlberg are documented here.

## [Unreleased]

### Upgrading

- **`CrawlPageResult` gained two fields and rejects unknown ones.** `noindex_detected` and
  `nofollow_detected` are always serialised, and `CrawlPageResult` carries
  `#[serde(deny_unknown_fields)]`, so **a page result serialised by this version is rejected by
  every older crawlberg** — even when both values are `false`. The break is one-directional: an
  older result still loads here, because both fields default to `false`.

  What this affects:

  - A cross-version pipeline that serialises a crawl result on one crawlberg and reads it on
    another. Upgrade the readers before, or with, the writers.
  - A persisted `CrawlCache`: entries written by this version cannot be read back by an older
    build, so a rollback must treat the cache as cold rather than reuse it.
  - Any binding that round-trips a page result through JSON across the FFI boundary
    (`cberg_crawl_page_result_from_json`), where the core and the binding can be at different
    versions.

- **The regenerated bindings add two required `CrawlPageResult` constructor arguments.** Code that
  constructs a `CrawlPageResult` by hand — Swift's `init`, Dart's `const CrawlPageResult({...})`,
  Ruby's `initialize`, the Java constructor, the Python signature — must pass `noindex_detected`
  and `nofollow_detected`. Reading a result that crawlberg returned is unaffected.

### Fixed

- **Four CI gates passed without examining anything.** The vendored-C-header check compared only
  `packages/go/include/crawlberg.h`, the one copy the header generator writes alongside the
  canonical file, leaving the three prebuilt-native copies unchecked; it now discovers every
  tracked `crawlberg.h` from the repository index, byte-compares the generator's own outputs,
  compares the vendored bundles as a normalised declaration stream, and fails on any copy it does
  not classify. The e2e fixture-drift check excluded `python`, `php`, `ruby` and `c` for formatter
  skew; measuring each formatter against alef 0.96.4 showed only `python` had any, so `ruff` is now
  pinned and asserted and all four languages are gated. A pull request stacked on another pull
  request's branch matched no CI workflow's `branches: [main]` base filter and ran none of them
  while showing green checks, so a base-branch guard now fails such a pull request explicitly. The
  hand-maintained docs-site changelog mirror had no check and had lost two `[Unreleased]` entries;
  it is resynced and gated. (#162, #127)

- **A WAF challenge served with 503 or 429 was retried instead of escalated.** WAF detection ran
  only for a 403 and for a 2xx, so a Cloudflare or Akamai interstitial served with 503 became a
  plain server error — and a challenge served with 429 a plain rate limit — before anything looked
  at the response. It was then retried by the same JavaScript-less client that provoked it and
  never reached the browser or bypass tier. A 403, 429 or 503 is now fingerprinted before it is
  turned into an error: a detected challenge is a WAF block and escalates, while a 429 or 503 with
  no WAF signal is unchanged — same error, same message, its status still attached, and still
  retried exactly as `retry_codes` says. Escalation is chosen over retry for a detected challenge
  because re-issuing the identical request only reproduces it. Response headers are checked first,
  so a challenge named by a header costs no body read; only a 429 or 503 whose headers say nothing
  now reads a body that was previously discarded, under the usual `max_body_size` cap. Browser mode
  was never affected: CDP reports its own 200 for a navigation, so it cannot observe a 503. (#169)

- **Links, images and the base address were read from `script`, `style`, `title` and `textarea`
  text, and a comment opener in that text hid the real markup after it.** `tl` has no raw-text
  element handling and parses the contents of these elements as markup, so
  `<script>document.write('<a href="/x">')</script>` added `/x` to the links list and a
  `<base href>` inside title text changed the base for the whole page. In the other direction a
  `<!--` anywhere in script or style text started a comment for the parser, which then swallowed
  every tag up to the next `-->`: real links after the script were missing from the links list
  altogether, not merely mis-resolved. The `<` characters inside raw-text element content are now
  masked in the source before it is parsed — the point at which a browser stops reading markup —
  so link, image, feed, favicon, heading, meta-tag, base-address and `<meta http-equiv="refresh">`
  extraction all see the document a browser sees. Title text and JSON-LD payloads are unchanged
  unless they contain a literal `<`, which valid HTML writes as `&lt;`. Contents of `svg` and
  `math` are left alone, because a browser parses those as markup too. (#124, #125)

- **A redirect in browser mode reported the requested URL.** Chrome follows a redirect itself,
  and the page result kept the URL that was asked for, so relative links on the landed page
  resolved against the wrong path and `final_url` named a page that never served the content. The
  browser backends now report the URL they landed on. In a crawl, that URL passes the same SSRF
  check, robots.txt, path filters and duplicate check as an HTTP redirect target, and a page whose
  landed URL is refused is dropped. (#75)

- **Dropping a crawl stream did not stop the crawl at once.** The crawl noticed the dropped
  receiver only when it next sent a page, so failed fetches kept it starting requests, a fetch in
  flight went on to retry, and a seed still resolving retried to the end. The crawl now stops when
  the receiver goes away: in-flight fetches are aborted, and no later seed of a batch stream is
  fetched. This fixes the Rust stream. The Python binding's generated stream still lets one or two
  requests start after the stream is closed; a later change to the binding generator fixes that.
  (#77)
- **A dropped batch stream still reported every seed it had not started.** The batch went on
  starting each remaining seed, and each one sent a `Complete` with zero pages to the event emitter
  and the event sink for a crawl that never ran. The batch now stops starting seeds when the stream
  is dropped, and a seed it never started reports nothing. (#91)

- **`retry_codes` did not gate error retries.** A 408, 429, 500, 502, 503 or 504 response, and a
  transport timeout, were each retried the full `retry_count` even when `retry_codes` listed other
  statuses; only a status that raised no error of its own was checked against the list. A non-empty
  `retry_codes` is now an allowlist over exactly those failures: one is retried only when the status
  it was raised for is listed, and a timeout that never saw a response carries no status, so it is
  not retried at all. An empty list is unchanged and still retries every rate limit, server error,
  bad gateway and timeout. `map()` and the wasm scrape path now follow the same rule, so with an
  empty list they retry these failures up to `retry_count` instead of never. (#76)

  This narrows retries for any configuration that already sets `retry_codes`, including a list
  written to *add* a status: `retry_codes = [503]`, meaning "also retry 503", now excludes the other
  five, so against a rate-limiting origin its 429 responses are no longer retried. List every status
  you want retried, or leave `retry_codes` empty to retry all of them. The default `retry_count` is
  0, so a configuration that never raised it sends one request either way and is unaffected.

- **A 408 was told apart from other timeouts by guesswork.** Every timeout counted as a 408,
  whether or not a response caused it, so a transport timeout was retried under
  `retry_codes = [408]`. An error raised for a response status now carries that status, and
  `retry_codes` matches only that. (#92)
- **`crawl()` and `scrape()` returned a 504 as a page.** The HTTP fetch treated a 504 as a
  success on these paths, while `map()` already reported it as a server error, so an empty
  `retry_codes` did not retry it and a gateway timeout page reached callers as content. Every
  path now maps a status to the same error, so a 504 is a server error everywhere and is
  retried like a 503. The messages of these errors on `map()` now match the other paths:
  `timeout`, `service unavailable` and `gateway timeout`. (#76)
- **A crawl ignored the page's own robots instructions.** With `respect_robots_txt` on, a crawl
  now leaves the links of a page marked `nofollow` (by its robots meta tag or any of its
  `X-Robots-Tag` headers) unfollowed. A link marked `rel="nofollow"` is still followed, because
  it is a hint and not a robots directive. A `noindex` page is still crawled and its links
  followed. Each page result now reports both directives in `noindex_detected` and
  `nofollow_detected`. With `respect_robots_txt` off, nothing changes. See
  **Upgrading** above for the wire-format consequence of the two new fields. (#135)
- **Only the first `X-Robots-Tag` header was read.** A response that sent the header twice had a
  `nofollow` or `noindex` in the second one ignored, and `scrape()` reported only the first value.
  Every header now counts, and `x_robots_tag` reports them joined with `, `. (#135)
- **An image embedded in the page copied its whole encoded data into the markdown.** An
  `<img>` whose address is a `data:` URL wrote the full payload into the text, so one inline
  icon added kilobytes of unreadable characters. The markdown now keeps the image's alt text
  and leaves the address empty, as in `![icon](<>)`. A lazy-load attribute or `srcset` with a
  real URL is still used in its place. `fit_content` follows the same rule. The markdown
  also reads `script`, `style`, `title` and `textarea` text as text, the way link extraction
  does: a `<base href>` inside title text no longer changes where the markdown's links resolve,
  and a `<!--` inside script text no longer leaves the links and images after it untouched. (#97)
- **Link-shaped text in a page title was rewritten.** `<title>use <a href="x.html"> tags</title>`
  got a full address in its front matter title, because the rewrite of relative links read the
  title's text as markup. The contents of `<title>`, `<textarea>`, `<script>`, `<style>`,
  `<xmp>`, `<iframe>`, `<noembed>`, `<noframes>`, `<noscript>` and `<plaintext>` are text, as a
  browser reads them, and now stay as written. (#102)
- **The front matter showed character references in the base address.** A page with
  `<base href="https://example.com/it&#x27;s/">` got `base: https://example.com/it&#x27;s/`. The
  front matter now shows the decoded address, `https://example.com/it's/`. (#103)
- **A `<graphic>` embedded in the page copied its whole encoded data into the markdown.** The
  `data:` rule for `<img>` now covers the addresses of `<graphic>` too. (#113)

### Added

- `CrawlEngineBuilder::document_filter` lets a Rust consumer decide document materialization from
  the response bytes rather than the declared MIME type alone. The predicate receives the
  normalized MIME type, at most `document_max_size` bytes of the already bounded body, and the
  decision `document_mime_types`/the built-in classification would have reached, so it can widen
  that decision (`by_declared_mime || bytes.starts_with(b"%PDF")`) instead of replacing it.
  `crawl()`, `scrape()` and the wasm crawl loop all honour it. With no predicate the declared-MIME
  decision is unchanged.

  The predicate runs for every fetched response, an ordinary HTML page included, so one that
  returns `true` for HTML materializes every page as a `DownloadedDocument` — duplicating its whole
  body into the result and writing it to `document_output_dir` on native targets. Keep it as narrow
  as the documents it is meant to admit. (#95)

- **Relative links in page markdown pointed nowhere.** The markdown kept each address exactly
  as the HTML wrote it, so `rel/child.html` could not be followed outside the page, and a
  `<base href>` had no effect. Relative addresses now resolve against the page's `<base href>`
  or the URL that served the page, the same base the `links` list uses. This covers `<a href>`;
  `<img>` `src`, `data-src`, `data-lazy-src`, `data-original`, `data-srcset` and `srcset`;
  `src` on `<iframe>`, `<video>`, `<audio>` and `<source>`; `<blockquote cite>`; and the
  addresses of `<graphic>`. Character references in an address are decoded first, so
  `&#x2F;app` resolves to `/app`. Absolute URLs, fragment-only links and `mailto:`,
  `javascript:` and `data:` links stay as written. Because resolved links are longer,
  `fit_content` can now drop a line of relative links that it kept before, the same way it
  already treated absolute links. (#63)
- **The markdown front matter showed the base address as written.** A page with
  `<base href="/other/">` got `base: /other/`. The front matter now shows the resolved base,
  the same address that relative links resolve against. (#94)

### Internal

- **A test now fails if `html-to-markdown-rs` resolves to 3.15 or newer.** 3.15 added a `base_url`
  conversion option that resolves relative addresses the same way the pre-pass above does, and the
  caret requirement admits it on a routine `cargo update` with nothing to compile against and
  nothing to fail — leaving two resolvers in the crate and no sign of it. Adopting `base_url` and
  deleting the pre-pass is the intended end state, but it is deliberately deferred: `base_url`
  resolves an empty `src` to the page URL and rewrites fragment-only links, neither of which the
  pre-pass does. (#190)

- **Teardown no longer shuts down an external Chrome.** With `browser.endpoint` set, crawlberg
  connects to a Chrome it did not start, and every teardown sent that Chrome a `Browser.close`: a
  one-shot fetch, `interact()`, and a browser pool shutdown. Crawlberg now closes only the tabs it
  opened and disconnects from a browser it connected to. A Chrome that crawlberg launched is still
  closed as before. (#73)

## [1.8.0] - 2026-09-25

Twelve issues raised by an external evaluation, ten of them in the crawl path. Most were defects a
green e2e suite could not see: the fixtures covering the affected behaviours passed with the bugs
fully present, and the assertion vocabulary cannot express request counts or elapsed time at all,
so the whole "how many requests did we send, and how long did we wait" class was invisible by
construction.

### Upgrading

Four changes can affect an existing setup:

- **`interact()` now enforces the SSRF policy.** It previously enforced none on the default browser
  backend, so a target `ssrf.deny_private` should have rejected was fetched anyway. Code that
  relied on reaching a loopback or private address through `interact()` must now opt in
  deliberately, the same way `scrape()` and `crawl()` already required. (#74)

- **Saved browser profiles.** Default Chrome flags now actually reach Chrome (see below), so
  cookies in a `browser_profile` written by 1.7.2 or earlier may no longer be readable: they were
  encrypted with a keychain-backed key and the mock keychain uses a different one.
- **`BrowserConfig` gained two fields and rejects unknown ones.** A configuration serialised by
  1.8.0 that carries `overall_timeout` or `shutdown_timeout` is rejected by older crawlberg
  versions. Older configurations still load unchanged.
- **`CrawlPageResult.normalized_url` now normalises the post-redirect URL** rather than the
  originally discovered one, so it keys on where the content actually came from. This also feeds
  `CrawlResult::unique_normalized_urls()`.

### Added

- `ContentConfig.extract_metadata` leaves the YAML frontmatter out of a page's markdown when set
  to `false`. The head values remain available on `PageMetadata`, which is populated independently
  of the converter. (#64)
- `CrawlConfig.path_patterns_match_query` matches `include_paths`/`exclude_paths` against the path
  and query (`/blog?p=42`) instead of the path alone. Path-only stays the default, because a
  pattern anchored with `$` changes meaning once the query joins the text. (#61)
- `CrawlConfig.dedup_include_query` keeps the query in the dedup key, with its parameters sorted,
  so `/item?id=1` and `/item?id=2` are no longer one page. `strip_tracking_params` and
  `tracking_params` remove tracking parameters from the URL that is fetched and reported, not only
  from the key. (#65)
- `CrawlConfig.retry_initial_delay_ms`, `retry_max_delay_ms` and `rate_limit_jitter_ratio` make the
  first retry delay, the backoff ceiling and the per-domain delay jitter configurable. (#67)
- `BrowserConfig.overall_timeout` and `shutdown_timeout` bound a browser fetch end to end. (#66)
- `CrawlPageResult.final_url` and `redirect_count` report where a page's content came from and how
  many hops it took. (#62)
- The Python release now publishes a macOS x86_64 wheel, so an Intel Mac no longer falls back to
  building the sdist. It carries a deployment target of 11.0, matching the existing arm64 wheel.
  (#57)

### Fixed

- **`allow_subdomains` had no effect.** Every cross-host link was dropped as external before the
  host-scope check ran, so a link to a subdomain of the start host was never requested. The scope
  decision is now one helper shared by both crawl loops. (#60)
- **A redirect on a discovered link was not followed.** Only the start URL resolved its redirect
  chain; a discovered link answering 3xx was reported as a page with an empty body and its target
  was never requested. Frontier fetches now resolve redirects with robots, `exclude_paths` and SSRF
  enforced on every hop. Relative links on a redirected page also resolve against the final URL
  instead of the pre-redirect one, which was wrong whenever a redirect crossed origins. (#62)
- **`retry_count` did not bound requests.** Two retry loops ran nested and the outer one never read
  the setting, so a URL answering 503 was requested `4 * (retry_count + 1)` times: 4, 8 and 20 for
  `retry_count` 0, 1 and 4. Retries now have a single owner. Backoff existed in six disagreeing
  implementations, including one uncapped shift reachable from an unvalidated `usize`; they now
  share one function and `retry_count` is bounded. (#67, #68)
- **A browser fetch could wait without a limit.** `BrowserConfig.timeout` covered only navigation
  and the ready wait, leaving page creation, setup, content extraction and shutdown unbounded — and
  a completed page result was not returned until Chrome exited, so a Chrome that would not exit
  held a finished result. One deadline now covers the whole fetch, shutdown no longer blocks the
  result, and the browser is killed if it does not close in time. (#66)
- **Default Chrome flags never reached Chrome.** The one-shot launch path passed them with a `--`
  prefix that chromiumoxide prefixes again, so Chrome received `----no-first-run` and ignored it.
  On macOS a crawl no longer shows a keychain prompt, because `--use-mock-keychain` is now among
  the defaults and actually applied. (#59)
- **Cross-host document links stopped being followed.** The fix for #60 applied the host-scope
  rule to every link type, but `classify_link` matches a file extension *before* it compares hosts,
  so a cross-host `.pdf`/`.docx`/`.zip` link is a document link rather than an external one, and
  every earlier version followed it by default. Documents served from a CDN or object store were
  silently dropped. They are followed again, and this settles what `stay_on_domain` means: it
  governs document links, which is the one thing it has ever actually done. Its `false` default is
  unchanged, so no existing configuration behaves differently. (#72)
- **`interact()` enforced no SSRF policy on its default backend.** Neither the pre-flight URL check
  nor the per-request interception that `scrape()` and `crawl()` apply was installed, so
  `ssrf.deny_private` — on by default — had no effect, and a page could reach loopback, private
  ranges or cloud instance metadata from the crawler's network position. The native backend was
  never affected. Both defences are now in place, validated once for both backends. Pre-existing
  rather than introduced here. (#74)
- **`crawl()` ignored `remove_tags`.** The setting was folded into the markdown converter's exclude
  selectors on the scrape path only, so a crawl kept elements a scrape of the same page removed.
  Both paths now share one merged configuration. Pre-existing rather than introduced here.
- **`browser-chromiumoxide` without `browser` did not compile.** The interact launcher called into a
  module gated on the wider feature. No CI job built that configuration; one now builds all
  fifteen. (#70)
- **A fully successful release reported failure.** The job that pushes the Go module's subdirectory
  tag checked the repository out at a tag that the Swift checksum job force-moves in the same
  second, and died in `actions/checkout`. It now creates the tag through the API, with no working
  tree and no tag fetch. (#71)

### Changed

- Repinned Alef to 0.96.2, which corrects two defects in the generated Python bindings.
- Upgraded `deno_core`, `utoipa` to 6 and `saphyr`, then upgraded the OpenTelemetry stack to 0.33
  (`tracing-opentelemetry` 0.34) once `liter-llm` 2.1.0 published with a matching floor, along with
  `serial_test` 4 and `sysinfo` 0.39. OpenTelemetry 0.33 needed no source change. `cssparser` stays
  at 0.37 because `selectors` 0.40 still requires it.

  Two things are worth recording for anyone repeating this. Before `liter-llm` 2.1, bumping
  OpenTelemetry resolved **both** 0.32 and 0.33 whenever the `otel` feature was on, and
  `cargo check --workspace --all-features` exited 0 in that state — a split graph is a duplicate
  resolution, not a type error, so only reading the lockfile detects it. And `opentelemetry-otlp`
  0.33 turns export retries on by default (exponential backoff with jitter, three retries);
  `init_otlp` does not configure a `RetryPolicy`.
- CI now builds every feature configuration, and runs the binding-parity gate when `alef.toml`
  changes — it was the only workflow whose path filter omitted the file while being the only place
  that gate runs.
- Closed the size and complexity baseline work (#42). Nothing left in the baseline is debt: five
  entries are config or log files, one is generated, and the remaining two are the same defect in
  poly's parameter counting, which counts an attribute on a parameter as a parameter. Reported as
  Goldziher/poly#28.

## [1.7.2] - 2026-09-24

A wasm and kotlin_android correctness release. A wasm engine handle was unusable after one call,
and the kotlin_android e2e suite had never once completed a run. Both traced to the binding
generator, so the fixes arrive via Alef 0.96.0.

### Fixed

- **A wasm engine handle survives more than one call.** Every generated wasm entry point took
  `WasmCrawlEngineHandle` by value, and wasm-bindgen's glue for a by-value exported struct calls
  `__destroy_into_raw()` on it, nulling the JS object's pointer. A second `scrape()` on the same
  engine threw `null pointer passed to rust`, and `batchScrape` consumed the handle identically —
  it only appeared to work because callers used it once. The core API always took a reference and
  the binding body immediately re-borrowed, so the move bought nothing; the Node binding has always
  emitted `&JsCrawlEngineHandle` from the same IR. The emitted `.d.ts` is unchanged, so no
  JavaScript or TypeScript caller needs editing. (#56)

- **Transient robots.txt failures no longer block unrelated crawls for five minutes.** Sharing the
  robots outcome cache across crawls made one `DisallowAll` fail-closed for every crawl of that
  origin and user-agent for the full TTL, and the cache could not tell a DNS blip from a WAF
  interstitial. Timeout, connection, DNS and TLS failures never reached the origin and now expire
  after 15 seconds; 5xx, 429 and WAF blocks keep the full five minutes, because backing off there
  is the intended behaviour. The catch-all stays durable, so an unrecognised failure does not get
  both fail-closed and the shortest memory of it. (#55)

- **The kotlin_android e2e suite completes.** It had never finished a run: a hung native call ran
  to the job's 90-minute cap, which GitHub records as `cancelled` rather than `failure`, so the job
  read as green while testing nothing. Once it completed, 23 failures surfaced that had been latent
  throughout. Generated stream tests deserialized the raw fixture blob into a request DTO that did
  not declare those fields; sealed-class serialization dropped the `type` discriminator, so the
  native layer rejected every `interact` call; and generated enums lacked the wire-value
  `toString()` the assertions compare against. The suite now also logs failures with stack traces
  and times itself out well inside the job cap.

- **The shell formatting CI job runs.** It invoked `shfmt`, which no runner image ships and nothing
  installed, and had exited 127 on every run since 2026-09-15.

### Changed

- **Alef pin moved from 0.93.1 to 0.96.0**, carrying the generator fixes above.

- **`alef.toml` now lists the modules alef parses for the API surface.** The `[[crates]] sources`
  paths are read as files and scanned for type definitions; alef does not walk the module graph, so
  a `pub use` re-export left behind by a file split is invisible to it. `path_mappings` gained
  entries for the same reason: alef derives a type's import path from the file it was found in,
  which after a split is not where the crate re-exports it.

### Notes for wasm callers

`config.content.skipImages = true` does not work, and never did. wasm-bindgen's getter returns a
detached clone, so the natural idiom mutates a throwaway. Read, modify and assign back:

```js
const c = config.content;
c.skipImages = true;
config.content = c;
```

The setter consumes its argument, so build a fresh one for the next edit. The same applies to
`ssrf`, `auth`, `browser` and `proxy`.

### Internal

- `CrawlConfig.content` now has end-to-end coverage. Of 267 fixtures exactly two set it, one with
  an empty assertion list and the other asserting only batch counts, so a `content` that was
  ignored entirely passed green in all sixteen generated language suites. Two matched fixtures now
  pin it from opposite directions, and a Rust-level test asserts the same without depending on
  regenerated suites.

- The size and complexity baseline (#42) goes from 83 findings across 44 files to eight entries,
  each with a stated reason rather than left as debt. Behaviour-preserving throughout: no public or
  crate-visible item changed name, signature, module path or field set. Several of the functions
  restructured had no test that called them at all, so characterization tests were captured against
  the original implementations first.

## [1.7.1] - 2026-09-16

A release-tooling fix. 1.7.0's Java artifact never reached Maven Central; this version carries the
fix and publishes it. No library code changed, so 1.7.0 and 1.7.1 are byte-identical apart from the
version string and the two CI files below.

### Fixed

- Checkstyle's suppressions file is found regardless of where maven is invoked from. `checkstyle.xml`
  named it by a bare relative path, which checkstyle resolves against the process working directory,
  not against `config_loc` — and `optional="true"` meant a miss discarded the entire suppressions
  file in silence. `mvn checkstyle:check` from `packages/java` therefore passed while
  `mvn -f packages/java/pom.xml` from the repo root, which is how the release job invokes it,
  failed on the same bytes. That is what stopped `io.xberg.crawlberg:crawlberg:1.7.0` reaching
  Maven Central. Every suppression in the file had been inert in CI for as long as it has existed.
  The path is now anchored to `${config_loc}` and the filter is no longer optional, so a missing
  file is an error rather than a silent skip.

- The publish workflow fails a run that cannot publish. Every publish job gates on
  `is_tag == 'true'`, so a `workflow_dispatch` carrying a branch as `ref` skips all of them and
  still reports success. Run 35122273610 skipped 68 jobs that way and went green having published
  nothing, which is indistinguishable from a real success in the run list. `is_tag` is true in
  every legitimate mode, dry-run included, so refusing a non-tag ref rejects no real dispatch.

## [1.7.0] - 2026-09-16

The generated bindings move to Alef 0.90.0 and the markdown converter to html-to-markdown 3.14.
Two binding changes are source-breaking, both in generated code and neither visible on the wire.

### Changed

- **BREAKING (java): enum constants are now `SCREAMING_SNAKE_CASE`.** `LinkType.Internal` becomes
  `LinkType.INTERNAL`. 38 constants across 11 enums: `AssetCategory` (10), `BrowserMode` (4),
  `CrawlStrategyKind` (4), `ImageSource` (4), `LinkType` (4), `BrowserWait` (3), `FeedType` (3),
  `BrowserBackend` (2), `ScrollDirection` (2), `ContentFilterKind` (1) and
  `DocumentContentEncoding` (1). The `@JsonValue` string each constant carries is unchanged, so
  nothing serialises differently -- only Java source that names a constant needs editing.

- **BREAKING (swift): `DownloadedDocument` is a native struct, not an alias for the bridge type.**
  It is now `Codable`, `Sendable` and `Hashable`, with Swift-cased stored properties
  (`mimeType`, `contentHash`, `contentPath`, `contentBase64`) and a memberwise initialiser.
  `DownloadedDocumentRef` and `DownloadedDocumentRefMut` remain aliases to the bridge types.

- **Upgraded `html-to-markdown-rs` to 3.14**, which fixes five ways HTML could lose visible content
  on its way to markdown. The one that reaches the widest input is an html5ever serializer defect:
  0.40.0 dropped the leading byte of a two-byte UTF-8 sequence, so a single `§`, `©`, `°` or `·`
  could make a repaired document's re-parse fail and silently truncate everything after it. The
  rest: character references in attribute values are now decoded (`title="A&amp;B"` reached the
  output literally, across twelve attributes); a nested `<table>` behind a wrapper element no
  longer emits raw `|` characters that re-parse as the outer row's cell boundaries; content after
  a table whose last row is never closed is no longer dropped; an `<a>` wrapping a block element no
  longer crushes that block into the link label; and `keepInlineImagesIn` is honoured for `<a>`.

- **Deduplicated the html5ever stack.** 3.14 pins html5ever 0.40, matching this workspace's own
  direct dependency, where 3.12 pinned 0.39 and the graph carried two copies of each crate in that
  stack. `Cargo.lock` loses five duplicate entries -- `html5ever`, `markup5ever`, `string_cache`,
  `string_cache_codegen` and `web_atoms` -- and 57 lines net.

- **The Java binding gains a handle borrow lifecycle.** Alef 0.90.0 emits package-private
  `HandleLease` and `HandleTransfer` types -- `AutoCloseable`, reference-counted and synchronized
  on the handle -- so a native engine handle cannot be closed while a streaming call still holds
  it. No public API changes; `crawlStream` and `batchCrawlStream` are the callers. The two methods
  cross the 150-line `MethodLength` limit as a result, and generated Java is now suppressed for
  that check: its method sizes belong to the emitter, not to this repo.

- **Bumped the Alef pin from 0.85.19 to 0.90.0** and moved the shell-formatter configuration into
  `alef.toml` as `[workspace.poly.shell-formatter]`. It had been hand-edited into the generated
  `poly.toml`, which carries a DO-NOT-EDIT header -- the next `alef generate` would have deleted it
  and returned poly to formatting no shell at all, since poly runs `shfmt` only when a config
  enables it.

### Fixed

- The version bump now refreshes `uv.lock`. `alef sync-versions` rewrites
  `packages/python/pyproject.toml`, but nothing regenerated the workspace lock beside it, so
  every 1.6.x release was tagged with a stale one: 1.6.1 through 1.6.4 all shipped
  `crawlberg 1.6.0` in `uv.lock`, and 1.6.0 itself shipped 1.4.2. `uv sync --locked` and
  `uv run --locked` fail against such a lock.

## [1.6.4] - 2026-09-14

A browser-flag fix, plus two release-infrastructure and test-correctness fixes.

### Fixed

- Pass Chrome command-line flags with a single `--` prefix. chromiumoxide's `BrowserConfig::arg` treats the whole string as the flag key and renders it back as `--{key}`, so every already-prefixed flag reached Chrome as `----flag` and was discarded as unknown. Confirmed in a launched browser's own argv: 21 flags carried four dashes. That silenced every entry of the built-in safe defaults — including `--disable-dev-shm-usage`, the standard workaround for the small `/dev/shm` in CI containers, where Chrome otherwise stalls or crashes — every caller-supplied `chrome_args` entry, and the interact path's `--proxy-server`, so a proxy configured for an interaction was never actually applied.
- The Elixir publish job no longer corrupts `Package.swift` on `main`. It checks out the release tag, and the Swift injection job force-moves that tag onto a commit which rewrites the `__ALEF_SWIFT_CHECKSUM__` placeholder into a literal checksum — so pushing this job's `HEAD` to `main` fast-forwarded `main` through that commit and destroyed the placeholder. The next release then failed with "carries no `__ALEF_SWIFT_CHECKSUM__` placeholder" and published no `release/swift/<version>` branch for SwiftPM to resolve, which is what left 1.6.3 unresolvable on SwiftPM. Whether it happened at all was a race between that checkout and the tag move, so it bit some releases and not others. The checksum commit is now built in a throwaway worktree based on the current `origin/main`, carrying the checksum file and nothing else.

### Changed

- Integration tests reach loopback through `CrawlConfigBuilder::allow_private_networks(true)` instead of writing `CRAWLBERG_ALLOW_PRIVATE_NETWORK` into the process environment. The previous approach was justified by a comment claiming `#[serial]` made it safe; it did not — `serial_test` serialises serial tests against each other and does nothing about a non-serial test calling `std::env::var` at the same moment, and that reader sits on the `CrawlConfig::default` path most of these binaries use. On glibc a concurrent `setenv` can reallocate `environ` underneath a `getenv` and abort the process with no panic, no backtrace and no failing test name. No test now writes the process environment.

## [1.6.3] - 2026-09-12

Redirect targets are now judged before they are requested. `exclude_paths` gains one deliberate behaviour change, described below.

### Changed

- Apply `exclude_paths` to every URL in a seed's redirect chain, not only the URL the chain lands on. A chain that passes through an excluded path is now refused, where it previously followed the redirect and crawled the target.

### Fixed

- Read a redirect target's own robots.txt before requesting it, and evaluate every hop rather than only the URL the chain ends on; each origin's file is read once per crawl. Thanks to @tobocop2.
- Publish each origin's `Crawl-delay` when its robots.txt is first read, so the delay reaches the rate limiter before any request to that origin instead of after the whole chain.
- Refuse a redirect URL whose origin cannot be determined, rather than admitting it: in the component that decides whether a request may go out, a parse failure must not become permission.
- Report the redirects already followed, and keep the cookies they set, when a later hop is refused.
- Report a crawl that stops before its loop begins to `EventEmitter` consumers. Neither `on_error` nor `on_complete` fired for a seed failure, an unreachable robots.txt, or a disallowed seed, so a callback-driven consumer could not distinguish a failed crawl from a hung one.
- Construct `crawlberg-browser`'s test isolates inside a tokio runtime. Without one, deno_core aborts the process from a V8 background thread, which made `cargo test -p crawlberg-browser --lib` fail intermittently under parallel execution with no panic or backtrace.
- Inject the loopback SSRF policy in `crawlberg-browser`'s tests instead of setting a process environment variable, which could abort the process when written while another test read it.
- Add the gnu Rust target before building the Ruby Windows gem, so the platform gem compiles and RubyGems receives the release.

## [1.6.2] - 2026-09-12

Repairs the v1.6.1 release pipeline, which shipped to crates.io, npm, pub.dev, Maven and Hex but missed RubyGems, NuGet and Packagist.

### Fixed

- Report real progress in the `crawl.pages_completed` field of the `crawl.loop.iteration` span during a streaming crawl; it read a buffer that streaming never fills and so was pinned at 0 for every iteration.
- Size the single-seed `crawl_stream` channel from the same default concurrency as every other caller, instead of a hardcoded 4 that matched neither the engine default nor the batch stream beside it.
- Build the Ruby Windows gem against the mingw target the RubyInstaller toolchain actually uses, so rb-sys can generate bindings; the msvc host target made it refuse and took the entire RubyGems release down with it.
- Pack the NuGet package against the runtime identifiers that are actually built. `win-arm64` was declared but never produced, so packing failed on its missing native assets.
- Stop building the PHP extension for Intel macOS, where `setup-php` can no longer provision PHP; one failed leg skipped the whole Packagist target.

## [1.6.1] - 2026-09-12

Robots.txt handling now fails closed. `respect_robots_txt` still defaults to `false`, so only callers that opted in are affected.

### Changed

- Regenerate language bindings, test harnesses, and package metadata with Alef 0.85.19.
- Upgrade html5ever and markup5ever to 0.40, refresh the Rust lockfile, and update the Node, Python, Ruby, and Elixir toolchains; cssparser stays at 0.37 because selectors 0.40 still depends on it.
- Migrate the Node workspaces to pnpm 12 and refresh JavaScript dependencies.
- Refresh the pinned GitHub Actions, correct two pins whose comments named the wrong major, and move setup-uv to 10.1.0.

### Fixed

- Build the robots.txt URL from the seed's origin, so a port-bearing seed reads its own file instead of another origin's, and a seed carrying credentials no longer sends them with the robots.txt request.
- Fail closed when robots.txt is unreachable, a 5xx or a network failure, as RFC 9309 section 2.3.1.4 requires; a 4xx response still means crawl with no rules.
- Treat HTTP 429 on robots.txt as unreachable rather than unavailable, a deliberate divergence from a literal reading of RFC 9309 section 2.3.1.3.
- Treat a WAF interstitial served in place of robots.txt as unreachable, since it is raised for a fingerprint on a 2xx response as well as for a 403.
- Report a fail-closed crawl through the existing `was_skipped` and `error` fields, the error prefixed `robots_unreachable: `, so no public struct gained a field.
- Read robots.txt before the seed request, so a disallowed seed costs the site no requests and `Crawl-delay` applies to the seed as well.
- Fetch the seed once instead of twice by carrying the redirect-resolution response into the crawl as the depth-0 page.
- Report `is_allowed: false` from `scrape()` when robots.txt cannot be read, instead of failing open and parsing HTTP error bodies as policy.
- Honour `respect_robots_txt` in the WebAssembly crawl loop, which previously ignored it and fetched, returned, and followed disallowed URLs.
- Key the browser page loader's robots cache by origin, so two ports on one host no longer share one answer.

## [1.6.0] - 2026-09-10

### Changed

- Update Rust dependencies, including dirs 7 and liter-llm 2.0; retain cssparser 0.37 for selectors 0.40 compatibility.
- Regenerate language bindings, test harnesses, and package metadata with Alef 0.85.14 to correct Java defaults, collection and enum assertions, and truncated mock responses.
- Synchronize package versions and consumer manifests to 1.6.0.
- Preserve custom test harnesses under explicit ownership and check their release pins during version sync.

### Fixed

- Enforce configured HTTP request timeouts on WebAssembly, including redirected requests.
- Resolve native Node packages from the workspace so frozen documentation installs work before publication.
- Export matching canonical and vendored C headers through `task c:headers`.
- Run documentation prose linting correctly and track the shared docs workflow's v1 tag.
- Publish separate NuGet runtime packages and native downloader archives with checksums for Dart and Go.
- Refresh PHP consumer development dependencies to resolve known security advisories.

## [1.5.2] - 2026-09-05

### Fixed

- **The v1.5.1 release published nothing: the plugin version pin was left at 1.5.0 while the
  crate moved to 1.5.1, so `validate-versions` failed and took the whole workflow with it.**
  The bump chain behind `task version:sync` runs `alef sync-versions` and then seven further
  steps that regenerate everything derived from the version -- stubs, scaffolding, README
  install snippets, test-app installers, e2e download scripts, and
  `scripts/sync_plugin_version.py`, which re-pins the coding-agent plugin. Those seven were
  written as `{{.ALEF}} <subcommand>`, but `ALEF` was never defined in `.task/` or
  `Taskfile.yml`, so each one rendered as a bare `generate`/`stubs`/`verify` and the chain
  aborted with exit 127 on the first of them. Only the literal `alef sync-versions` ahead of
  them ever ran. That is why the alef-managed binding manifests tracked the crate while
  `plugin/.ai-rulez/config.toml` -- and the `plugin/package.json`,
  `plugin/gemini-extension.json` and `plugin/kimi.plugin.json` bundles rendered from it --
  stayed behind on 1.5.0. `validate-versions` gates nearly every publish job, so its failure
  skipped 36 of them and no artifact reached any registry. `ALEF` is now defined, so the full
  chain runs and the `alef verify` gate at the end of it is reachable for the first time
  since it was added. Because the release shipped nothing, v1.5.0 and v1.5.1 never reached
  npm; the latest npm release remained 1.4.2, and this is the first 1.5.x to land there.

- **`task version:set VERSION=X` could not set any version other than the one already in
  `Cargo.toml`.** `.task/config/vars.yml` defined a `VERSION` variable computed from
  `Cargo.toml`, and Task resolves `{{.VERSION}}` inside an *included* taskfile from that
  shared file in preference to a CLI-passed `VERSION=...`. Both `version:set` and
  `alef:bump` live in included taskfiles, so both read the computed value instead of the
  argument: `version:set` re-set the version it already had, and `alef:bump` would have
  written the crate version into alef.toml's `alef_version` pin. The `requires: vars:
  [VERSION]` guard never caught it, because the variable was always defined. Root-level
  tasks resolve the argument correctly, which is what made the shadowing easy to miss. The
  computed variable is renamed `CARGO_VERSION`; it had no other readers.

- **Every test app and e2e harness installed crawlberg 1.2.1 -- nine releases behind -- and the
  Zig test app downloaded a `v1.2.1` release asset.** `test_apps/node`, `test_apps/wasm`,
  `test_apps/go`, `test_apps/java` and `test_apps/zig` each pin the published package they exist
  to validate, and every one of those pins had sat at 1.2.1 since 2026-08-11, across 1.3.0,
  1.3.1, 1.3.2, 1.3.3, 1.4.0, 1.4.1, 1.4.2, 1.5.0 and 1.5.1 -- so the suite that answers "does
  the released artifact work" was answering it about a package from three weeks and nine releases
  earlier. `test_apps/zig/build.zig.zon` is the worst case, because its `.url` pointed at
  `releases/download/v1.2.1/crawlberg-zig-v1.2.1.tar.gz` and that asset still returns 200: the
  fetch succeeded, so the staleness never surfaced as an error. (`e2e/go/go.mod` carried the same
  stale pin, though a `replace` directive redirects it to the local tree, so there it was
  cosmetic.) Nothing kept these lines in step because alef classifies all of them as create-once
  seeds -- it writes each only when the path is absent and never re-renders it -- so no
  regeneration step has ever touched them. Adopting the files is not the fix: the same paths hold
  hand-grown build logic (`e2e/zig/build.zig` alone is 903 lines of FFI, rpath and mock-server
  wiring), and adopting a create-once seed consents to alef replacing that content with a
  placeholder on the next overwriting regen. Six `[[workspace.sync.text_replacements]]` entries
  now stamp only the version-bearing line in each file on every `task version:sync`, leaving
  everything around it untouched. The Zig `.hash` deliberately keeps its placeholder value, which
  `zig fetch` resolves once the release publishes.

- **The published TypeScript interaction examples did not type-check.** Every generated
  `interact` snippet and the Node e2e interaction suite read `result.actionResults[0].success`
  directly, but `actionResults` is optional on `InteractionResult`, so all eleven failed `strict`
  type-checking with `TS18048: 'result.actionResults' is possibly 'undefined'` -- a reader who
  copied one out of the docs got a compile error rather than a working example. Regenerating on
  alef 0.84.2 emits `result.actionResults?.[0]?.success` instead. The effect is confined to
  TypeScript and Node: alef gates the fix on the target language at the accessor's entry point,
  and regenerating every backend against 0.84.2 changed no other language's output.

### Changed

- Regenerated all language bindings on alef 0.84.2 (from 0.82.2), picking up its
  reproducible-generation fix.

- Upgraded `vitest` 4 -> 5 across all six Node and WASM suites, and `@vitest/coverage-v8` to the
  matching major, since it is version-locked to vitest. Dev-dependency only -- no published
  package carries vitest, and no shipped code changed.

## [1.5.1] - 2026-09-03

### Fixed

- **A cached HTTP client was reused across tokio runtimes, so requests failed intermittently
  in any process that creates and drops runtimes.** `reqwest::Client` instances are cached
  process-wide, but the cache key carried no runtime identity. hyper drives each pooled
  connection with a task spawned on the runtime that built the client, so when that runtime
  is dropped the connection dies while the client stays cached -- and the next caller, on a
  new runtime, checks out a dead connection and fails mid-request. The failure surfaced as
  `error sending request for url ...` when the connection died during send, or
  `error decoding response body` (classified `data_loss`) when it died while reading the
  body, and neither is retried, since `retry_count` defaults to 0 and only status-derived
  errors are retryable. The cache key now carries the runtime's identity. Measured at ~8.5%
  of requests across 28 short-lived runtimes before the fix and 0% after. This affects
  embedders that create and destroy runtimes -- most visibly every consumer's
  `#[tokio::test]` suite, where it reads as flaky integration tests.

## [1.5.0] - 2026-09-01

### Changed

- Upgraded `html-to-markdown-rs` to 3.12, picking up its Tier-1/Tier-2 GFM autolink parity fix
  and the case-insensitive HTML attribute matching behind the link fix below.

### Added

- Scoop is now a release channel alongside Homebrew: a release publishes a Scoop manifest for the
  CLI, and the install instructions cover it.

### Fixed

- **`batch-scrape` with no URLs reported a different error than every other binding.**
  The positional was `required = true`, so clap aborted at parse time with
  `the following required arguments were not provided: <URLS>...`. Empty input now
  reaches the library, which returns the same
  `invalid_config: batch_urls must not be empty` the Python, Node, Go and other
  bindings already return. `batch-crawl` is unchanged.
- **Links with uppercase or mixed-case attribute names were silently dropped.**
  HTML attribute names are case-insensitive, but the parser matched them byte-for-byte
  as written, so `<a HREF="/docs/guide.html">` yielded no link at all and
  `ReL="nofollow"` was not honoured -- the link was lost, not merely mis-resolved. The
  `astral-tl` 0.8.0 upgrade lowercases attribute keys at parse time. Verified against a
  control build: both cases fail on 0.7.11 and pass on 0.8.0.

## [1.4.2] - 2026-08-28

### Fixed

- **Python e2e configs passed raw dicts where a binding type was required.** The
  `handle_nested_types` map for the Python e2e generator declared `browser`, `proxy` and
  `auth` but not `content` or `ssrf`, so generated tests emitted
  `CrawlConfig(ssrf={...})` and `CrawlConfig(content={...})`. The generated pyclasses
  only extract from real instances, so both raise
  `TypeError: 'dict' object is not an instance of ...` at construction. Python now
  declares the same nested types as its wasm sibling.

### Changed

- Upgraded `deno_core` 0.410 -> 0.411 and `uuid` 1.25 -> 1.26.
- Removed the inert `[overrides.c]` blocks for `crawl_stream` and `batch_crawl_stream`;
  both calls already list `c` in `skip_languages`.

## [1.4.1] - 2026-08-25

### Changed

- Regenerated all language bindings on alef 0.68.0.

## [1.4.0] - 2026-08-24

### Added

- **Bounded LLM extraction concurrency (`ai` feature).** `LlmExtractor` now builds a
  `liter_llm::ManagedClient` instead of a bare `DefaultClient`, with liter-llm 1.18.0's queueing
  `InFlightLimitLayer` wired in. The new `LlmExtractorConfig::max_in_flight` caps simultaneously
  outstanding provider requests globally for the extractor's client rather than per call site, so
  a wide crawl fan-out cannot burst past a provider's per-key concurrency allowance. The bound is a
  dedicated `InFlightBound` enum -- `Limited(NonZeroUsize)` or `Unlimited` -- rather than an
  `Option<usize>`, so neither unsafe state is reachable by accident: the default is
  `Limited(8)`, lifting the bound requires naming `InFlightBound::Unlimited` at the call site, and
  a zero bound (which admits no request at all and would deadlock every extraction) is a compile
  error rather than a runtime `CrawlError::InvalidConfig`. `LlmExtractorConfig` implements
  `Default`, so `..Default::default()` picks up the bounded default instead of silently disabling
  the limiter. `InFlightBound` is exported from the crate root.
  `LlmExtractorConfig::response_cache` optionally puts liter-llm's in-memory response cache in
  front of the provider; cache hits are served without consuming an in-flight permit, so repeat
  pages still answer immediately while the bound is saturated. `LlmExtractor::new` keeps its
  existing signature and picks up the default bound. Enabling `ai` now also enables
  `liter-llm/tower`, which supplies `ManagedClient` and the limiter.

- **`LlmExtractor` is now public API (`ai` feature).** `crawlberg::{LlmExtractor,
  LlmExtractorConfig, LlmResponseCacheConfig}` are exported from the crate root. The extractor had
  been declared as a private `mod llm_extractor;` inside a `pub(crate)` module with no re-export
  and a file-level `#![allow(dead_code)]`, so it and its `ContentFilter` implementation were
  unreachable from outside the crate -- including the in-flight bound above. The `allow(dead_code)`
  is removed; the type is reachable and used. Like the other `defaults` implementations it is a
  Rust-level export only and is not part of the generated language bindings.

### Fixed

- **The published Rust documentation snippets did not compile.** 220 of the 264 generated Rust
  snippets read a `result` binding their own call site had discarded into `_`, so every one of
  them failed to compile with `error[E0425]: cannot find value result`. Regenerating against
  alef 0.67.5 emits `let result = scrape(&engine, &url).await.expect("call failed")` and takes
  Rust snippet failures from 220 to 9; the remaining 9 are stream fixtures that miss a
  `tokio_stream` import.
  The same regeneration fixes the assertion-rendering defect in the Dart and C# snippets (dart
  22 -> 12 failures, csharp 11 -> 9). The broken output had persisted across alef upgrades
  because alef's generation cache is not keyed on the alef version, so `alef generate` replayed
  stale bytes and `alef verify` reported them as fresh.

- **The Java binding no longer flattens a native error into a generic `CrawlbergRsException`.**
  All eight synchronous FFI entry points in
  `packages/java/src/main/java/io/xberg/crawlberg/CrawlbergRs.java` caught `Throwable` and rethrew
  it as `new CrawlbergRsException("FFI call failed", e)`. `checkLastError()` reports the real
  Rust-side failure by throwing `ConversionErrorException`, `CoreErrorException`, `PanicException`
  or `CrawlbergRsException` — all of which are `CrawlbergRsException` subtypes, so every one was
  caught by that generic handler one frame later and re-wrapped. Callers saw `"FFI call failed"`
  with the real message demoted to a cause, and `catch (PanicException e)` could never match
  because the concrete type had been erased. The regenerated code rethrows a
  `CrawlbergRsException` unchanged and wraps only genuinely unexpected throwables. The six
  `*Async` wrappers are unaffected: they wrap in `CompletionException`, which is the documented
  `CompletableFuture` contract and already preserves the cause.

- **A skipped `publish-crates` no longer reads as a passing gate, and a release that published
  nothing can no longer report success.** Six places in `.github/workflows/publish.yaml` gated
  downstream build, publish and release-promotion jobs on
  `needs.publish-crates.result != 'failure'`. That expression is TRUE when the dependency was
  *skipped*, and `publish-crates` skips for two opposite reasons: the version is already on
  crates.io (a re-run or a resumed release, where downstream must proceed) or an upstream gate
  such as version validation or crate packaging failed (where downstream must not). `result`
  alone cannot separate them, so every one of those conditions was gating on nothing --
  tree-sitter-language-pack v1.15.5 promoted a GitHub release to `Latest` with 40+ failed jobs and
  every registry publish skipped, and still reported success.

  A new always-running `crates-gate` job resolves the ambiguity once, into an explicit
  `outcome` (`published` / `already-present` / `not-required` / `dry-run` / `blocked`) and an
  `ok` flag that every consumer now tests instead of `result`. Because the job always runs, its
  outputs always exist; because it never fails, depending on it cannot skip a consumer. The
  legitimate already-published path stays exactly as permissive as before, and only the
  gate-failed path is newly blocked.

  Nothing in the workflow failed a run whose publish jobs were skipped: `release-finalize` and
  `announce-discord` gate on `!contains(needs.*.result, 'failure')`, which is blind to `skipped`,
  so a release that reached zero registries would still be flipped out of draft and announced. Both
  now also require the crates gate, and a new `release-report` job verifies every enabled publish
  target individually, treating `skipped` as a failure unless the target is not enabled for this
  release or its registry probe already found this exact version published.

- **The Ruby gem published on a failed build and bypassed version validation.** `publish-rubygems`
  accepted `needs.ruby-gem.result == 'failure'` and carried `!cancelled()`, so a release in which
  the gem build failed still ran the publish step against whatever artifacts happened to exist.
  Only the `linux` matrix leg keeps the source gem -- the other three legs delete it -- and that
  source gem is the only artifact crawlberg has ever shipped: all 23 versions on RubyGems are
  platform `ruby`, with no per-platform gem in the registry's history. So the accepted `failure`
  never preserved a partial-platform publish; it only allowed a release with no source gem at all.
  The disjunction was not a deliberate choice either -- it entered in a bulk regeneration commit,
  replacing an explicit `== 'success'`. The job now requires `ruby-gem` to succeed. Separately, it
  was reached without `validate-versions` in `needs:` at all. The other publish jobs that omit it
  still inherit the gate, either through a `needs` chain carrying no skip override (`publish-pypi`,
  `publish-packagist`) or through an explicit success check on a job that does carry it
  (`publish-hex`, `publish-homebrew-bottles`); the `!cancelled()` here removed both routes, so the
  version gate could not block a Ruby publish. `validate-versions` is now a dependency and its
  success is required. Dropping `!cancelled()` also restores the default dependency skip, so a
  failed `check-rubygems` no longer lets the publish proceed on an unanswered
  already-published check. The gate now matches `publish-node`, `publish-maven`, and
  `publish-nuget` exactly.

- **Entity-escaped sitemap URLs were truncated to the text after the last entity.** quick-xml
  reports an entity reference as its own `Event::GeneralRef` and splits the element's character
  data around it, so `<loc>https://example.com/s1.xml?a=1&amp;b=2</loc>` arrived as three events.
  The sitemap parsers assigned each text event to the current field instead of accumulating, so
  every piece but the last was discarded and that `<loc>` parsed to `b=2`. Because `&` must be
  entity-escaped in XML, every sitemap URL carrying more than one query parameter was silently
  corrupted -- a truncated string still looks like a successful parse -- so the wrong URLs were
  enqueued and the real ones were never crawled. Both `parse_sitemap_xml` and `parse_sitemap_index`
  now buffer character data across events and commit it on the closing tag, and they resolve the
  reference events themselves, so the escaped character survives rather than vanishing from the
  middle of the value. This covers every text-bearing field -- `loc`, `lastmod`, `changefreq`,
  `priority`, and the sitemap-index `loc` -- and applies to numeric character references
  (`&#38;`, `&#x26;`) as well as the named XML entities. The defect predates the quick-xml 0.42
  upgrade; a probe built against 0.41 truncates identically.

- **The docs advertised musl artifacts that are not published.** The README claimed precompiled
  binaries "across every binding" and linked a platform matrix that did not exist, and
  `RELEASE.md` described the two Node musl packages as an OIDC misconfiguration with a
  trusted-publisher fix. Neither held up against the registries: `@xberg-io/crawlberg-linux-x64-musl`
  and `@xberg-io/crawlberg-linux-arm64-musl` are name-reservation placeholders at `0.0.1` and have
  never carried a release, PyPI has no `musllinux` wheels, and the Go and PHP release assets are
  glibc-only. The cause is not credentials — the `node-bindings` matrix in `publish.yaml` has no
  musl target, so no artifact is built, and the publish step skips platform directories with no
  binary rather than failing. Installation now carries a per-ecosystem musl support matrix stating
  which bindings work on Alpine (CLI, Docker, Rust, Ruby, Java, C#, Elixir) and which do not (Node,
  Python, Go, PHP), why the omission is deliberate, and the Alpine workarounds. It also records the
  two silent failure modes: `npm install` on Alpine exits 0 while installing no native binary,
  because npm skips the unresolvable optional dependency, and `pip` falls back to an sdist build.
  `RELEASE.md` now documents the decision instead of a fix that would publish nothing, and the two
  placeholder package READMEs say they are unpublished rather than describing a binary that does
  not exist.

- **The coding-agent plugin version gate never ran on the commits that cause drift.**
  `plugin/` sat at 1.3.1 while core shipped 1.3.2 and 1.3.3, so every runtime bundle —
  OpenCode, Hermes, Claude Code, Cursor, Codex, Gemini, Kimi, Factory — declared a version
  two releases stale. The checker for this already existed
  (`scripts/sync_plugin_version.py --check`, run by `CI Plugin`), but `ci-plugin.yaml`'s
  `paths:` filter did not list `Cargo.toml`. A release commit bumps `Cargo.toml` and nothing
  under `plugin/`, so the workflow was never triggered and the gate reported nothing rather
  than failing — `CI Plugin` last ran on the 1.3.1 release. `Cargo.toml` and
  `.task/tools/version-sync.yml` are now in the filter, so any core bump re-runs the gate.
  `sync_plugin_version.py` also grew `--expect <version>`, which asserts that core *and* the
  plugin both equal the version being released; `publish.yaml`'s `validate-versions` job runs
  it against the tag, so a drifted plugin now fails the release instead of publishing a bundle
  that lags the version it claims to be. The 13 stale version declarations are re-synced to
  1.3.3.

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

- Upgraded workspace dependencies across semver-incompatible boundaries (`cargo upgrade
  --incompatible`): `quick-xml` 0.41 -> 0.42 and `uuid` 1.24 -> 1.25. quick-xml 0.42 is a breaking
  change — element names now read as `&str` instead of `&[u8]`, and `BytesText::xml_content` is
  infallible rather than returning a `Result` — so `sitemap.rs` matches on string literals and
  consumes the decoded text directly. The `flutter_rust_bridge` (`=2.12.0`) and renamed `getrandom`
  (0.2/0.3) requirements were deliberately left behind; both are pinned on purpose, the latter to
  force the JS backend features into the older getrandom lines pulled in transitively.

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
