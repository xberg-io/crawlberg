---
status: accepted
date: 2026-06-14
decision-makers: ["Na'aman Hirschfeld"]
---

# SSRF policy on by default in the HTTP layer

## Context and Problem Statement

Crawlberg fetches URLs that callers, and the pages it crawls, supply: seeds, followed links, redirects,
sitemaps, robots.txt and assets. A service that embeds it, such as the REST API or the MCP server, can be driven
to request internal hosts and cloud metadata endpoints. Before 277ef16f6c the REST and MCP handlers checked only
that the URL a request named was http(s), the browser crate had its own check (the GHSA-8v6v-g4rh-jmcm
mitigation), and the core HTTP path checked nothing.

## Considered Options

* Deny private, loopback, link-local, metadata and non-http(s) destinations by default in the core HTTP layer,
  with an explicit opt-out

No alternatives were recorded.

## Decision Outcome

Chosen option: "Deny private, loopback, link-local, metadata and non-http(s) destinations by default in the core
HTTP layer, with an explicit opt-out", because every fetch path then goes through one check, and an embedding
service is protected without configuring anything (277ef16f6c, 2026-06-14).

* `validate_url` runs before each fetch in `http_fetch` and in the Tower service path, so scrape, crawl, batch
  crawl, sitemap, robots.txt, asset and link-enqueue requests are all checked.
* Each redirect target is re-resolved and re-validated before the next hop, up to `SsrfPolicy::max_redirects`
  (default 5), which mitigates DNS rebinding.
* The opt-outs are `CRAWLBERG_ALLOW_PRIVATE_NETWORK=1` (named `KREUZCRAWL_ALLOW_PRIVATE_NETWORK` before the
  rename) and `CrawlConfigBuilder::allow_private_networks(true)` (`crates/crawlberg/src/types/builder.rs:338`),
  plus a host allowlist.
* The browser layer checks against the same policy core.

### Consequences

* Good, because a caller that does nothing gets the safe behaviour.
* Bad, because tests and deployments that crawl local or private hosts must opt out explicitly.
* Bad, because on `wasm32` there is no DNS resolution, so only literal IP hosts are checked and `from_env()`
  allows private networks; under Node.js the embedding service must restrict egress itself.

### Confirmation

* `crates/crawlberg/src/net/ssrf/policy.rs:80-90` defaults `deny_private` to `true` and `max_redirects` to 5.
* `crates/crawlberg/src/http.rs:369` validates the initial URL and `:402` each redirect target.
* `crates/crawlberg/tests/ssrf_integration.rs:152` (`crawl_refuses_loopback_by_default`), `:174`
  (`scrape_refuses_metadata_ip`) and `:389` (`redirect_to_private_outside_allowlist_refused`) check the defaults.
* `docs-site/src/content/docs/concepts/ssrf-defense.md` documents the refused ranges, the opt-outs and the
  `wasm32` gap.
