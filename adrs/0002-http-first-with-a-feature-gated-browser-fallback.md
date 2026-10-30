---
status: accepted
date: 2026-03-23
decision-makers: ["Na'aman Hirschfeld"]
---

# HTTP first, with a feature-gated browser fallback

## Context and Problem Statement

Crawlberg fetched every page over HTTP with reqwest. That works for server-rendered pages but not for
single-page applications that return an empty shell, for pages whose content is injected by JavaScript, or for
sites behind a WAF that blocks non-browser clients. The deleted `docs/adr/001-engine-architecture.md` had rejected
headless Chrome as the fetcher because it costs 50 to 200 MB per tab and is not embeddable as a library, and
listed "no JavaScript execution" as the cost of that choice. The deleted `docs/adr/008-browser-fallback.md`
(added in 5f8ddca31b) required a browser fallback that does not penalize the common case where HTTP works.

## Considered Options

* HTTP only, with reqwest
* Headless Chrome for every page
* HTTP first, with a headless browser used only when a page needs it, behind a Cargo feature

## Decision Outcome

Chosen option: "HTTP first, with a headless browser used only when a page needs it, behind a Cargo feature",
because pages that HTTP handles pay nothing for the browser, while JavaScript-rendered and WAF-protected pages
still get rendered content without caller changes (`docs/adr/008-browser-fallback.md`).

* `BrowserMode::Auto`, the default, re-fetches with the browser when the HTTP fetch is WAF-blocked or when the
  `js_render_hint` heuristics flag an empty JavaScript shell. `Always` and `Never` force either path.
* The browser code is behind the `browser` feature (chromiumoxide over CDP). The config types and the detection
  heuristics are always compiled, so `CrawlConfig` and `js_render_hint` are the same in every feature set.
* The browser path returns the same response shape as HTTP, so the extraction pipeline runs unchanged.

The in-process `browser-native` backend (2bd84b987e, 2026-05-18) is a second implementation of the same
fallback, behind its own feature; it does not change when the browser is used.

### Consequences

* Good, because a build without `browser` carries no browser dependency.
* Good, because `js_render_hint` tells callers a page needs rendering even when the fallback is not compiled in.
* Bad, because in `Auto` mode a JavaScript-rendered page is fetched twice, once over HTTP and once in the browser.
* Bad, because the chromiumoxide backend needs Chrome or Chromium installed.
* Bad, because the heuristics can flag an intentionally minimal page and trigger an unneeded browser fetch.

### Confirmation

* `crates/crawlberg/src/types/config/primitives.rs:26-33` defines `BrowserMode` with `Auto` as the default.
* `crates/crawlberg/src/engine/scrape_page.rs:14-20` documents the WAF and `js_render_hint` fallbacks that
  `CrawlEngine::scrape` applies.
* `crates/crawlberg/Cargo.toml:34` keeps `browser` out of the default features, and `:38` and `:56` define the
  `browser` and `browser-native` features.
* `crates/crawlberg/src/lib.rs:14-16` compiles `browser` only with its feature and `browser_detect` always.
