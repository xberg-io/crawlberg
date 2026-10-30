---
status: accepted
date: 2026-09-29
decision-makers: ["Tobias Perelstein"]
---

# One robots.txt parser crate for the engine and the browser

## Context and Problem Statement

The crawl engine has had its own RFC 9309 robots.txt parser since the first implementation
(`docs/adr/003-crawl-orchestration.md`). The browser fallback crate had a second parser. Issue #540 found that the
two disagreed: the browser's dropped the first group after a UTF-8 byte-order mark, and differed from the engine on
longest-match precedence, inner wildcards, multi-agent groups, agent matching and trailing comments. The browser
crate is an optional dependency of `crawlberg`, so it cannot depend on `crawlberg` for the engine's parser without
a dependency cycle.

## Considered Options

* Keep a separate robots.txt parser in the browser crate
* Move the engine's parser into a dependency-free `crawlberg-robots` crate that the engine and the browser both use

## Decision Outcome

Chosen option: "Move the engine's parser into a dependency-free `crawlberg-robots` crate that the engine and the
browser both use", because one parser removes the disagreements, and a separate crate lets the optional browser
crate use it without a dependency cycle (f1c5667284, 2026-09-29, #548).

`crawlberg::robots` re-exports the crate with the same API, and the browser crate's parser was deleted. The
parser now combines every group that names the crawler, and every `*` group, as RFC 9309 section 2.2.1 says.
`crawlberg-robots` is published to crates.io first, before the crates that depend on it.

### Consequences

* Good, because the HTTP and browser paths reach the same allow or disallow verdict for a URL.
* Good, because the parser is usable on its own, without the crawl engine.
* Bad, because the workspace publishes one more crate, and every Docker build and the publish order must include it.

### Confirmation

* `crates/crawlberg-robots/src/lib.rs:1-3` documents the crate as the workspace's one robots.txt parser, and
  `crates/crawlberg-robots/Cargo.toml:16` gives it no dependencies.
* `crates/crawlberg/src/robots.rs:20` re-exports it, and `crates/crawlberg-browser/src/net/robots.rs:4` uses it.
* `.github/workflows/publish.yaml:53` lists `crawlberg-robots` first in `CRATES_TO_PUBLISH`.
