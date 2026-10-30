---
title: "Decision records"
description: "Architecturally significant decisions, recorded as MADR ADRs under adrs/ in the crawlberg repository."
---

Architecturally significant decisions are recorded as [MADR](https://adr.github.io/madr/) Architecture
Decision Records under [`adrs/`](https://github.com/xberg-io/crawlberg/tree/main/adrs) in the repository.
This page is the index of that log. Decisions that apply to every xberg-io repository are recorded once, in the
[org log](https://github.com/xberg-io/agent-conventions/tree/main/adrs).

* One decision per file, named `NNNN-kebab-title.md` and numbered from `0000`.
* Front matter carries `status`, the `date` the decision was made and its `decision-makers`. A changed decision
  is a new ADR that supersedes or amends the old one; an accepted ADR is never edited.
* `adrlint lint` and `adrlint format --check` check the log, configured by `.adrlint.toml`. Add the row to this
  page in the same change as the ADR.

| ADR | Title | Status | Date |
| --- | --- | --- | --- |
| [0000](https://github.com/xberg-io/crawlberg/blob/main/adrs/0000-record-architecture-decisions.md) | Record architecture decisions | Accepted | 2026-10-30 |
| [0001](https://github.com/xberg-io/crawlberg/blob/main/adrs/0001-fixture-driven-e2e-suites-against-a-mock-http-server.md) | Fixture-driven e2e suites against a mock HTTP server | Accepted | 2026-03-09 |
| [0002](https://github.com/xberg-io/crawlberg/blob/main/adrs/0002-http-first-with-a-feature-gated-browser-fallback.md) | HTTP first, with a feature-gated browser fallback | Accepted | 2026-03-23 |
| [0003](https://github.com/xberg-io/crawlberg/blob/main/adrs/0003-crawl-infrastructure-as-traits-with-working-defaults.md) | Crawl infrastructure as traits with working defaults | Accepted | 2026-04-01 |
| [0004](https://github.com/xberg-io/crawlberg/blob/main/adrs/0004-markdown-conversion-is-always-on.md) | Markdown conversion is always on | Accepted | 2026-04-01 |
| [0005](https://github.com/xberg-io/crawlberg/blob/main/adrs/0005-ssrf-policy-on-by-default-in-the-http-layer.md) | SSRF policy on by default in the HTTP layer | Accepted | 2026-06-14 |
| [0006](https://github.com/xberg-io/crawlberg/blob/main/adrs/0006-one-robots-txt-parser-crate-for-the-engine-and-the-browser.md) | One robots.txt parser crate for the engine and the browser | Accepted | 2026-09-29 |
