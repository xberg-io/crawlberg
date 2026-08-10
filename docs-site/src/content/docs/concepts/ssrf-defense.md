---
title: "SSRF Defense"
---

Crawlberg refuses outbound HTTP requests targeting internal infrastructure,
cloud metadata endpoints, and unsupported schemes. The policy is on by
default and applies to every crawl, scrape, sitemap fetch, robots.txt fetch,
asset download, and link-following enqueue.

## What is refused

| Category | Ranges |
|----------|--------|
| Loopback | 127.0.0.0/8, ::1 |
| Private (RFC1918) | 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 |
| Link-local | 169.254.0.0/16 (incl. AWS/GCP metadata 169.254.169.254), fe80::/10 |
| Unspecified | 0.0.0.0/8 |
| Multicast | 224.0.0.0/4, ff00::/8 |
| IPv6 unique-local | fc00::/7 |
| Non-http/https schemes | file, ftp, gopher, … |

DNS rebinding is mitigated: if a hostname resolves to a mix of public and
denied IPs, the request is refused.

Each 30x `Location` is re-resolved and re-validated against the same policy
before the next hop is taken. Up to `SsrfPolicy::max_redirects` (default 5)
hops are followed.

## Opting out

Two equivalent paths:

**Environment variable** — applies to every crawler in the process:

```bash
export CRAWLBERG_ALLOW_PRIVATE_NETWORK=1
```

**Per-config builder** — applies to a single CrawlConfig:

```rust
use crawlberg::CrawlConfigBuilder;

let config = CrawlConfigBuilder::default()
    .allow_private_networks(true)
    .build();
```

When opt-out is on, the policy permits private IPs but **still refuses
non-http(s) schemes**. The redirect cap and per-hop re-validation also stay
in effect.

### Pinning `deny_private` against the environment variable

`SsrfPolicy.deny_private` defaults to `true` for every binding, so a plain
`true` on that field is ambiguous: it cannot distinguish "the caller
explicitly wants private networks denied" from "the binding's own
structural default happened to land on `true`". Because of that ambiguity,
`CRAWLBERG_ALLOW_PRIVATE_NETWORK` is still consulted and can flip
`deny_private` to `false` even when a config sets it to `true`.

Set `CrawlConfig::ssrf_deny_private_explicit` when that default-deferral is
wrong for a specific call — for example, a test that must prove
`deny_private: true` still denies even while the operator has set
`CRAWLBERG_ALLOW_PRIVATE_NETWORK` suite-wide for every other call:

```rust
use crawlberg::CrawlConfig;

let config = CrawlConfig {
    ssrf_deny_private_explicit: Some(true),
    ..Default::default()
};
```

`None` (the default) preserves today's behavior: the environment variable
may still flip `ssrf.deny_private` to `false`. `Some(value)` pins
`ssrf.deny_private` to `value` and the environment variable is not
consulted for that config.

## Host allowlists

Allowlist specific hosts while keeping the rest of the policy strict:

```rust
use crawlberg::{CrawlConfigBuilder, HostMatcher};

let config = CrawlConfigBuilder::default()
    .ssrf_allowlist_host(HostMatcher::suffix(".internal.xberg.io"))
    .ssrf_allowlist_host(HostMatcher::cidr("10.42.0.0/16")?)
    .build();
```

`HostMatcher::cidr` returns `Result` — a malformed block is rejected when you build it,
rather than silently never matching.

| Matcher | Matches |
|---------|---------|
| `HostMatcher::exact("api.example.com")` | the exact hostname, case-insensitive |
| `HostMatcher::suffix(".example.com")` | `api.example.com`, `example.com` — but **not** `notexample.com` |
| `HostMatcher::cidr("10.42.0.0/16")` | resolved IPs inside the CIDR; also permits literal-IP URLs whose IP is inside |

In JSON or TOML config, a matcher is a tagged object:

```json
{"ssrf": {"allowlist": [
  {"type": "suffix", "value": ".internal.xberg.io"},
  {"type": "cidr", "value": "10.42.0.0/16"}
]}}
```

A bare string is still accepted and is treated as `exact`.

Allowlist entries permit access regardless of the default denylist. A
mismatch between hostname allowlist and resolved IPs (e.g. `Exact("svc.internal")`
resolves to a public IP) still permits the request — the allowlist trusts the host string.

## What happens when a request is refused

Errors are typed:

```rust
pub enum CrawlError {
    SsrfPolicyViolation { url: String, reason: String },
    /* … */
}
```

`url` is the refused URL (original input or the redirect target that failed).
`reason` is one of `"loopback"`, `"private_network"`, `"link_local"`,
`"unique_local"`, `"multicast"`, `"unspecified"`, or `"disallowed scheme: <scheme>"`.

The default retry policy classifies `SsrfPolicyViolation` as permanent —
the crawler will not retry the request.

For link-following inside the crawl loop, refused targets are dropped from
the queue and a `tracing::warn!` is emitted with structured fields
(`url`, `reason`) so operators can see what was blocked.

## Browser layer parity

The headless browser layer (`crawlberg-browser`) shares the same policy core
and applies it to every JS-initiated `fetch()` and every navigation. Two
browser-specific extras are kept:

- `file://` is permitted in the browser process so test pages can use local
  fixtures.
- A `localhost`/`.localhost` string short-circuit runs before DNS to mitigate
  rebinding through the browser's resolver.

This is the same mitigation chain that fixed GHSA-8v6v-g4rh-jmcm.

## Configuration reference

See the `SsrfPolicy` rustdoc for the full type signature.
