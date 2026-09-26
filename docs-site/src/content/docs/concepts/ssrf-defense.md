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
| IPv6 forms that embed an IPv4 address | IPv4-mapped (::ffff:0:0/96), IPv4-compatible (::/96), IPv4-translated (::ffff:0:0:0/96), NAT64 (64:ff9b::/96), 6to4 (2002::/16) and ISATAP (interface identifier 0000:5efe or 0200:5efe): the embedded IPv4 address is checked against the rows above |
| IPv6 local-use NAT64 (64:ff9b:1::/48, RFC 8215) | The IPv4 address is read at each position a /48, /56, /64 or /96 network prefix puts it, and the address is refused when any reading falls in the rows above. A reading in 0.0.0.0/8 or 224.0.0.0/4 is skipped, because the unused positions of a real address read that way. An address whose every reading is skipped is refused |
| Non-http/https schemes | file, ftp, gopher, … |

DNS rebinding is mitigated: if a hostname resolves to a mix of public and
denied IPs, the request is refused.

### What the IPv6 rows do not cover

The embedded-IPv4 rows above cover the forms that fix the address at a known position. Three
gaps remain, and an egress restriction outside the process is the only defence against them:

- **A NAT64 network-specific prefix.** RFC 6052 allows a translator to sit on any prefix the
  operator chooses, not only `64:ff9b::/96` or `64:ff9b:1::/48`. Crawlberg does not discover
  the local prefix (RFC 7050 would), so an address under a custom prefix is checked as IPv6
  only. [Issue #110](https://github.com/xberg-io/crawlberg/issues/110) proposes the caller-supplied
  deny ranges that would cover it.
- **A dotted quad in the low 32 bits under an arbitrary prefix.** `2001:db8::a00:5` carries
  `10.0.0.5` in its last 32 bits but is not any of the forms above, so it is not unwrapped.
  Unwrapping every address that way would refuse public addresses whose last 32 bits happen to
  read as private, on every prefix rather than just inside `64:ff9b:1::/48`.
- **Two ranges inside the local-use NAT64 prefix.** The skipped-reading rule above cuts both
  ways: on a /48, /56 or /64 network, an address that genuinely encodes a destination in
  `0.0.0.0/8` or `224.0.0.0/4` is permitted, because that reading is skipped as if it were
  unused bits. `64:ff9b:1:1:2:300::` (`0.1.2.3` after a /48 prefix) and `64:ff9b:1:0:e0:0:100:0`
  (`224.0.0.1` after a /64 one) are permitted. No private, loopback, link-local or CGNAT
  destination escapes this way, and a /96 network is unaffected. In the other direction, the
  same rule refuses some public addresses on those three prefix lengths; an IPv4 allowlist entry
  admits them. [Issue #174](https://github.com/xberg-io/crawlberg/issues/174) tracks both sides.

Teredo (`2001::/32`) is not covered either: its embedded address is XOR-obfuscated with all-ones in
the low 32 bits, so no fixed reading finds it — `2001:0:4136:e378:0:ffff:5601:5601` decodes to
`169.254.169.254`. [Issue #196](https://github.com/xberg-io/crawlberg/issues/196) tracks it, and
denies the prefix outright rather than decoding it.

:::caution[WebAssembly: hostnames are not checked]
On `wasm32` targets — `crawlberg-wasm`, including its `pkg/nodejs` build — there is no DNS
resolution available to the crawler. `validate_url` only checks a **literal IP** host against
the policy; a domain name is always permitted, regardless of `deny_private`. In a browser this
gap is covered by same-origin/CORS. **Under Node.js, `fetch` enforces no CORS**, so a Node
service embedding the wasm binding can be driven to internal hosts by domain name even with
`deny_private = true`. Do not rely on `deny_private` to stop this in Node — enforce egress
restrictions (network policy, firewall, proxy allowlist) outside the process.
:::

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
