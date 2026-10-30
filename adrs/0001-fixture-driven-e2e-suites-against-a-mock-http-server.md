---
status: accepted
date: 2026-03-09
decision-makers: ["Na'aman Hirschfeld"]
---

# Fixture-driven e2e suites against a mock HTTP server

## Context and Problem Statement

Crawlberg's behaviour (scraping, crawling, mapping, metadata, robots.txt, sitemaps, redirects, errors) depends
on what a web server answers. The suite was to be written before the engine, and it had to run without real
network calls and stay the same across the language bindings that were planned. The deleted
`docs/adr/006-e2e-testing-strategy.md` (added in 00ef1b2323) records that the xberg and spikard repositories
already generated their e2e suites from JSON fixtures.

## Considered Options

* JSON fixtures that declare mock HTTP responses, a config and assertions, from which every language's e2e
  suite is generated and run against a local mock server

No alternatives were recorded.

## Decision Outcome

Chosen option: "JSON fixtures that declare mock HTTP responses, a config and assertions, from which every
language's e2e suite is generated and run against a local mock server", because one fixture corpus then drives
every binding's suite, and mock responses keep the tests fast, deterministic and off the network
(`docs/adr/006-e2e-testing-strategy.md`).

The first generator was `tools/e2e-generator/` with wiremock (00ef1b2323, 2026-03-09). alef now generates the
suites and the mock server from the same fixtures, as
[org ADR-0002 (Rust cores with alef-generated bindings)](https://github.com/xberg-io/agent-conventions/blob/main/adrs/0002-rust-cores-with-alef-generated-bindings.md)
records; the fixture-first, mock-served design is unchanged.

### Consequences

* Good, because a behaviour change is one fixture edit, and every binding's suite picks it up on regeneration.
* Good, because the suites need no network access and give the same result on every run.
* Bad, because a mock response cannot show every real-world HTTP edge case.
* Bad, because the generated suites and the mock server must be regenerated whenever the fixture schema changes.

### Confirmation

* `alef.toml:1229-1230` points `[crates.e2e]` at `fixtures/` and writes the suites to `e2e/`.
* Each fixture declares its server, as `fixtures/scrape/scrape_asset_dedup.json:11` does with `mock_responses`.
* `.github/workflows/ci-e2e.yaml:106-110` builds the generated `mock-server`, and the `e2e-tests` job runs each
  language's suite against it.
