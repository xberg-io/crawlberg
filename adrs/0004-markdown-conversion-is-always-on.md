---
status: accepted
date: 2026-04-01
decision-makers: ["Na'aman Hirschfeld"]
---

# Markdown conversion is always on

## Context and Problem Statement

Crawlberg added HTML-to-Markdown conversion with html-to-markdown-rs behind a `markdown` Cargo feature, with
results carrying an optional `markdown` field (8144cda5a5, 2026-04-01). Markdown is what AI and LLM pipelines
consume, which the deleted `docs/adr/009-component-boundary.md` gives as the reason the conversion belongs in the
open-source engine. With the feature off, a caller got pages without it.

## Considered Options

* Markdown conversion behind an opt-in `markdown` feature
* Markdown conversion always on, with html-to-markdown-rs as a required dependency

## Decision Outcome

Chosen option: "Markdown conversion always on, with html-to-markdown-rs as a required dependency", because every
result then includes Markdown (87eea9ee99, 2026-04-01), the output AI and LLM pipelines need
(`docs/adr/009-component-boundary.md`), and no build of the crate can lack it.

Conversion runs on every page that is read, in both `scrape()` and the crawl engine. A page skipped as binary or a
PDF is not converted, so the result field stays `Option<MarkdownResult>`.

### Consequences

* Good, because every binding and every feature set returns Markdown without configuration.
* Bad, because every build compiles html-to-markdown-rs and every page pays for the conversion, also for callers
  that only want links or metadata.

### Confirmation

* `crates/crawlberg/Cargo.toml:106` declares `html-to-markdown-rs` as a non-optional dependency, and no
  `markdown` feature exists.
* `crates/crawlberg/src/markdown.rs:1` documents the conversion as always active.
* `crates/crawlberg/src/scrape.rs:118-121` converts every page that was not skipped, and the test at
  `crates/crawlberg/src/scrape.rs:982` checks that a skipped PDF is not converted.
