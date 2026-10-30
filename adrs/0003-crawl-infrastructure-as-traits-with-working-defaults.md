---
status: accepted
date: 2026-04-01
decision-makers: ["Na'aman Hirschfeld"]
---

# Crawl infrastructure as traits with working defaults

## Context and Problem Statement

Crawlberg is both a standalone open-source crawler and the crawling foundation that commercial platforms build on.
Those platforms need to inject a distributed frontier, persistent storage, their own rate limiting and event
delivery without modifying the engine, while a developer who adds only the crate needs a crawler that works
out of the box. The deleted `docs/adr/009-component-boundary.md` (added in a3cc35ca39) states the principles:
every infrastructure concern is a trait with a real default for single-worker use, not a stub, and no
multi-tenancy, billing or distributed-coordination logic lives in the engine.

## Considered Options

* Infrastructure concerns as traits, each with a working default, composed by a `CrawlEngine` builder
* No trait system: consumers wrap the crawler opaquely
* A plugin system with dynamically loaded libraries
* Configuration-only extension
* Separate open-source and commercial engines

## Decision Outcome

Chosen option: "Infrastructure concerns as traits, each with a working default, composed by a `CrawlEngine`
builder", because the deleted `docs/adr/009-component-boundary.md` rejected the others: wrapping duplicates the
crawl orchestration, Rust's dylib ABI is unstable, a Redis frontier or NATS emitter cannot be expressed as a
config flag, and two engines duplicate the core.

The seven traits in `traits.rs` are `Frontier`, `RateLimiter`, `CrawlStore`, `EventEmitter`, `CrawlStrategy`,
`ContentFilter` and `CrawlCache`. Defaults ship in `crates/crawlberg/src/defaults/` (`InMemoryFrontier`, `PerDomainThrottle`,
`NoopStore`, `NoopEmitter`, `BfsStrategy`, `NoopFilter`, `NoopCache` and alternatives such as `DfsStrategy`,
`BestFirstStrategy`, `AdaptiveStrategy` and `Bm25Filter`). An unset trait takes a default chosen from
`CrawlConfig`: the crawl strategy picks the frontier and strategy, and a BM25 query picks `Bm25Filter`. Request and response
middleware is no longer a trait: it became a Tower service stack in `crates/crawlberg/src/tower/` (5fb9e1765b,
2026-04-01). Concerns added later follow the same pattern: `EventSink`, `PageBudget` and `ProxyProvider` are
set through the builder (`crates/crawlberg/src/engine/builder.rs:198`, `:212` and `:257`), and the
`DispatchProfile` on `CrawlConfig` carries the `RetryPolicy`, `WafClassifier`, `DomainStatePort`,
`EscalationBudget`, `AntibotStrategy` and `BypassProvider` traits (`crates/crawlberg/src/types/dispatch.rs:597-621`).
Extraction, Markdown conversion, robots.txt, URL normalization and sitemap parsing stay concrete.

### Consequences

* Good, because the crate is a complete crawler with no infrastructure supplied.
* Good, because a platform swaps one concern by implementing one trait, and the boundary is enforced by the
  compiler rather than by policy.
* Bad, because the trait signatures are public API, so changing one is a breaking change.
* Bad, because the engine calls each concern through `dyn` dispatch.

### Confirmation

* `crates/crawlberg/src/traits.rs` defines the traits (`Frontier` at line 125, `CrawlCache` at line 290).
* `crates/crawlberg/src/engine/builder.rs:113-184` takes one implementation per trait, and `build()` at line 266
  fills each unset one with its default.
* `crates/crawlberg/src/lib.rs:99-104` exports the default implementations.
