# Changelog

All notable changes to crawlberg are documented here.

## [Unreleased]

### Upgrading

- **A `BypassProvider` must say what the body of its answer is.** `BypassResponse` has a new field,
  `body_kind`: `BypassBody::Bytes` when `body_bytes` holds the bytes the origin sent, and
  `BypassBody::Text` when `body` holds text that is decoded already, such as the HTML a vendor's
  browser rendered. A `BypassResponse { .. }` literal does not compile until it sets the field.
- **`crawlberg-browser`: `NativeBrowserConfig` and `RenderedPage` each have a new public field**,
  `document_decoder` and `charset`, and the crate has a new public type, `DocumentDecoder`. A
  struct literal that names every field must add the new one; `..NativeBrowserConfig::default()`
  needs no change.
- **`CachedPage` has two new public fields**, `charset` and `decoded`. A `CrawlCache` that stores
  the entry as it gets it needs no change. A cache entry that an earlier release stored is not
  served: the page is fetched again and the entry is replaced. A `CrawlCache` that builds its own
  entries must set `decoded` to `true` for a body it stored as decoded text:
  `..CachedPage::default()` sets it to `false`, and such an entry is not served.

### Changed

- **A crawl now treats two addresses that differ only by a trailing slash as two pages.** `/docs`
  and `/docs/` are two resources, and a server can answer them differently, so the crawl requests
  both. Before, it requested the one it met first and never requested the other.
  `CrawlPageResult.normalized_url` keeps the trailing slash of the address, and `map` lists both
  addresses. A site that serves the same content at both addresses now gives two pages where it
  gave one, and each of the two counts against `max_pages`. When one form redirects to the other,
  the crawl reports one page. For each such pair it sends one more request in HTTP mode, and up
  to two more in browser mode and in the sequential loop of the wasm build. To merge two pages,
  compare them in your own code. (#607)
- `CrawlPageResult.normalized_url` writes percent-encoding in one form, as RFC 3986 section 6.2.2
  states it: an escape of a letter, a digit, `-`, `.`, `_` or `~` is decoded, and the hex digits of
  every other escape are in upper case. `/a%2db` is reported as `/a-b`, and `/caf%c3%a9` as
  `/caf%C3%A9`. (#615)
- `LinkInfo.link_type` is decided against the address of the page, not against its `<base>`
  address. `anchor` is a link that has a fragment and names the page it is on, however the link
  is written. `external` is a link to another host than the page's host.
- With `dedup_include_query`, a crawl keeps each query parameter as it is written. It sorts the
  parameters by name and keeps the order of the values of one name. Before, it decoded each
  parameter and encoded it again, so `?a=1&a=2` and `?a=2&a=1`, `?a` and `?a=`, and `?q=a+b` and
  `?q=a%20b` were one page, and the crawl requested only the first. `/p?m=1&n=2` and
  `/p?n=2&m=1` are still one page. `CrawlPageResult.normalized_url` writes the query the same
  way: `?x=A%26y=B` was reported as `?x=A%26y%3DB`.
- `detected_charset` names the detected encoding (for example `windows-1252`) for a page that
  declares none and is not UTF-8, where it was `None`. A declared label that no encoding has is no
  longer reported. A page declared as `us-ascii` is read as windows-1252, as the HTML standard reads it.

### Fixed

- A crawl requested one page once for each percent-encoded spelling of its address. `/a-b`,
  `/a%2db` and `/a%2Db` are now one page, requested once with the spelling of the first link.
  An escape of a reserved character, such as `%2F`, still names its own page. (#615)
- A crawl did not follow a link that is only a fragment, such as `#part`, on a page whose
  `<base>` address is another document. The link names that other document, and the crawl now
  requests it. `map` lists it too. (#616)
- A crawl dropped a linked page that redirects to another address of the same page, and every
  page behind it. A link to `/docs` that answers a redirect to `/docs/` gave one request, no
  page and no error. The crawl now follows the redirect and reads the links of the page it
  lands on, in HTTP mode and in browser mode. The same holds for a redirect to another
  percent-encoded spelling of the address, and for a redirect that adds a query. A redirect to
  a page the crawl already has from another link is still not requested again. (#629)
- The sequential crawl loop of the wasm build reported a page two times when a link and a
  redirect both reached it, for example a folder linked as `/docs` and as `/docs/` where the
  first redirects to the second. It now reports the page one time, and it does not request a
  page again that it first reached through a redirect.
- The Python package sometimes printed `RuntimeError: _crawlberg::CrawlEngineHandle is
  unsendable, but is being dropped on another thread` on stderr after an async call, and the
  engine was then never freed: its connections and its browser stayed until the process ended.
  The engine handle is now a class that any thread can release. (#641)
- Read a page with the character set a browser uses for it. In HTTP mode a page that declares no
  character set and is not UTF-8 came back with replacement characters; its encoding is now detected
  from its bytes. The sources are read in the order of the HTML standard: a byte-order mark, the
  `Content-Type` header, a `<meta>` tag or an XML declaration, then detection. A `charset=` in a
  comment or in the text of the page no longer counts as a declaration, an unknown label no longer
  stops the decision, and one bad byte sequence no longer discards the decode of the whole page.
  Undeclared UTF-8 stays UTF-8, also when a size limit cut the body inside a character or the page
  holds a few bytes that are not UTF-8. An undeclared Shift_JIS, EUC-JP, EUC-KR, GBK or Big5 page
  that a size limit cut inside its last character keeps its encoding. JSON is read as UTF-8. Only
  HTML is searched for a `<meta>` tag, and only its first 1 MiB.
- Keep the text a browser decoded. In browser mode a page whose `<meta>` tag names a character set
  other than UTF-8 came back with two wrong letters for each non-ASCII letter (`cafÃ©`), because the
  decoded text was decoded again by that tag. `detected_charset` now reports the character set the
  browser used.
- Keep the text a bypass vendor decoded. A provider that reads the page from a JSON field (Zyte's
  `browserHtml`) returns text; it was decoded again by the `<meta>` tag of the page.
- Read a page with its character set on the native browser backend. It read every document as
  UTF-8, so a page in another character set lost its letters. It now makes the same decision as HTTP
  mode.
- Replay the text of a cached page. A cache hit for a page that is not UTF-8 came back with broken
  letters. The entry now holds the decoded text and its character set.
- Browser mode lost a page that opens a JavaScript dialog. Chrome stops the page until the
  dialog is answered, and nothing answered it, so `scrape`, `crawl` and `interact` failed with
  a browser timeout after the full `browser.timeout`. Browser mode now closes the dialog and
  returns the page. An `alert`, a `confirm` and a `prompt` are dismissed: the page gets `false`
  from the `confirm` and `null` from the `prompt`. A `beforeunload` dialog is accepted, so the
  navigation that opened it goes on. The text of a dialog is not part of the page and is not
  returned. (#602)
- Browser mode lost a page whose text holds one half of a surrogate pair, such as a string that
  a script cut in the middle of an emoji. The reply from Chrome could not be read, and the
  call failed with a browser timeout after the full `browser.timeout`. The page is now
  returned, with U+FFFD in place of the unpaired half, as a browser shows it. The same holds
  for an attribute, for the title and for the result of an `interact` script. (#631)

## [1.10.3] - 2026-10-09

### Security

- Enforce browser egress policy before every page, popup, iframe, worker, and service-worker
  target can execute. Browser controller loss now fails closed, and WebSocket, WebRTC, and UDP
  transports remain confined to the configured proxy boundary.

### Changed

- Reject external browser endpoints while IP-based SSRF denial is active because Crawlberg cannot
  prove or control the remote browser's network boundary.
- Update compatible dependencies, including ext-php-rs 0.16.1, liter-llm 2.2, and minijinja 3.
- Use the security-hardened `crawlberg-chromiumoxide` 0.9.2 fork for browser control.

## [1.10.2] - 2026-10-06

### Changed

- Update `html-to-markdown-rs` to 3.17.2.

## [1.10.1] - 2026-10-05

### Fixed

- Restore crates.io package verification and WASM builds by compiling SSRF helpers only on the
  targets and feature sets that use them.
- Restore Swift artifact builds for iOS by holding `libc` at 0.2.189 until the released `sysinfo`
  dependency supports the Mach API visibility change in `libc` 0.2.190.

## [1.10.0] - 2026-10-05

### Upgrading

- **Hostname requests through an upstream proxy or external `browser.endpoint` now fail closed
  under IP-based SSRF denial.** With the default `deny_private = true`, add an `Exact` or `Suffix`
  allowlist entry for each hostname whose remote DNS you trust, or set `deny_private = false`.
  A CIDR allowlist cannot verify a DNS answer produced outside Crawlberg. A custom `denylist`
  always wins: remote hostname resolution remains refused even for an allowlisted hostname or
  when private networks are enabled. Literal IP URLs and direct connections retain their existing
  address checks. Custom `crawlberg-browser` validators now inherit the same fail-closed remote-DNS
  default and must override `validate_remote_resolution` to opt out deliberately. (#110)

### Added

- `SsrfPolicy.denylist` adds deployment-specific CIDR ranges to the built-in SSRF denials.
  Configured ranges override allowlists and `deny_private = false`, apply to DNS answers and
  embedded IPv4 addresses, and are available through JSON config and
  `CrawlConfigBuilder::ssrf_denylist_cidr`. Hostname requests are refused when an upstream proxy
  or remote browser would perform the connection's DNS lookup, because that lookup cannot be
  bound to the addresses Crawlberg checked. (#110)

### Fixed

- Generated Python representations and Rust-side Ruby/Elixir binding diagnostics redact proxy
  credentials, authentication values, custom headers, cookie values, document headers, browser
  endpoints, interaction text and scripts, and Chrome arguments while preserving their serialized
  values. (#386)

## [1.9.0] - 2026-10-03

### Upgrading

- **In browser mode, `max_redirects` now counts the navigations a page starts.** A meta refresh
  or a script navigation counts as one redirect, and past the limit the page stays where it is.
  A crawl with a low `max_redirects` that relied on a page's script to move it to the real page,
  such as a challenge page, needs a higher limit. In `crawlberg-browser`, `NativeBrowserConfig`
  gains `max_redirects` and `RenderedPage` gains `redirects`, so a struct literal of either needs
  the new field; `NativeBrowserConfig::default()` sets no limit. (#117, #193, #115)
- **The bypass provider holds its secrets in a type that never prints.** In `crawlberg-bypass`,
  the auth scheme's token, user name and header or query value, and each fixed query value, are
  now a `Secret`. Its `Debug` and `Display` print `***`, so a struct that derives `Debug` over it
  cannot show the key. Build one with `.into()` from a string, and read it with `expose()`. (#386)
- **The config check refuses a proxy password that is not percent-encoded.** A `#`, `/` or `?`
  in a proxy user name or password ends the address early, so `http://user:4242#rest@proxy:8080`
  was read as the host `user` on port 4242. Such an address now fails `CrawlConfig::validate`,
  and the error asks to percent-encode the credential or to set it in `username` and `password`.
  The error never shows the address. An address with no credentials and an `@` in its path,
  query or fragment is refused too, and its error says that a proxy address takes no path,
  query or fragment.
- **The config check refuses proxy credentials set in two places.** A proxy URL that holds a user
  name or password, together with `username` or `password`, now fails `CrawlConfig::validate`.
  Set the credentials in the URL or in the two fields, not both.
- **A proxy with only `username` set now sends its credentials.** The HTTP client sends
  `Proxy-Authorization` with the user name and an empty password. Before, it sent credentials
  only when both `username` and `password` were set.
- **A proxy that a `ProxyProvider` returns gets the same check as the configured proxy.** A
  provider proxy that the check refuses is not used: the request goes direct, and an ERROR line
  names the target host and the reason, never the proxy URL. Its credentials reach the proxy as
  `Proxy-Authorization`. The provider is asked once for each request, redirect hops included,
  so a rotating provider no longer sends one proxy's credentials to another, and a refused proxy
  logs one ERROR line for each request. Each provider proxy has its own connection pool.
- **A native browser render goes through the proxy that a `ProxyProvider` picks.** Before, a
  render ignored the provider and connected directly. Now the render asks the provider once for
  the page's host, and the page load and every request the page makes go through that proxy with
  its credentials. If the provider returns no proxy, the render goes direct, as an HTTP fetch
  does. `browser.proxy`, if set, still wins over the provider for renders. (#248)
- **A Chrome render with a `ProxyProvider` fails instead of going direct.** The Chrome backend
  cannot render through a provider, so a Chrome render with a provider and no `browser.proxy`
  now fails with an error that names the fix: use the native backend, or set `browser.proxy`.
  HTTP fetches with the provider are not affected. (#248)
- **`crawlberg-browser`: the native backend takes a proxy with its credentials apart.**
  `NativeBrowserConfig` gains `proxy`, an `UpstreamProxy`: an address that holds no user name or
  password, and optional `ProxyCredentials`. `UpstreamProxy::new` refuses an address that holds
  credentials, and an address with an `@` after the host, which an unencoded `#`, `/` or `?` in a
  password leaves behind. Its error never shows the address. The same type replaces the proxy URL
  string in the browser context, the HTTP clients, the module loader and the JS runtime
  constructors.
- **`crawlberg-browser`: `NativeBrowserConfig.proxy_url` is deprecated.** Set `proxy` instead.
  `proxy_url` still works, and a user name and password in the URL become the proxy
  credentials. It gets the same checks as `proxy`, so an unencoded `#`, `/` or `?` in a password,
  which leaves an `@` after the host, fails the render. A path, a query or a fragment with no `@`
  is accepted, as in v1.8.0. An unusable
  `proxy_url` fails the render even when `proxy` is set. When both are set, they must name the
  same proxy: a different address or other credentials fail the render with an error that names
  both fields.

- **The config check refuses a SOCKS proxy where no client can use it.** A `socks5://` or
  `socks5h://` address in `proxy` now fails `CrawlConfig::validate` with "SOCKS proxies are not
  supported". Crawlberg's HTTP clients are built without SOCKS support, so every HTTP fetch
  through such a proxy failed at connect time. The same holds for `browser.proxy` with the native
  backend. Chrome speaks SOCKS, so with the Chrome backend `browser.proxy` still takes `socks4://`
  and `socks5://`. Chrome has no `socks5h` scheme. Use an `http` or `https` proxy everywhere else.
- **With the Chrome backend, the config check refuses a crawl-wide proxy with credentials.** A
  Chrome render uses `proxy` when `browser.proxy` is not set, and Chrome cannot use a proxy with a
  username or password. Such a config now fails `CrawlConfig::validate` instead of every browser
  render. The HTTP client still takes the proxy. To keep it, set `browser.proxy` to a proxy that
  needs no credentials, use the native backend, or set `browser.mode` to `never`. A build without
  the Chrome backend is not affected.
- **The config check also checks `browser.proxy`.** A `browser.proxy` with a scheme the browser
  cannot use, such as `gopher://`, now fails the config check instead of the render. (#249)

- **In browser mode, a page with an error status is now the error HTTP mode returns.** A scrape
  of such a page returned the rendered HTML with status 200. It now returns the same error that
  HTTP mode returns for the same status. The statuses are 401, 403, 404, 408, 410, 429, 500, 502,
  503 and 504. A 403, 429 or 503 page is a WAF error when its headers or its body name a WAF,
  as in HTTP mode, so it escalates instead of being retried. A page with another status, such as
  501, 505 or 599, stays a page, as in HTTP mode. Code that expects a page from every browser-mode
  scrape must handle these errors. A crawl in browser mode now keeps the same pages as one in HTTP
  mode. Under `soft_http_errors` a 404 or 403 page, and a 404 at the end of a redirect, is a page
  that keeps its status and has an empty body, as in HTTP mode. The Chromiumoxide backend reports the status and the
  response headers of the document the page shows, so a WAF block is found from the headers of a
  403, 429 or 503 page as well as from its body. When Chrome shows its own error page in place of
  such a response, such as for a download or an empty body, only the headers are checked, because
  Chrome never rendered the body. (#143)

- **The native browser backend reports an empty body for a 204, 205 or 304.** It reported an
  empty HTML skeleton for these statuses. It now reports an empty body, as HTTP mode does. If
  your code reads the body of such a page, expect an empty string. (#121)
- **A page that navigated to a 204, 205 or 304 before its load event could time out in browser
  mode.** Chrome commits no document and fires no load event for those responses, so the
  Chromiumoxide backend could wait until `browser.timeout`. It now keeps and returns the page
  that was already loading. (#436)

- **`interact` now follows at most `max_redirects` redirects.** The default is 10. The
  Chromiumoxide backend followed every redirect a chain offered, and the native backend followed
  up to 20. For a longer chain, `interact` returns the URL of the redirect at the limit. On the
  Chromiumoxide backend it also returns empty HTML and a failed result for each action. If an
  `interact` call must follow a longer chain, raise `max_redirects`. (#116, #115)

- **A Chromiumoxide browser fetch or `interact` session fails when Chrome reports no main
  frame.** The redirect limit counts only the redirects of the page's main frame, so crawlberg
  must know that frame. If Chrome reports no main frame, or the read of it fails, the call
  returns a browser error that says the redirect limit cannot be applied. (#90)

- **`ScrapeResult`, `CrawlPageResult` and `InteractionResult` gained `ssrf_refused_urls`.** The
  field is left out when it is empty, so an older crawlberg still reads a result with no refused
  request. A scrape or page result that lists one is rejected by an older reader, because both
  types refuse unknown fields.

- **`BrowserConfig` gained two fields and rejects unknown ones.** `chrome_path` and `chrome_args`
  are always serialised, and `BrowserConfig` rejects unknown fields, so **a browser configuration
  serialised by this version is rejected by every older crawlberg**, even when both are unset.
  The break is one-directional: an older configuration still loads here, because both fields
  have defaults. (#79, #80)

- **`BrowserPoolConfig.chrome_args` now refuses entries that the pool used to launch with.** The
  pool applies the rules of `BrowserConfig.chrome_args`, so these entries now fail: an entry
  without a leading `--` (`disable-gpu`), a flag name with an uppercase letter, a flag named twice
  (`--enable-features` given two times), and `--headless`, `--remote-debugging-port` or
  `--user-data-dir` in any form, `--headless=new` included. `BrowserPool::new` still accepts the
  config: the refusal comes when the pool launches Chrome, as an error from `warm` and
  `acquire_page` that names `BrowserPoolConfig.chrome_args`. Write each flag once, as `--flag` or
  `--flag=value` with a lowercase name, and join several `--enable-features` values with commas.
  (#79, #80)

- **`BrowserProfile::chrome_args()` is deprecated.** It returns a single `--user-data-dir=<path>`
  flag, and `chrome_args` now refuses `--user-data-dir` in any form, so the flag only helps a
  caller that starts Chrome itself. Nothing in crawlberg calls it: a profile reaches Chrome
  through `CrawlConfig.browser_profile` and `save_browser_profile`, which set the launch's
  `--user-data-dir` directly. Read `BrowserProfile.user_data_dir` instead, or set
  `CrawlConfig.browser_profile` and let crawlberg apply it. (#254)

- **The regenerated bindings add required `BrowserConfig` constructor arguments.** Code that
  constructs a `BrowserConfig` by hand must pass the new settings: `chrome_path` and `chrome_args`
  to Swift's `init` and the Java record constructor, and `chromeArgs` to Dart's constructor. The
  Java builder and the other bindings give both settings defaults. (#79, #80)

- **`crawlberg_browser::adapter::DEFAULT_DENY_NET_CIDRS` is now a slice and grows from 13 to 14
  entries**, adding `240.0.0.0/4`. Its exported type is `&[&str]`, so future length changes no
  longer alter the type. Code that expected the fixed array must accept a slice instead. A direct
  `for cidr in DEFAULT_DENY_NET_CIDRS` loop now yields `&&str` rather than `&str`; use
  `DEFAULT_DENY_NET_CIDRS.iter().copied()` when the loop body needs `&str` values.

- **An IPv6 allowlist entry no longer admits an address that carries a denied IPv4 address.**
  The IPv4-compatible (`::/96`), IPv4-translated, 6to4 (`2002::/16`), Teredo (`2001:0::/32`),
  ISATAP and local-use NAT64 (`64:ff9b:1::/48`) forms are now checked as the IPv4 address they
  carry, so an allowlist entry for such an address has to name that IPv4 range instead of the
  IPv6 one. IPv4-mapped and `64:ff9b::/96` addresses already behaved this way.

- **The browser crate's fallback validator names the denial reason.** `DefaultSsrfValidator`
  messages now end with the reason the core policy reports (`loopback`, `private_network`,
  `link_local`, `unspecified`, `multicast`, `unique_local`). Code that compares the whole message
  must allow for the new suffix.

- **A URL's `user:pass@` no longer appears in any URL crawlberg returns.** crawlberg takes the
  userinfo off a URL when a call starts, and sends it only as an `Authorization: Basic` header to
  that URL's host. Every URL in a result, a stream event or a plugin callback is the URL without
  the userinfo: `final_url`, page and link URLs, map entries, and the URL that pairs each
  `batch_scrape` and `batch_crawl` result. If you match batch results against your own input URLs,
  remove the userinfo from your input first. A URL that carries userinfo is now a configuration
  error when `auth` is also set; use one of the two.

- **`auth` and `custom_headers` now go only to the seed URL's host.** A subdomain, a linked
  document on another host or a redirect target on another host gets neither. If a crawl needs a
  header on another host, start a separate call with that host as its seed.

- **`metadata.canonical_url` is now an absolute URL.** It was the canonical link's `href` as
  the page wrote it, so `<link rel="canonical" href="/en/page">` gave `/en/page`. It is now
  resolved against the page's base URL and normalized as the links list is, so it gives
  `https://example.com/en/page`, and `https://Example.com` gives `https://example.com/`. If your
  code joins a relative canonical URL to the page URL, remove that step. If it compares the value
  with a literal, compare with the normalized form. (#101)

- **`metadata.hreflangs[].url` is now an absolute URL.** It was each alternate-language link's
  `href` as the page wrote it, so `<link rel="alternate" hreflang="de" href="/de/">` gave `/de/`.
  It is now resolved against the page's base URL and normalized as the canonical URL is, so it
  gives `https://example.com/de/`. If your code joins a relative hreflang address to the page URL,
  remove that step. (#126)

- **A 503 or 429 behind Akamai, Imperva or F5 is retried again instead of escalating.** The three
  fingerprints in `rules/waf_fingerprints.toml` whose only signal is the CDN's own `server` header
  (`AkamaiGHost`, `Incapsula`, `BIG-IP`) now decide a 403 only. An overloaded or redeploying origin
  behind one of those CDNs is therefore retried per `retry_codes` as it was before challenge
  statuses were fingerprinted, instead of being classified as a WAF block and escalated to the
  bypass or browser tier. A real block from those vendors is still caught on a 403, and no other
  fingerprint changes. A custom corpus can scope any fingerprint the same way with an optional
  `statuses` array of the codes it may decide; an empty array is rejected. (#197)

- **`CrawlError::WafBlocked` has a `source` field.** Rust code that builds the variant by hand
  must pass `source: None`, or call `CrawlError::waf_blocked(vendor, message)` instead.
  `CrawlError::waf_blocked_with_source` attaches an underlying error. A match on the variant with
  `..` does not change. The Swift and Kotlin Android bindings give the WAF block case a `source`
  value, as their other error cases already have: Swift code that matches
  `.wafBlocked(vendor:message:)` must bind the third value, and Kotlin code that builds
  `CrawlError.WafBlocked` must pass `source`. The other bindings do not change. (#133)

- **`CrawlConfig` gained `path_patterns_match_url`, which older versions reject.** The field is
  always serialised, and `CrawlConfig` already carries `#[serde(deny_unknown_fields)]`, so **a config
  serialised by this version is rejected by every older crawlberg**, even when the value is
  `false`. The break is one-directional: an older config still loads here, because the field
  defaults to `false`.

  What this affects:

  - A config serialised on one crawlberg and read by another. Upgrade the readers before, or
    with, the writers.
  - Any binding that round-trips a config through JSON across the FFI boundary
    (`cberg_crawl_config_to_json`, `cberg_crawl_config_from_json`), where the core and the binding
    can be at different versions.

- **`metadata.og_url`, `og_image`, `og_video`, `og_audio` and `twitter_image` are now absolute
  `http` or `https` URLs, or absent.** Each field was the meta tag's `content` as the page wrote
  it, so `<meta property="og:image" content="/img/hero.png">` gave `/img/hero.png`, and a
  `javascript:` or `file:` address came back unchanged. Each field now resolves against the page's
  base URL, as the canonical URL does, and is absent when the result is not an `http` or `https`
  address. If your code joins a relative address to the page URL, remove that step. (#312)

- **A struct literal of a type that gained a field needs the new field.** This holds for
  `ScrapeResult`, `CrawlPageResult` and `InteractionResult` (`ssrf_refused_urls`), `BrowserConfig`
  (`chrome_path`, `chrome_args`), `CrawlConfig` (`path_patterns_match_url`) and, in
  `crawlberg-browser`, `NativeBrowserConfig` (`proxy`). Each of these types implements `Default`,
  so ending the literal with `..Default::default()` is enough.
- **Three more public structs gained a field, so a struct literal of each needs it.** In
  `crawlberg-browser`, `NativeBrowserConfig` gains `origin_headers`: set it to `None`, or end the
  literal with `..Default::default()`. In `crawlberg`, `BrowserPoolConfig` gains `chrome_path`:
  set it to `None` to find Chrome as before, or end the literal with `..Default::default()`. In
  `crawlberg-browser`, `NativeCookie` gains `host_only` and has no default: set it to `true` for a
  cookie that goes only to its `domain`, and to `false` for one that also goes to subdomains.
- **`crawlberg-browser`: `PageError` gained `InvalidConfig`.** A render returns it when its
  configuration cannot be used, such as an unusable proxy, and then fetches nothing. A `match` on
  `PageError` with no wildcard arm needs an arm for it.

### Added

- **Choose the Chrome binary and add Chrome flags.** `BrowserConfig.chrome_path` names the one
  Chrome or Chromium executable a browser-mode fetch launches; a missing or non-executable path is
  an error that names it, never a fallback to another Chrome. `BrowserConfig.chrome_args` adds
  Chrome flags, each written as `--flag` or `--flag=value` with a lowercase flag name, and a flag
  that names one of crawlberg's defaults replaces that default. The Rust `BrowserPoolConfig`
  applies the same checks to its own `chrome_args` when it launches Chrome. Both settings reach
  every Chrome that crawlberg launches, and both are ignored with a warning, and not checked,
  when `browser.endpoint` is set or the native backend is in use. Flags such as
  `--proxy-server` and `--host-resolver-rules` route around the SSRF policy, so set
  `chrome_args` only from trusted configuration. The `BrowserConfig` debug output and the
  warning give the number of flags, not their values, because a flag value can carry a
  credential. (#79, #80)

- `CrawlConfig.path_patterns_match_url` matches `include_paths`/`exclude_paths` against the full
  URL, `scheme://host[:port]/path?query`, so a pattern can scope by host. The matched text leaves
  out any userinfo and the fragment, and the host is in punycode. It defaults to `false` and takes
  precedence over `path_patterns_match_query`. (#78)

### Fixed

- **A Chrome that crawlberg kills could leave a child process behind on macOS.** crawlberg stops
  every process of a Chrome it launched before it kills them, so that none of them can start a
  child that the kill does not see. On macOS, a stop sent to a process that is starting a program
  does not always hold: the process can run again and start a child after crawlberg last read the
  process table. crawlberg now reads every process again after it reads the table. If a process
  runs, crawlberg stops it again and reads the table once more. A process that does not take its
  stop in two waits of one second no longer holds the kill: crawlberg logs a warning and kills
  the processes it found. It also logs a warning when the processes keep changing for eight
  reads of the table. A process that runs after the last read is still not seen. This applies to
  the Chromiumoxide backend. (#585)

- **The native browser serializes HTML raw-text elements by the HTML rules.** Text in `xmp`,
  `iframe`, `noembed`, `noframes`, `plaintext` and `noscript` is no longer escaped, while text in
  `title` and `textarea` is escaped so markup-like text cannot become active markup after a
  reparse. (#288)

- **The native browser fetched a blank stylesheet or script address as the page itself.** An
  empty address, or one made only of URL-parser whitespace, is now skipped before resolution, so
  it produces no duplicate request or network event. (#270)

- **Native-browser subresources ignored the document's base address.** A relative stylesheet,
  classic script or module script under `<base href="/assets/">` was fetched relative to
  the page address instead. These subresources now resolve against the first `<base href>`,
  falling back to the page address when that base does not parse. (#265)

- **Inline data no longer bloats markdown output.** An image whose source is a `data:` URL becomes
  its alt text, and an inline SVG becomes its title. Links keep their visible text without the
  destination, video and audio keep their fallback content, and an iframe with only an inline
  address disappears. Elements with ordinary addresses are unchanged. (#97, #120)

- **A redirect header with a non-ASCII byte was dropped.** Header collection accepted visible
  ASCII only, so a `Location` or `Refresh` value containing an `obs-text` byte disappeared and
  its redirect was missed. Response field values now use the browser's byte-preserving
  isomorphic decode in both HTTP fetch paths, and direct `Location` handling uses the same
  decode. (#278)

- **Relative links in the markdown resolved against a `<base>` that was not one.** The markdown
  found the page's base with a second parser. That parser read markup inside `<title>`,
  `<script>` and `<style>` as tags and counted a `<base>` inside `<svg>` or `<template>`, so
  `<title><base href="/x/"></title>` moved every relative link under `/x/`. It now uses the first
  `<base href>` in tree order, the same base as the links list, and leaves markup inside raw text
  as written.

- **An `interact` session waited forever when its Chrome died.** With the Chromiumoxide backend,
  the CDP client stops reading a broken connection to Chrome, but it keeps every command that waits
  on that connection. So the session waited without end for the reply to the close of its page. The
  session now ends its CDP client when the connection breaks, every command that waits fails at
  once, and `interact` returns an error. (#577)

- **A browser pool kept a crashed Chrome, and a scrape waited on a broken browser connection.**
  The pool and the one-shot scrape had the same CDP client defect as `interact` (#577): after the
  connection to Chrome broke, every command that waited on it waited without end. The pool
  launches a new Chrome only when that client has ended, so after a crash every page request
  failed and `shutdown` waited forever. Now both end the CDP client at the first connection
  error, and log that error as a warning. A page request that finds the old Chrome gone gets a
  new Chrome, and a scrape ends with an error that says the browser's connection closed. (#581)

- **A browser command that got no answer waited without end.** With the Chromiumoxide backend,
  the CDP client fails a command after its 30-second request timeout only when something else
  wakes the client, and on a connection with no other traffic nothing did. So a command Chrome
  never answered held a crawl or an `interact` session forever. crawlberg now wakes the client
  each second, and such a command fails with a timeout within twice the request timeout. (#586)

- **A browser read of the committed document ignored `browser.timeout`.** `browser.timeout`
  bounded only the navigation. The reads after it, of the HTML, the committed document and the
  screenshot, run in the page's renderer, so a page that keeps the renderer's main thread busy held
  each read up to the CDP client's fixed 30-second timeout, and a render can make several reads.
  In scrape, crawl and `interact`, the reads of the final document now share one `browser.timeout`
  budget, and a read past it fails as a browser timeout that names the budget. For such a page,
  REST callers now get 504 `TIMEOUT` instead of 500 `BROWSER_ERROR`, and Python callers get
  `BrowserTimeoutError` instead of `BrowserError`. (#567)
- **A stalled module script cost 10 seconds for every module script after it.** The native browser
  backend waited, after each module script, until nothing at all was pending on the page. A module
  whose top-level `await` never settled left work pending forever, so each later module script also
  waited out its full 10-second budget: a page with one stalled module and two after it took 30
  seconds to render. Each module script now waits only for its own evaluation, so the stalled module
  costs the page one budget. Work a module starts without awaiting it, such as a fetch, now finishes
  after the next module script runs. (#486)

- **An awaited script evaluation waited for the whole page to go idle.** The native browser backend
  ran an awaited evaluation or function call until nothing at all was pending on the page. A page
  with a fetch that never answers made every awaited evaluation wait out a 5-second budget, even
  for `Promise.resolve(1)`: two such evaluations took 10 seconds. Each awaited evaluation now waits
  only for its own promise, for at most 5 seconds, and an error from other work on the page no
  longer ends the wait early. An evaluation that did not settle in time used to return the result
  of the evaluation before it; it now comes back as `undefined`. (#541)

- **A classic script could hold its page for 5 seconds after it finished.** The native browser
  backend runs each classic script under a 5-second watchdog. When the script finished before the
  watchdog thread started, which happens on a loaded host, the watchdog missed the signal and slept
  out its full budget while the page waited for it. The watchdog now checks whether the script is
  done before it starts to wait. (#566)

- **Chrome's WebSocket, WebTransport and WebRTC traffic, and a second DNS answer, reached addresses
  the SSRF policy refuses.** The request check sees only HTTP requests, so a WebSocket opened a
  connection to a denied address, WebTransport and WebRTC sent UDP datagrams to one, and Chrome
  could resolve a checked host name again to a different address. With `deny_private` on, Chrome
  now sends every connection through a small proxy inside crawlberg. The proxy resolves each host
  once, checks the addresses against the SSRF policy, and connects only to an address that passed,
  so Chrome never resolves a name itself. This covers scrape, crawl, `interact`, pooled browsers,
  a `browser_profile` session and a `browser.endpoint` Chrome on this machine. A refused
  connection is listed as `host:port` in the result's refused URLs for a one-shot scrape and for
  `interact`; in a browser pool it is logged with its host and port. With an upstream proxy, the
  proxy sends a host name to the upstream unresolved and checks only address literals, as the
  HTTP client does, so the upstream resolves the name. Chrome's requests reach an `http` or
  `https` upstream as they did before. A `browser.endpoint` Chrome on another machine cannot use
  the proxy: its HTTP requests are still checked, and crawlberg logs one warning that its sockets
  are not. With `deny_private` on, a launched Chrome sends WebRTC UDP only through a proxy, which
  stops it; a pooled Chrome always does, because one pool serves crawls with either setting.
  (#165, #178, #452)
- **A sandboxed Chrome ran without the WebRTC policy and sent UDP to denied addresses.** crawlberg
  turns off WebRTC UDP that bypasses the proxy by writing a preference into the profile directory
  it gives Chrome, in the system temp directory. A sandboxed Chrome, such as the Chromium snap on
  Ubuntu, has a private /tmp: it opened an empty directory at that path, made a fresh profile, and
  sent WebRTC datagrams to denied addresses with no error. When the Chrome to launch is a snap,
  crawlberg now makes its scratch profile in `~/snap/<name>/common`, which the snap reads at the
  same path. After each launch that relies on the profile, crawlberg also checks that Chrome wrote
  into that directory. If it did not, crawlberg stops Chrome and returns a browser error that names
  the cause, and the crawl does not run without the policy. (#165)
- **A saved browser profile failed on a snap Chrome with an error that did not name the cause.**
  crawlberg keeps saved profiles under `~/.local/share/crawlberg/profiles` on Linux. A snap, such
  as the Chromium snap on Ubuntu, cannot open a folder in the home directory whose name starts
  with a dot, so Chrome exited at once on a lock file it could not create. crawlberg now refuses a
  saved profile that a snap Chrome cannot open, before it starts Chrome. The error names the cause
  and the fixes: set `XDG_DATA_HOME` to a folder the snap can open, use a Chrome that is not a
  snap, or turn off `save_browser_profile`. (#556)
- **Browser mode returned Chrome's error page as the page.** When the main frame ended on
  Chrome's own error page, the Chromiumoxide backend returned that page's HTML as content. This
  happened for a download with a status such as 501, 505 or 599, for an error status with an
  empty body, and for a navigation that failed at the network, which reported status 200. When
  the server answered, the fetch now reports its status, headers and URL with no body, and handles
  the status as HTTP mode does: a 404 or 500 is the same error, and a 400 or 501 is a page. When
  the server did not answer, the fetch fails with a browser error that names the URL. After a page
  navigates itself, only the redirects of that navigation make a 404 a page, not the redirects of
  the requested URL. (#317, #319)
- **A successful response whose body Chrome could not decode was reported as an empty page.** A
  malformed compressed body now fails with a browser error that names its 2xx status and URL,
  whether it is the seed or the target of a page navigation. This applies before `interact`
  renderer scripts and actions too. (#417)
- **The HTML, status, final URL and screenshot of a browser render come from one document.**
  They were separate reads, so a navigation that committed between them could pair the HTML of
  one document with the status, URL or screenshot of the next. The render now reads which
  document is committed before and after it reads the HTML and takes the screenshot, and reads
  both again when the document changed. The final URL is the URL of that document. A page that
  navigates during each of three reads fails with a browser error. (#318)
- **A browser screenshot of a page that keeps navigating held the fetch until its deadline.**
  Chrome can leave a screenshot unanswered while the page keeps replacing its document. The
  screenshot now stops after 5 seconds, and the page is reported without one.
- **`interact` returned Chrome's error page as the page.** With the Chromiumoxide backend, a
  Scrape action, and the final HTML of a session, could be Chrome's own error page, for example
  after a download that Chrome cannot show. Such a Scrape action now fails, and a session that
  ends on the error page fails with a browser error that names the URL. When the error page is for
  a navigation the SSRF policy refused, the session keeps its result: the refusal fails the action
  that caused it and is listed in `ssrf_refused_urls`, the final HTML is empty, and the final URL
  is the refused URL. An ExecuteJs or Screenshot action that starts on the error page reports a
  failure. The action still runs once, so a script such as `history.back()` can leave the error
  page. The final HTML and the final URL of a session, and a Scrape action's HTML, are read from
  one committed document. (#345, #346, #355)
- **A secret in an endpoint host label printed in debug output.** A browser endpoint, a bypass
  provider endpoint or a browser session proxy prints as its origin, and a per-account host such
  as `sk-live-abc.api.example.com` printed in full. The host now keeps its last two labels and
  prints `***` for each label to their left: `***.***.example.com`. An IP address prints as
  before. (#175)
- **The Chrome backend's record of a main-frame response printed its `Set-Cookie`.** The SSRF
  interception keeps the headers of each main-frame response, and its debug output printed every
  value. It now prints `***` for each credential header on the shared list, as the other response
  header maps do. (#141, #386)
- **The native browser backend logged the proxy password.** The backend put the proxy user
  name and password back into the proxy URL, and every module import logged that URL at debug
  level. The credentials now stay apart from the proxy address from the config check to the
  connection, where the HTTP clients send them as `Proxy-Authorization`. No proxy URL that a log
  line or an error can show holds a password. (#238, #385)
- **The `Debug` text of a proxy showed part of an unencoded password.** For
  `http://user:4242#rest@proxy:8080`, the `Debug` text of `ProxyConfig` and of
  `StaticProxyProvider` showed `http://user:4242`. It now shows `***` for an address that the
  config check refuses. (#285)
- **The `Debug` text of `BrowserPoolConfig` showed a credential in a Chrome flag.** A flag value
  that holds an `@`, such as `--proxy-server=http://user:pass@proxy:8080`, now prints as
  `--proxy-server=***`. The flag name stays.
- **The native browser backend could ignore its proxy and connect directly.** A proxy URL that
  did not parse, or one whose scheme the HTTP client cannot speak, such as `ftp://`, was dropped
  without an error, and every request of the render then went direct. A caller who relied on the
  proxy for egress control got neither the proxy nor a failure. The backend now checks the proxy
  URL when it builds its clients and fails the render with a configuration error that names the
  reason. The error never contains the URL, so credentials in it cannot leak. The same check
  covers page-initiated fetches and dynamic module imports, whose errors printed the proxy URL,
  credentials included. (#237)
- **A proxy address without a scheme works everywhere the HTTP client takes it.** An address
  such as `127.0.0.1:3128`, `localhost:3128` or `user:pass@proxy:3128` is read as an `http://`
  proxy, exactly as the HTTP client reads it. The config check refused it for `proxy` (#420),
  the native backend refused it in `browser.proxy` once a username or password was set (#421),
  and the stealth mode ignored it and connected directly.
- **Chrome renders ignored the proxy and connected directly.** With the Chrome backend, a render
  launched Chrome without the proxy, so a caller who relied on `browser.proxy` or `proxy` for
  egress control got direct connections with no error. Only the interact path passed it. Every
  Chrome launch now takes the proxy, read the same way as the HTTP client reads it. A shared
  browser pool, or a Chrome reached through `browser.endpoint`, opens each page in a browser
  context made with that crawl's proxy, so crawls with different proxies share one Chrome and
  each goes through its own proxy. Requests to a loopback address go through the proxy too;
  Chrome sends them direct by default. (#434)
- **A proxy flag in `chrome_args` replaced the configured proxy on a launched Chrome.** With
  `browser.proxy` or `proxy` set, a `--proxy-server` in `browser.chrome_args` sent a one-shot
  render or an interact session through the caller's proxy, and a `--proxy-bypass-list` sent
  loopback requests direct. `--no-proxy-server`, `--proxy-pac-url` and `--proxy-auto-detect` did
  the same, because Chrome reads them before `--proxy-server`. A pooled or connected Chrome used
  the configured proxy. Every Chrome now uses the configured proxy. Crawlberg drops each of these
  flags with a warning that names the flag but not its value, and loopback requests still go
  through the proxy.
- **A Chrome proxy with credentials never connected.** Chrome takes the proxy address as a
  launch flag and ignores credentials in it, so a render through `user:pass@proxy:3128` or a
  proxy with `username` and `password` made no connection and failed without saying why. The
  Chrome backend now refuses a proxy with credentials, with an error that says so and does not
  show them. Chrome gets the address as the HTTP client reads it, so `127.0.0.1:3128` and
  `http:proxy:3128` now work. (#435)
- **An `interact` action reported success while a request it sent was refused.** A paused
  request counted toward its page only once the check had matched it to the page. For a frame the
  check does not know yet, that match reads the frame tree of every live page, and it can take
  longer than the 25 ms grace after an action. The action then ended with nothing in flight and
  reported success, and the refusal was charged to no action at all. A result's
  `ssrf_refused_urls` could miss such a request for the same reason. The request itself was always
  refused. A paused request now counts from the moment the check receives the pause. (#192)

- **A 204 or 304 seed timed out in browser mode.** Chrome commits no page for a response without
  a document, so the Chrome backend waited for the browser timeout (20 seconds by default) and
  then failed. A 204, 205 or 304 answer, including one at the end of a redirect, now ends the
  fetch at once with the status, final URL and empty body that HTTP mode reports. The native
  backend already returned at once, but it reported an empty HTML skeleton as the body; it now
  reports an empty body too. (#121)

  A 304 Chrome asked for itself is unaffected: when Chrome revalidates a page it holds in its
  cache, the page still renders from that cache with status 200. Only a 304 that no cache entry
  can satisfy is reported as an empty 304, which is what it carries.

- **`max_redirects` did not limit browser mode.** Chrome follows a redirect chain itself, and the
  chain counted the whole of it as one hop, so a browser-mode crawl followed chains that HTTP mode
  refuses. Chrome now follows at most the redirects the chain has left. The chain stops on the
  redirect response at the limit, with the same redirect count, status and final URL that HTTP
  mode reports, and the next hop is never requested. This applies to both browser backends.
  (#90, #115)

  A navigation the page starts itself also counts: a meta refresh Chrome acts on counts as one
  redirect, as it does in HTTP mode, and so does a script navigation such as `location.replace`.
  Each redirect such a navigation follows counts too. The limit covers every navigation of the
  page until the crawl reads it, and past the limit the page keeps the document it has. Before,
  only the redirects before the first document counted, so a page could lead Chrome through any
  number of refreshes or script navigations. A redirect inside an iframe does not count.
  (#117, #193)

- **A native browser scrape skipped the redirect chain.** It did not follow a meta refresh, so
  it returned the refresh page where HTTP mode returns the page the refresh points at. It also
  reported a seed that answers 404 as a page with status 404 when the seed had no trailing
  slash, where HTTP mode fails with `not_found`. A native scrape now takes the same redirect
  chain as HTTP mode and uses the redirect count the backend reports. (#529, #530)

- **A native browser scrape lost the cookies a meta refresh page set.** Each hop of the redirect
  chain is its own native render, and each render started with an empty cookie jar, so the request
  to the refresh target did not carry the refresh page's cookies and the result did not list them.
  The chain now carries the jar from one hop to the next, with the Secure and HttpOnly flags of
  each cookie.

- **The native browser stored cookies that a page had no right to set.** A page could set a
  cookie for another host with a `Domain` attribute, for example a page on `127.0.0.1` for
  `localhost`, and the native browser then sent that cookie to the other host, after a 302 or a
  meta refresh. It also stored a Secure cookie that a page set over plain http. The native
  browser now ignores a cookie whose `Domain` does not match the host that set it, and a Secure
  cookie set over http.
  It also ignores a cookie whose `Domain` is a public suffix, such as `co.uk`, `github.io` or
  `localhost`, unless the host that set it has that exact name.
  A cookie set without a `Domain` attribute now goes back to the host that set it only, not to
  that host's subdomains. In `crawlberg-browser`, `NativeCookie` gains `host_only`, so a struct
  literal of it needs the new field.

- **The native browser matched a cookie by its name alone.** A cookie with the same name but
  another path, or another `Domain` setting, replaced the first cookie. A deletion for one path
  removed the cookie at every path. A cookie set without a `Path` took the whole request path, a
  `Path` that did not start with `/` was kept as given, and the path `/only` also matched
  `/onlyfoo`. The native browser now identifies a cookie by its name, its path and whether it
  has a `Domain`. A missing or relative `Path` gives the directory of the request path, and a
  path matches only at a `/` boundary. A page over plain http can no longer overwrite or delete
  a Secure cookie, and a `__Secure-` or `__Host-` cookie that breaks its prefix rules is ignored.

- **A native scrape through a meta refresh listed only the last page's refused addresses.**
  `ssrf_refused_urls` now lists the addresses the SSRF policy refused on every page of the chain.

- **`interact` set no redirect limit, and a 204 or 304 seed timed out there.** The pages
  `interact` opens now follow at most `max_redirects` redirects, and a 204, 205 or 304 answer
  returns at once. When the navigation ends on a response without a document, `interact` reports
  the URL that answered, empty HTML, and a failed result for each action that names the status.
  The SSRF check still applies to every request. On the native backend `interact` follows at most
  `max_redirects` redirects too, and stops on the redirect response at the limit. The limit covers
  the navigation to the page, not the navigations the actions start. (#116, #140, #115)
- **A page could navigate to a refused address after it loaded.** The Chromiumoxide backend
  stopped checking requests against the SSRF policy when the page finished loading, so a script
  that navigated during `extra_wait` reached any address. The check now stays on until the HTML
  is read. A main-frame navigation it refuses, during the load or after it, fails the fetch with
  the SSRF policy error, because the page Chrome then shows is its own error page. A refused image
  or iframe keeps the page. (#143)
- **Browser mode reached addresses the SSRF policy refuses.** The request check covered one page
  and stopped when the navigation finished. In `interact`, a click, a form submission, a script
  `fetch()` or a popup the actions started reached private and loopback addresses. In scrape and
  crawl, a popup the page opened, and a request it sent during the extra wait or while it was
  screenshotted, did too. Each browser now has one check for every page it serves, and each
  request is judged by the policy of the page it belongs to: the page, its frames, and the
  popups it opened. On a browser crawlberg launched, a request that belongs to no checked page
  is refused; on a browser reached through `browser.endpoint`, another client's tabs are left
  alone. A launched browser no longer opens a tab of its own. On a pooled browser, on a
  `browser.endpoint` Chrome, and on a one-shot browser without a profile, every page crawlberg
  opens lives in a browser context of its own, so it shares no cookies or storage with the
  browser's other pages; on a `browser.endpoint` Chrome the page starts with the browser's cookies.
  When a fetch or a session ends, that context is disposed: the page, its popups and every request
  of theirs Chrome still holds go with it, so nothing they sent reaches the network after. The
  check is turned off only when it stops, once every page it opened is gone; a request Chrome
  pauses after that is not checked. A session with a `browser_profile` runs on a Chrome launched
  for it alone, so its page uses the profile's own storage, cookies and localStorage included, and
  the check stays on until that Chrome is closed. In `interact`, a main-frame navigation refused
  before the actions fails the session with the SSRF policy error, as it fails a scrape. This
  applies to the Chromiumoxide backend. (#153, #165, #168, #281, #506)
- **An `interact` action whose request the SSRF check refused was reported as successful.** The
  action now fails with the SSRF policy error that names the refused URL. A refused request counts
  for the action that was running when the check received it from Chrome, so on a busy host it can
  count for the next action. This applies to the Chromiumoxide backend. (#167)
- **An `interact` session or a scrape could send requests to a refused address as it ended, on a
  busy host.** Under load Chrome can take longer than the close limit to destroy a page or a popup,
  and a page can still send just after Chrome reports it destroyed. The check then turned
  interception off, or closed the browser, while they were still sending, and their requests went
  out. A browser crawlberg launches with a throwaway profile, for one `interact` session or one
  scrape, now keeps interception on until it is killed, so its requests stay paused until the
  process is gone. crawlberg then ends every process of that Chrome, waits until none of them and
  none of their threads is left, for at most what is left of `browser.shutdown_timeout`, and then
  removes the profile. Ending the processes and removing the profile are not bounded by that
  timeout, so on a busy host an `interact` call can return later than `browser.shutdown_timeout`
  after its last action. A scrape returns its result before the kill. A scrape with a saved
  `browser_profile` and a scrape through a `BrowserPool` end as before. This applies to the
  Chromiumoxide backend. (#468)
- **The SSRF check on a `browser.endpoint` browser turned interception off while the page of an
  `interact` session or a scrape, or a popup of it, was still open, on a busy host.** crawlberg
  cannot kill a browser it does not own, and the check stopped waiting for the close after a time
  limit. Each page of the check now has a browser context of its own. Closing the page disposes
  that context, which takes the page, its popups and their pending requests before the check
  turns interception off. The browser and its other tabs stay open. This applies to the
  Chromiumoxide backend. (#484)
- **A scrape on a `browser_profile` could fail right after another one on the same profile.** A
  scrape returns before its Chrome has exited, and Chrome writes the profile until it exits. A
  scrape that started then copied the profile while Chrome renamed files in it, and failed with
  "failed to copy profile file". A session on a saved profile now holds the profile until its
  Chrome has exited, or has been killed after `shutdown_timeout`. A session on an unsaved profile
  copies the profile only while no Chrome writes it, and two saved sessions on one profile no
  longer run at the same time. A session that waits for the profile counts the wait against its
  `overall_timeout`. This applies to sessions in one process. A symlink to a profile directory
  shares the hold of the directory it points to. (#524)
- **`interact` silently ignored `browser_profile` and `save_browser_profile`.** It still uses an
  isolated session rather than reading or writing the named profile, but now logs a warning that
  both settings are ignored. This applies to both browser backends. (#559)
- **A browser-mode page did not say which of its requests the SSRF policy refused.** A refused
  image, script, frame or `fetch()` keeps the page, and the result now lists each refused address
  in `ssrf_refused_urls`, without its credentials. An `interact` result lists the refusals of
  the whole session, the extra wait included. The first five refusals of a page are each logged
  as a warning, then one warning reports the count, so a page cannot flood the log. This applies
  to both browser backends, for scrape, crawl and `interact`.

- **Feeds, hreflang alternates, canonical links and icons reported `file:` and `blob:` addresses.**
  They still used the older check from #307, which drops only `data:`, `javascript:` and
  `vbscript:` addresses, so a `file:///etc/passwd` feed, hreflang or canonical link, or a `file:` or
  `blob:` icon, was still reported, though the crawler can never fetch it. Feeds, hreflang
  alternates and canonical links now use the same rule as the links list, images and asset
  discovery: only `http` and `https` addresses are reported. Icons use that rule too, but keep
  #307's exception for an inline `data:` address: a `data:` icon is a real, usable icon that needs
  no fetch, unlike a `file:` or `blob:` address, so it still comes back. (#472)

- **The links list, images and asset discovery reported `file:` and `blob:` addresses.** A
  `file:///etc/passwd` link, a `<img src="file:///x.png">`, or a stylesheet or script with a
  `blob:` address was reported as a normal link, image or asset, though the crawler can never
  fetch any of them: it fetches only `http` and `https`. A `file:` link also raised an SSRF
  warning during a crawl. All three now report only `http` and `https` addresses. The links list
  already dropped `mailto:`, `tel:` and the inline `data:`, `javascript:` and `vbscript:` schemes;
  images and asset discovery now drop `mailto:` and `tel:` too. (#275, #341)

- **Images, feeds, hreflang alternates, icons and canonical links reported an address that does
  not resolve.** An address the URL parser cannot read, such as `http://[bad/x`, or a relative
  address under a `<base href>` that cannot take one, such as `blob:https://example.com/b`, was
  reported as the page wrote it. It is now dropped, as the links list already dropped it. (#472)

- **The Open Graph and Twitter card address fields reported any address.**
  `<meta property="og:url" content="file:///etc/passwd">` gave `file:///etc/passwd`, and a
  `javascript:` address came back unchanged. `og_url`, `og_image`, `og_video`, `og_audio` and
  `twitter_image` now use the same rule as the links list: the address resolves against the page's
  base URL and is kept only when it is `http` or `https`. A whitespace-only address is absent, and
  a tag whose address is dropped does not clear an address that another tag set. (#312, #472)

- **`map()` reported sitemap entries of any scheme.** A sitemap `<loc>` of `file:///etc/passwd`,
  `mailto:`, `ftp:` or any other scheme came back as a page. `map()` now reports only `http` and
  `https` entries, and skips a robots.txt `Sitemap:` line or a sitemap-index child with another
  scheme instead of trying to fetch it. (#565)

- **The browser crate's default SSRF policy left the reason out of a connect-time refusal.** When
  `crawlberg-browser` is used directly, its check of a URL names the reason it refuses an
  address, such as `loopback`. A host name or address refused when the connection resolves it
  gave the same decision with no reason. That refusal now ends with the reason too, so
  `::ffff:127.0.0.1` gives the reason `loopback` at both checks. (#532)
- **The full and CLI Docker images, and the Elixir NIF builder, failed before compiling.** Their
  build rewrote the workspace `members` list with a pattern that expects a one-line array, and the
  root `Cargo.toml` writes it on several lines, so cargo could not load the copied manifest. The
  full and CLI images now replace the whole array and copy `crates/crawlberg-browser`, which the
  core crate names as a path dependency. The NIF builder no longer rewrites the root manifest: the
  NIF crate is its own workspace, so it builds from its own manifest. (#553)

- **`soft_http_errors` did not cover a refusal by a custom retry policy or an antibot strategy.**
  A page refused by a custom retry policy, or by an antibot strategy that asks for browser
  escalation, came back as an error when no escalation tier was left. It now comes back as the
  same soft page as a WAF block: the refused status for a 4xx or 5xx, and 403 for a 2xx. A tier
  left to escalate to still runs first. (#549)

- **`soft_http_errors` reported every WAF block as a 403.** A 429 or 503 block page came back
  with status 403, so a caller could not tell a rate limit from a forbidden response. A WAF block
  now reports the status of the response it refused. A block page served with a 2xx status still
  reports 403, because a 2xx soft error reads as success. A 429 or 503 soft page has no markdown
  or response metadata, like a 403 or 404 page. (#518)

- **A robots.txt that opens with a UTF-8 byte-order mark lost its first group.** The mark stayed
  attached to the first `User-agent` line, that directive did not match, and the whole group,
  rules included, was dropped, so every path was allowed. A leading byte-order mark is now
  skipped once, as RFC 9309 asks. (#516)

- **The sitemap walk and the well-known `/sitemap.xml` fallback gave no URLs for a gzip sitemap
  served with the wrong content type.** A robots.txt `Sitemap:` directive, a sitemap-index child,
  and the `/sitemap.xml` fallback each decided whether to inflate a body by its content type, so a
  gzip sitemap served as `application/octet-stream`, or recognised only by its gzip header bytes,
  yielded no URLs there, while `map()`'s direct fetch read the same file. All three now inflate a
  body that starts with the gzip header, whatever its content type says, the way the direct fetch
  already did. (#534)

- **The browser fallback read robots.txt with its own parser, which dropped the first group after
  a UTF-8 byte-order mark.** A file that opened with the mark and disallowed `/private` let the
  browser open `/private`. The browser fallback now uses the crawl engine's robots.txt parser,
  which moves into the new `crawlberg-robots` crate; `crawlberg::robots` re-exports it unchanged.
  That parser already skips a leading byte-order mark (#516). The browser fallback now decides
  these cases the way the crawl engine does (#540):
  - The longest matching rule wins. Before, any matching `Allow` beat a longer `Disallow`.
  - A `*` inside a pattern, such as `Disallow: /*.pdf$`, matches any text. Before, only a
    trailing `*` did, and an inner one matched nothing.
  - A group with several `User-agent` lines applies to each of them. Before, only the last
    `User-agent` line of the group counted.
  - A `User-agent` token applies only when it is a prefix of the crawler's user agent. Before,
    a token that contained the user agent, or that the user agent contained anywhere, also
    matched.
  - A trailing `# comment` on a rule line is ignored. Before, it became part of the pattern.
  - When a group names the crawler, only the groups that name it apply. Before, the
    `User-agent: *` rules applied as well.
  - A rule before the first `User-agent` line joins the first group. Before, the browser
    fallback ignored it.
  - An unknown directive between two `User-agent` lines joins them into one group. Before,
    only the second `User-agent` line counted.

- **When a robots.txt had two groups for the crawler, the crawl engine obeyed only the last
  one.** A file with `User-agent: crawlberg` / `Disallow: /a` and, further down, a second
  `User-agent: crawlberg` group with `Disallow: /c` let the crawler fetch `/a`. The parser now
  combines every group that names the crawler into one, as RFC 9309 section 2.2.1 says, and
  does the same for several `User-agent: *` groups. When two combined groups set a
  `Crawl-delay`, the later one wins. When only an earlier group sets one, that value applies.
  The browser fallback uses the same parser (#540).

- **Browser fetches left their Chrome profile directories in the temp directory.** A one-shot
  fetch, an interact run or a pool that ended without its own cleanup left a `crawlberg-*`
  directory of several megabytes behind: a pool dropped without `shutdown()`, or a fetch whose
  Tokio runtime stopped before its teardown ran. Each such directory is now removed when its owner
  is dropped. First crawlberg stops each process of the Chrome it launched that still uses the
  directory as its profile, and waits up to five seconds for them to exit, because Chrome's helper
  processes outlive the browser and keep writing into it. On Linux 5.3 and later the wait lasts
  until the last thread of each killed process has exited, because a thread still finishing a
  write made the removal fail with "directory not empty". This also works when a launcher script
  runs Chrome as its child, and a shell that only names the directory is left running. This work
  runs on a background thread, so it does not stall other tasks or hold the browser pool's lock.
  A saved `browser_profile` is never removed; only the temporary copy of it is. (#415)

- **`map()` did not follow a meta refresh.** A page that forwards with a
  `<meta http-equiv="refresh">` tag or a `Refresh` header gave no URLs, because the direct fetch
  followed only HTTP redirects. It now follows both the way the crawl does: the same tags win,
  only the same HTTP statuses (301, 302, 303, 307, 308) count as a redirect hop, each hop counts
  toward `max_redirects`, each hop passes the SSRF policy, and the seed's credentials go only to
  the seed host. The links come from the page it lands on. A chain that reaches the redirect
  limit, leads back to a URL it already requested, or ends on a missing page now stops there, as
  the crawl does, instead of failing the whole `map()`. (#502)

- **`map()` requested pages that robots.txt or the path filters refuse.** Its direct fetch
  checked each request against the SSRF policy only, so a seed, an HTTP redirect or a refresh to a
  path robots.txt disallows was requested and its links returned, while the crawl refuses the same
  page without requesting it. Each request of the direct fetch now passes the crawl's own checks
  first: `exclude_paths`, `include_paths` for a redirect or refresh hop, and, with
  `respect_robots_txt` on, the robots.txt of the URL's own origin, which fails closed when that
  file is unreachable. A refused URL is never requested, and `map()` returns the crawl's forbidden
  error with the reason, which the REST API answers with a 403. The sitemaps that `map()` reads
  are not checked this way. (#512)

- **A custom retry policy got no status for a 403 or a WAF block.** A plain 403 and a response
  refused as a WAF block ended the attempt with an error that did not keep the response status, so
  `AttemptOutcome.status` stayed empty for them. Both errors now keep the status, so the policy
  reads 403 for a plain forbidden, and 403, 429, 503 or the 2xx status for a block. The built-in
  retry decisions do not change: a forbidden and a WAF block still escalate, and listing 403 in
  `retry_codes` still does not retry them. (#133)

- **Link extraction read markup inside raw-text elements and took the wrong `<base>`.** Only
  `script`, `style`, `textarea` and `title` were treated as raw text, by a hand-written scanner.
  Links and a `<base href>` inside `xmp`, `iframe`, `noembed`, `noframes` and `plaintext`, after
  `<script/>` and in a script inside SVG `foreignObject` were read as real, and a `<!--` in such
  text hid every link after it. Links inside a bogus comment (`<? ... >`, `<!x ... >`, `<![CDATA[`
  outside SVG) and inside an SVG or MathML CDATA section were read as real too. A crawled or
  scraped page is now read once by html5ever with scripting off, and that read decides the raw
  text, the link tags, the base, the meta refresh target and the render hint. Two kinds of page
  are still read twice: a page decoded again from a declared non-UTF-8 charset, and a body cut to
  `max_body_size`. The base is the first `<base href>` in the finished document, as in a browser:
  a `<base>` in a table moves in front of it, and a `<frameset>` drops the body with its `<base>`.
  (#201, #287)

- **One tag with tens of thousands of attributes slowed link extraction quadratically.** The
  HTML parser compares each new attribute name of a tag with every earlier one. Attributes past
  the 1,024th of one tag are now overwritten with spaces before the parser reads the page, so the
  cost grows linearly. Repeated attribute names count toward the limit, so an `href` after the
  1,024th attribute of an `<a>` or `<base>` tag is not read. (#269)

- **A 2xx from a site behind Akamai, Imperva, F5 or Sucuri is returned as content again.** Those
  products stamp their own header on every response they proxy, and a WAF fingerprint that matches
  on response headers alone was enough to refuse the response. Robots.txt, sitemap and asset
  fetches refused every 2xx served through one of them, and the crawl refused such a 200 when its
  body was under 5000 bytes, with the real page already in hand. A header-only fingerprint now
  needs the body to show the interstitial before a 2xx is refused. A 403 behind one of those CDNs
  still blocks. (#231)

- **Every fetch path now makes the same call on a 2xx.** Robots.txt, sitemap and asset fetches
  checked the body of any 2xx up to 100 KB, the crawl checked only a 200 under 5000 bytes, and a
  `WafClassifier` set on the engine flagged a 2xx to the antibot strategy and retry policy on a
  header-only match, so the built-in antibot strategy refused an ordinary 200 behind Sucuri. All
  three now apply one rule: any 2xx status, a body under 5000 bytes, and a header-only match that
  the body corroborates. So sitemap and asset fetches return a 2xx of 5000 bytes or more as content,
  the crawl refuses a 202 or 203 interstitial, and a classifier set on the engine flags a 2xx only
  under the same rule. A robots.txt that is a block page still denies the whole site at any size up
  to 100 KB. (#500)

- **A robots.txt that says "blocked" in a comment is read as rules.** Behind Cloudflare, a
  `server: cloudflare` header and the word "blocked" anywhere in the body matched a block-page
  fingerprint, so a real robots.txt with a comment such as "AI crawlers are blocked below" denied
  the whole site. The robots.txt fetch now leaves whole-line comments (lines that start with `#`)
  out of the fingerprint, so a file whose only match is in such a comment is read as the site's
  rules. Any other body that fingerprints as a block page still denies the whole site at any size
  up to 100 KB. So does a robots.txt with the word in a rule (`Disallow: /blocked-users`) or in a
  trailing comment, and any body that contains `<`, which the check reads whole. (#507)

- **A sitemap that lists a URL saying "blocked" is read behind Cloudflare.** A `server: cloudflare`
  header and the word "blocked" anywhere in a small body matched a block-page fingerprint, so a
  sitemap listing a URL such as `/blog/why-we-blocked-the-old-api` was refused and `map()` lost
  every URL it listed. A body with one `urlset` or `sitemapindex` root, at least one entry, and no
  text outside its entries is now read as a sitemap, gzipped or not. A block page served at a
  sitemap URL is still refused. (#515)

- **`crawl_waf_blocks_total` counts refused responses, once each.** The counter moved on every
  WAF fingerprint match. The fetch path fingerprints one response more than once, so a single block
  added one or two, and a `TomlClassifier` set on the engine added one for every match it made. It
  now moves once for each response refused as a WAF block: by the fetch path, for a 403, 429 or
  503 challenge or a 2xx interstitial, or by the engine, when its antibot strategy or retry policy
  refuses a response as a WAF block. A response that is returned as content does not count, and no
  response counts twice.

- **An address with an upper-case scheme was refused.** The REST API and the MCP tools tested a
  caller-supplied address against a lower-case `http://`/`https://` prefix, so `HTTP://example.com/`
  and `Https://example.com/` were rejected even though the URL parser accepts them. A URL scheme is
  case-insensitive. Both entry points now parse the address and read the parsed scheme instead. (#221)

- **A configured user-agent rotation list had no effect on the wasm target.** Every wasm
  request sent the fixed default agent, and robots.txt was judged for that same default agent.
  Neither used the rotation list. The wasm crawl loop now picks the next rotation agent once per page,
  judges that page's robots.txt for it, and sends that same agent on the request -- the per-page
  behavior the native crawl loop already had. A page on another origin, such as a subdomain
  under `allow_subdomains`, is now judged by that origin's own robots.txt, as on native. Before,
  every page was judged by the seed origin's robots.txt, so such a page can now be refused. (#483)

- **The WASM crawl sent `auth` and `custom_headers` to every host it followed.** The sequential
  crawl loop, which the WASM build runs, scraped each page as if it were a new seed, so a subdomain
  page followed under `allow_subdomains` or a document link on another host got the credentials
  set for the seed host. Each page now keeps the seed's credential scope, as the native crawl
  already did. The same loop also dropped the user name and password written into a seed URL, so
  no page got them, not even the seed; every page on the seed host now gets them. (#404)

- **The native browser backend connected to a rebinding host's second DNS answer.** It checked
  a host's addresses against the SSRF policy, and then its HTTP clients resolved the host again
  to connect. A DNS answer that changed between the two lookups reached an address the policy
  denies. The page, redirect, script `fetch()`, module import and stealth clients now connect
  only to the addresses the policy checked, as the HTTP path already does. With a configured
  proxy, the proxy resolves the target. Two setups that worked before are now refused, as on the
  HTTP path: a proxy set by the `HTTP_PROXY` environment variable whose host name resolves to a
  private address, and, when `crawlberg-browser` is used directly with its default policy, a host name
  that resolves to a private address. A refusal now names the policy's reason. (#451)

- **IPv6 forms that carry an IPv4 address bypassed the SSRF deny-list.** The deny-list matches
  within one address family, so only the IPv4-mapped and NAT64 well-known forms were unwrapped
  before it ran; `http://[::10.0.0.5]/`, `http://[::ffff:0:a00:5]/` and `http://[2002:a00:5::]/`
  all reached the private host 10.0.0.5 with `deny_private` on. The IPv4-compatible (`::/96`),
  IPv4-translated (`::ffff:0:0:0/96`), 6to4 (`2002::/16`) and ISATAP (interface identifier
  `0000:5efe` or `0200:5efe`, under any prefix) forms are now unwrapped as well, and the embedded
  address is checked against the IPv4 rows of the deny-list. The pre-connect check, the
  connect-time resolver and the browser crate's fallback validator apply the same rules. An
  address that only has the shape of one of these forms is refused for the address it seems to
  carry: `2001:db8::5efe:1:1` reads as `0.1.0.1` and is refused. (#109)

- **A Teredo address reached the private IPv4 address it carries.** A `2001:0::/32` address
  stores the client's IPv4 address inverted in its last 32 bits, and nothing decoded it, so
  `http://[2001:0:4136:e378:0:ffff:5601:5601]/` reached 169.254.169.254 with `deny_private` on.
  The address is now decoded and checked like the other embedded forms, so a Teredo address that
  carries a public IPv4 address still works. (#196)

- **The local-use NAT64 prefix `64:ff9b:1::/48` carried private addresses past the deny-list.**
  The IPv4 address in the last 32 bits, where a /96 network puts it, is now checked, so
  `http://[64:ff9b:1::a00:5]/` is refused. A /48, /56 or /64 network puts the address elsewhere
  and its unused bits read as zeros at that position, so a reading whose last three octets are
  zero is skipped unless the prefix bytes after the /48 are zero too. Addresses of those three
  network sizes are checked as IPv6 only, as before. (#108)

- **The reserved range `240.0.0.0/4` passed the SSRF deny-list.** With `deny_private` on,
  `http://255.255.255.255/` and every other address in the range was fetched, plain or embedded
  in an IPv6 form that carries an IPv4 address. The range is now refused everywhere the deny-list
  applies, with reason `private_network`, the same reason the shared address space and the other
  RFC 1918 ranges already report. (#173)

- **A denial reason could name an address the allowlist permits.** The reason was classified from
  the first deny-listed candidate rather than the first one the allowlist did not admit, so an
  allowlisted `fe80::/10` with `fe80::5efe:10.0.0.5` reported `link_local` instead of
  `private_network`. The allow or deny decision itself was always correct.

- **With user-agent rotation on, robots rules were matched against the configured agent, not
  the one a request actually sent.** A rotating crawl sends a different agent per request, but
  robots.txt group selection and meta or header directives always judged the page against the
  single configured agent. A site's rule for the agent that made the request was ignored, and a
  rule for the configured agent applied even to a request that used a different one. Every
  robots decision now reads the agent the request actually sent; a crawl that does not rotate
  sees no change. A `user-agent` set through `custom_headers` is judged the same way, since it
  is the agent the request actually sends. With `browser.mode` set to `always` or `stealth`,
  the browser never sends a rotated agent; robots decisions for a browser-fetched request now
  read the browser's own configured or custom-header agent, so a disallowed browser request is
  blocked instead of judged against an agent it never sends. With `browser.mode` set to `auto`,
  a request that escalates mid-crawl to the browser tier is now judged again at that point: the
  earlier robots decision, made before the tier was known, read whatever agent the HTTP attempt
  used, and the browser tier ignored it and sent its own agent regardless. Escalating to the
  browser tier now re-checks robots.txt against the agent the browser actually sends, and a
  disallow stops the fetch. An empty or whitespace-only
  `custom_headers["user-agent"]` value now counts as absent for both robots judging and what
  every tier sends, instead of being sent on the wire as a literal blank agent. A robots.txt,
  sitemap or asset fetch with a `custom_headers` agent configured alongside `user_agent` sent
  both as two separate `User-Agent` header lines; it now sends the custom-header agent once.
  (#423)
- **The credential redactor passed a malformed address through unchanged.** It only stripped
  `user:pass@` when the value parsed as a URL with a host. A value that failed to parse, such as a
  stray space in the host, a bare `user:pass@host` with no scheme, or an address inside a longer
  message, was logged exactly as received, credentials included. Such a value is now replaced
  whole with `[address hidden: it may carry credentials]` when it contains an `@`, and a value
  without an `@` comes back unchanged. A URL that parses with a host and has no whitespace is
  redacted as before. The placeholder also replaces any other value with an `@` that is not one
  URL with a host: an "invalid URL" error for such an address, a `mailto:` or `data:` value, and
  a message that mentions an e-mail address. Three sitemap warnings (the document budget cap, the
  index depth cap, and cycle detection) also logged their address without going through the
  redactor at all; they now do. A call that starts from an address with no host and an `@`,
  such as `user:pass@host/path`, is now refused before any trace span or event records the
  address, and the error names it as `(unparseable URL)`, so the password no longer reaches a log
  field. (#236, #243, #261, #399)

- **Links after an abruptly closed or empty comment were not extracted.** `tl` ends a
  comment by searching for a literal `-->` right after the opening `<!--`, so it never
  recognized `<!-->` or `<!--->`, which close before any `-->` exists; a comment closed with
  `--!>` instead of `-->`; or a plain, valid, empty comment, `<!---->`, whose close sits
  directly against the opener's own dashes. After any of these, `tl` kept reading as if
  still inside the comment, so every link, image and base address past it was missed. The
  raw-text masking pass now neutralizes all of them before `tl` parses the page. (#212)

- **Robots directives ignored `none` and applied a crawler-scoped directive to every crawler.**
  A robots meta tag or `X-Robots-Tag` header that said only `none` was read as neither noindex
  nor nofollow, although `none` means both. A header addressed to one crawler, such as
  `X-Robots-Tag: googlebot: noindex`, bound crawlberg too, and a meta tag named for crawlberg's
  own user agent was ignored. `none` now sets both directives. A directive named for a crawler
  binds crawlberg only when that name is a prefix of crawlberg's user agent, the same rule
  robots.txt groups use, and the generic `robots` form still binds every crawler. (#156)

- **A URL's password leaked, and credentials reached hosts they were not for.** The `user:pass@`
  of a caller's URL stayed inside every URL the engine handled, so logs, errors, results, cache
  keys and plugin callbacks each had to redact it, and several did not. Relative links and
  redirects also copied it to other pages. The engine now removes it at the start of each call
  and keeps it as a credential for the seed host only. The same host rule now applies to `auth`:
  a page, asset, robots.txt or redirect on another host gets no credentials. Both browser backends
  now send `Basic`, `Bearer` and header credentials only to the seed host, one request at a time,
  instead of to every host a page loads from. robots.txt and sitemaps on the seed host are now
  fetched with the credentials. A response fetched with credentials is never stored in or served
  from the response cache or the shared robots.txt cache. (#378, #387, #388, #389, #390)

- **A page could make the browser send a URL with userinfo.** A page-supplied link, sitemap
  entry or redirect target loses its userinfo, and in the native browser a navigation, module
  import or `fetch()` to a URL with userinfo is refused, as the Fetch standard requires. The
  chromiumoxide backend refuses such a request too. A URL that does not parse is reported
  without its text. (#347, #357, #382)

- **Custom headers reached every host a crawl touched.** Plain HTTP requests and both browser
  backends sent `custom_headers` to other hosts: linked documents, third-party subresources and
  cross-host redirect targets. They now go only to requests on the seed's host, the same as the
  credentials. (#393)

- **A page script in the native browser did not get the seed-host credentials.** A `fetch()` or a
  module import to the seed's host now carries the credentials and the custom headers, as it does
  in Chrome. A module redirect to another host drops them, and every module redirect is now
  checked against the SSRF policy. (#409)

- **A link whose `href` does not resolve was returned as raw text.** Such a link is now left out
  of the page's links instead of appearing with its unresolved text as its URL. (#394)

- **A redirect to a non-web address failed the whole scrape.** A 3xx whose `Location` was a
  `mailto:`, `tel:`, `javascript:`, `data:`, `file:`, `about:` or `ftp:` address, or a custom app
  scheme, failed `scrape()` with an `ssrf_policy_violation` error. A crawl stopped on such a seed
  with the same error and dropped such a linked page. A browser sends no request for such an
  address. That `Location` is now no redirect target, matching how a refresh naming such an
  address directly already was, so the 3xx response is the page. The same holds for robots.txt,
  sitemap and asset fetches: a robots.txt that redirected to such an address made the crawl refuse
  the whole site. Redirects to `http` and `https` addresses still pass the SSRF check. (#361)
- **A redirect to a non-web address still failed or timed out with the Chromiumoxide backend.**
  Chrome either refused such a redirect or waited for an external application. Browser mode now
  returns the original 3xx status, headers and URL without following the `Location`. This also
  applies when a page script starts the redirect after its initial load. Its body is empty in this
  backend: at the response headers Chrome receives an internal 200 with only sandboxed plain-text
  headers, so it cannot act on the non-web `Location`, attachment or HTML content.
  (#471)
- **The Chromiumoxide interaction renderer script ignored `browser.timeout`.** A configured
  post-navigation `eval_script` could wait for chromiumoxide's fixed 30-second command deadline
  when the page's renderer was busy. It now fails within the configured browser timeout. (#569)

- **A relative meta refresh could still fail the scrape once it resolved to a non-web address.**
  A meta refresh target was checked for a fetchable scheme before it resolved, so a relative
  target passed that check and could still resolve to a `mailto:`, `ftp:` or other non-web
  address afterward, for example under a `<base href>` on such an address, and the fetch then hit
  the SSRF policy. The scheme is now checked on the resolved address instead, the same way the
  `Location` header already was, so such a target is no redirect target either, and the page is
  kept. (#478)
- **A meta refresh target ignored the page's base address.** `<meta http-equiv="refresh"
  content="0; url=next">` under `<base href="/app/">` was requested at `/next`, the page's own
  path, while a browser requests `/app/next`. The target now resolves against the page's base
  URL, the same base the links list uses. A `Refresh` HTTP header still resolves against the
  response's own address: it arrives before any document exists to carry a base element, and
  Chrome ignores the body's base for it too. (#300)
- **A `data:` or `javascript:` base address was used as the page base.** With
  `<base href="javascript:alert(1)//">`, every relative link, image, feed, icon and canonical link
  on the page resolved against the script address, and the markdown kept relative links as
  written. The page base is now the page address when the base address has one of these schemes,
  in any letter case and with spaces around it, as the HTML spec and browsers do. (#311)

- **The browser page used an absolute subresource address without parsing it.** A `<script src>`
  or `<link rel=stylesheet href>` that began with `http://` or `https://` reached the interception
  block list and the network events exactly as written, while a relative address was parsed and
  normalized. A script address with trailing spaces or an inner tab or newline therefore slipped
  past a block pattern such as `*blocked.js` and was still fetched and run. A module script's
  network event also carried the raw address. Every script and stylesheet address in the page
  markup now goes through the URL parser against the page address, and an address that does not
  parse is skipped. (#225)

- **The CLI and `browser.endpoint` config field refused an upper-case `WS://` or `Wss://`
  address.** Both compared the raw text against a lower-case `ws://`/`wss://` prefix, but a URL
  scheme is case-insensitive (RFC 3986 §3.1). Both now parse the address and read its scheme, and
  a websocket endpoint with no host is still refused. The browser connection uses the same parse
  and sends the address with a lower-case scheme, so an upper-case, space-padded or slash-less
  spelling that the checks accept also connects. The CLI's rejection error no longer prints the
  address, the same as the config check. (#343)

- **A failed connection to a remote browser printed its password.** When crawlberg could not
  connect to a `browser.endpoint`, the connect error showed the address as configured, with its
  `user:pass@` credentials and its CDP path token. The error now prints only the scheme, the host
  and the port. (#424)

- **The interact backend's connect error printed a browser endpoint's password.** It built the
  same connect error as the launch path, without redacting the address. It now prints only the
  origin, the same as the launch path. (#473)

- **The SSRF check could print a credential as the refused scheme.** An address written without
  a scheme, such as `user:token@host` or `KEY:@host:1`, parses with its user name as the scheme,
  and the refusal printed that scheme: `disallowed scheme: user`, or `Forbidden URL scheme 'user'`
  from the browser check. The refusal now names the scheme only when it is a known one, such as
  `ftp` or `file`. For any other scheme, `DisallowedScheme` carries `unrecognized` and the browser
  check says the scheme is forbidden without showing it. The address in the same error goes
  through the credential redactor, which hides such an address whole. (#329)

- **A crawl of an address without a host printed its credential.** A crawl refused a seed such
  as `user:token@host` because the seed has no host to read robots.txt from, and the reason named
  the seed as written, so `token` reached the crawl result, the error event and the error hook.
  Such a seed is now refused before the crawl starts and is named `(unparseable URL)`. The robots
  refusal for an address without a host also names it through the credential redactor. (#427)

- **Links with an encoded `&` were crawled at the wrong URL.** The links list kept character
  references as written, so `href="list?a=1&amp;b=2"` was requested as `list?a=1&amp;b=2`.
  Every attribute value that crawlberg reads is now decoded first, as a browser decodes it.
  This also covers image addresses, feed and favicon links, and text such as an image's alt
  text. The `javascript:`, `mailto:`, `tel:` and `data:` addresses that the links list, the
  images list and asset downloads skip are now recognised as the URL parser reads them, in any
  letter case and with tabs or newlines inside, so `java&#9;script:` is skipped like
  `javascript:`. (#86)

- **The native browser never ran a module script loaded from an address.** A
  `<script type="module" src="app.js">` was registered with empty code, so `app.js` was never
  fetched and the page rendered as if the script were absent. The module is now fetched through
  the module loader, with the same SSRF policy, proxy and seed-host credential as an `import()`,
  and then run with every module it imports. Every module address, including each module that a
  module script or an inline module imports, now goes through the interception block list, as a
  classic `<script src>` does, and every module request carries the page's User-Agent. A module
  that fails to load, or whose server does not answer within 10 seconds, is skipped and the other
  scripts still run. The same 10-second bound now also applies to the modules an inline module
  script imports. (#441)

- **Uppercase markup was ignored.** `<A HREF="up.html">` was missing from the links list, so
  the crawl never followed it, and uppercase `<IMG>`, `<TITLE>`, `<META>` and `<LINK>` tags
  were skipped the same way. Tag names now match in any case. (#87)

- **The images list ignored `<base href>`.** Image addresses now resolve against the same
  base as the links list: the first `<base href>`, resolved against the page URL. (#88)

- **Attribute values were matched with exact case.** HTML compares values such as `rel`,
  `name`, `http-equiv` and `type` without case, but crawlberg compared them byte for byte, so
  `<meta name="ROBOTS" content="noindex">` did not mark the page as noindex, and
  `rel="Canonical"`, `rel="Alternate"` and `rel="ICON"` were skipped. These values now match in
  any case. `rel` is a list of words, so it matches when any word matches: `rel="shortcut icon"`
  and `rel="alternate stylesheet"` count, and a link with `rel="External NoFollow"` is
  nofollow. A comma also separates the link qualifiers `nofollow`, `ugc` and `sponsored`, so `rel="ugc,nofollow"` is nofollow too. Asset downloads now also fetch alternate stylesheets. The fallback scan for `<meta>` tags in malformed pages also reads `<META NAME=...>` now. (#100)

- **Feed, favicon, asset and canonical addresses ignored `<base href>`.** They resolved
  against the page URL, and the canonical URL was not resolved at all, so
  `<link rel="canonical" href="c.html">` was reported as `c.html`. They now resolve against the
  same base as the links list, as a browser resolves a `<link href>`. (#101)

- **Attribute values with spaces or parameters were not matched.** `<meta name=" robots ">`
  was not read as the robots tag, and a JSON-LD or feed `type` with parameters, such as
  `application/ld+json; charset=utf-8`, was skipped. A `type` is now compared by its MIME type
  without the parameters. A `type`, `name`, `property` or `http-equiv` value is also compared
  without the ASCII whitespace around it. HTML strips that whitespace from a `<script type>`, but
  not from the others: a browser ignores `http-equiv=" refresh "`. Reading those values with the
  spaces is a deliberate leniency for pages that add them. (#136)

- **An empty canonical link was reported as a canonical URL.** `<link rel="canonical" href="">`
  gave a canonical URL of `""`. An empty or whitespace-only `href` points at the page itself, so
  the page now has no canonical URL. (#137)

- **hreflang addresses were not resolved.** The alternate-language links kept each address as
  the page wrote it, and `<base href>` had no effect. They now resolve against the same base as
  the links list. The language code is reported without the spaces around it, and a link whose
  language or `href` is only whitespace is skipped, as an empty one was. (#126)

- **Attribute values kept CR and NUL characters.** A browser turns CR and CRLF in an attribute
  value into LF, and NUL into U+FFFD. crawlberg did this only for values with a character
  reference, so `href="x.html\r\n"` stayed as written. Every attribute value now gets this
  rewrite. (#160)

- **A feed or icon link with a blank `href` was reported.** `<link rel="alternate"
  type="application/rss+xml" href="  ">` was reported as a feed at the page URL, and an empty
  `href` as a feed at `""`. A feed or icon link whose `href` is empty or only whitespace is now
  skipped, as a canonical or hreflang link is. (#187)

- **The links list dropped Unicode spaces from the ends of an address.** A link such as
  `href="&nbsp;page.html"` was reported as `page.html`, but a browser and the Markdown rewrite
  keep the no-break space. Every address in a page (links, feeds, icons, hreflang, canonical,
  images, assets, the Markdown rewrite and a meta refresh target) now loses only what the URL
  parser removes: control characters and spaces up to U+0020 at either end, and tabs and
  newlines inside. An address with nothing else in it counts as blank. (#191)

- **Images and assets with a blank address were reported at the page URL.** `<img src=" ">`,
  an `og:image` or `twitter:image` of only whitespace, and a stylesheet, script or image asset
  with a blank address each resolved to the page itself. They are now skipped.

- **A `srcset` was split on Unicode spaces.** The first `<source srcset>` candidate was cut at
  a no-break space, and leading commas hid the candidate after them. The list is now split as
  a browser splits it, on ASCII whitespace and commas. An inline `data:` candidate is skipped,
  as an `<img>` one is.

- **A meta refresh target dropped a trailing no-break space.** The target now keeps it, as a
  browser does, and a target of only control characters is no redirect.

- **Some inline and script addresses still reached the images and links lists.** A
  `<picture><source srcset>` whose first candidate was a `data:` address in upper or mixed case,
  such as `DATA:image/png;base64,...`, was reported as an image. An `og:image` or `twitter:image`
  whose content was a `data:` address, in any case, was reported as an image too. Both are now
  skipped, as an `<img>` with a `data:` address is. The links list now also skips `vbscript:`
  links in any case, as it skips `javascript:`. (#200)

- **A refresh target kept its quotes, and the two refresh forms cleaned the target by different
  rules.** A `<meta http-equiv="refresh">` or `Refresh` header written as `0; url='/next'` sent the
  crawl to `'/next'` with the quotes, where a browser goes to `/next`. The `Refresh` header target
  was trimmed by the Unicode whitespace rule, which drops a no-break space, while the meta refresh
  target was cleaned by the URL parser's rule, which keeps it. Both forms now use one reader that
  follows the HTML refresh steps: a leading delay, then `;`, `,` or whitespace, then an optional
  `url=` in any case, then an optional pair of matching quotes. The URL parser's rule then cleans
  the target. As in a browser, a value with no leading delay is not a refresh, and a target without
  `url=` is followed, so in `0; /go?url=/elsewhere` the target is `/go?url=/elsewhere`. A refresh
  to an address the URL parser reads with a scheme the crawl cannot fetch, such as `mailto:`,
  `javascript:` or `data:`, is no longer a redirect: the page is kept, where the scrape used to
  fail with an SSRF policy error. (#206, #208)

- **A page with several meta refresh tags was sent to a different target than a browser.** The
  crawl skipped a meta refresh with a blank target and followed the next one, and otherwise
  followed the first tag. Chrome acts on the refresh with the shortest delay, and on the later tag
  when two delays tie, and a blank or self target reloads the page. The crawl now chooses the same
  tag, and stays on the page when that tag reloads it. A `javascript:` refresh takes no part in
  that choice, as the HTML refresh steps require, so a later refresh can be used. (#279)

- **The Alef pin named 0.97.0, but the committed Go binding already carried 0.97.1's goroutine
  thread pinning.** Regenerating with the pinned 0.97.0 binary drops `runtime.LockOSThread` around
  the FFI's cgo calls; regenerating with 0.97.1 reproduces the committed binding exactly.
  `alef verify` did not compare the Go binding with a fresh render, so it could not see the
  mismatch. Repinned Alef to 0.97.1. Alef 0.97.1 no longer turns off the SSRF private-network
  check in the wasm e2e tests and wasm doc snippets on its own, so `alef.toml` now asks for it with
  `wasm_config_overrides`. The Rust mock server in `e2e/rust` and `test_apps/rust` is regenerated
  with 0.97.1. (#412)

- **Images with a script address were reported.** The images list skipped only `data:`
  addresses, so `<img src="javascript:...">`, a `vbscript:` `<source srcset>` or an `og:image` of
  `javascript:...` came back as an image. It now skips `data:`, `javascript:` and `vbscript:`
  addresses in any case, as the links list does. Asset discovery now skips the same addresses when
  it finds assets on the page. No asset with one of these addresses was downloaded before, because
  the downloader accepts only `http:` and `https:`. (#276)

- **Feed, favicon, canonical and hreflang links with a script address were reported.** A
  `<link rel="icon" href="javascript:...">` came back as the page's favicon, and a `javascript:` or
  `vbscript:` feed, canonical or hreflang link came back as an address. Feed, canonical and
  hreflang links now skip `data:`, `javascript:` and `vbscript:` addresses in any case, as the
  links list does. Favicons skip the script schemes and keep any `data:` icon, whatever its media
  type. These links, and the `<source srcset>`, `og:image` and `twitter:image` entries of the images
  list, are checked on the address after it resolves against the base, so an address that resolves
  to a script scheme is skipped too. (#291)

- **Link extraction could disagree with the markdown about the same tag.** Link extraction read
  every page with tl. On a page with an unterminated quote or a stray `=` before a tag's `>`, tl
  could read a different tag boundary than the page's real structure, so the links list showed no
  link, or the wrong address, for a link the markdown still carried. Each real `<a>` start tag is
  now rewritten into unambiguous form first -- one copy of each attribute, double-quoted, as
  html5ever's tokenizer reads it -- so link extraction and the markdown agree on the same tag. This
  reads every page's links a second time and is slower on a link-heavy page; a well-formed `<a>`
  tag is rewritten to itself. (#294)
- **The bypass provider could expose a vendor API key.** For a vendor that takes its key as a
  query parameter, the vendor's request URL carries the key. `BypassProvider::fetch` returned that
  URL as the response's `final_url`, and its send and body-read errors printed it. A caller of
  `fetch` that read `final_url` or formatted the response with `{:?}` saw the key. Crawl and scrape
  results never carried it, because the engine does not read a bypass response's `final_url`.
  `final_url` is now empty, as the field's contract allows when the vendor does not report the
  resolved URL. The send and body-read errors now name the vendor and the error kind only. (#89)

- **A caller's debug output of a config printed its secrets.** Crawlberg does not log these types,
  but a caller that formats one with `{:?}`, such as `tracing::debug!(?config)`, a panic or an
  `expect` message, printed a bypass provider config's API key, token or auth header value. The
  same held for custom request headers, a CDP endpoint token, proxy credentials in a browser session
  key, the REST API token, cookie values and the native browser's proxy URL. Each now prints `***`
  in place of the secret and keeps the non-secret fields. A browser `eval_script` prints as `***`
  with its length, because a script can embed a token. This covers the Rust types only: the
  language bindings define their own config types, and their `repr` and `inspect` output is
  unchanged. (#118, #290)
- **An unclosed `${` in a bypass provider config echoed its value.** The loader error printed the
  whole config value, which can hold a secret. It now names the field and the byte position. (#119)
- **A config validation error echoed the rejected `browser.endpoint`.** An endpoint that is not
  `ws://` or `wss://` printed the value, so one carrying a `?token=` parameter reached the error
  text and, through it, an API error body. The error now names only the field and prints no part
  of the value, not even redacted: the endpoint is a capability, and the field name is enough to
  find it. The `proxy.url` error has not carried the value since #401. (#118)

  Redaction covers `Debug` and error `Display`. `serde` serialisation is deliberately unchanged:
  `CrawlConfig`, `BrowserConfig`, `ProxyConfig`, `AuthConfig` and `CookieInfo` still serialise
  every secret in full, because a config must round-trip through `to_json()`/JSON exactly. Treat
  serialised config as secret-bearing.

- **A caller's debug output of a bypass provider config printed `${ENV}` values.** Crawlberg
  prints only the vendor name for a provider, but a caller that formats a loaded `ProviderConfig`
  with `{:?}` saw a secret substituted into the endpoint, a fixed query value or the JSON body
  template. The endpoint now prints as its origin only: the scheme, the host and a non-default
  port, or `***` when it does not parse as an absolute URL or has no host. Each query value prints
  as `***`. The body template prints as `***` with its length, and with whether it holds the
  `{{url}}` marker. (#144, #152)
- **A CDP endpoint token in the URL path printed in full.** The canonical endpoint is
  `ws://host:9222/devtools/browser/<GUID>`, and the GUID in the path is the capability that drives
  the browser. Redaction covered only the userinfo and the query, so the debug output of
  `browser.endpoint` and `BrowserPoolConfig.browser_endpoint` printed the GUID, and an endpoint
  that did not parse printed whole. Both now print through
  `crawlberg::net::redact::redact_url_to_origin`, the origin-only helper the bypass provider config
  uses, which prints `***` for a value without a host. A proxy URL now prints as its origin too,
  in a `ProxyConfig`, a browser session key and a static proxy provider. The port stays: it tells a container-mapped endpoint from the default 9222, and it is
  no more secret than the host. Two pooled endpoints on the same host and port now print the same.
  (#152)
- **A failed bypass request logs its cause.** The send and body-read errors carry only the error
  kind, so the provider now logs a warning with the vendor, the endpoint's origin and the cause
  chain when a send or a body read fails. (#89)
- **A caller's debug output of a response printed its credential headers.** The fetch and bypass
  responses, the native browser's rendered page and responses, and the network events printed every
  response header value with `{:?}`, including a `Set-Cookie` session cookie. A response header
  map now hides the values of a denylist of credential headers: `Authorization`,
  `Proxy-Authorization`, `Cookie`, `Set-Cookie`, `Authentication-Info`, `X-Api-Key` and
  `X-Amz-Security-Token` print as `***`. Every other response header prints in full, because
  `Content-Type`, `Server` and the like are the debugging value. Header names always stay
  visible. A request header map prints no value at all, whatever the header's name, as
  `custom_headers` in `CrawlConfig` already does. (#141)

- **An absolute redirect target was followed exactly as sent, without going through the URL
  parser.** A relative redirect target was resolved through `Url::join`, which parses it and
  reports the parser's normalized form, stripped of an embedded tab or newline and trimmed of
  leading/trailing spaces. An absolute `http://`/`https://` target skipped that parse entirely
  and came back byte-for-byte as received, so a `Location`, `Refresh`, or `<meta refresh>` value
  crafted with stray whitespace was followed and reported exactly as sent. Both forms now go
  through the same parser, and a target that fails to parse, absolute or relative, is refused
  rather than followed: the redirect source it came from contributes nothing, and the chain
  falls through to the next source or stops. A target is now followed in the URL parser's
  normalized form: an IDN host becomes punycode, a default port is dropped, the host is
  lower-cased, a bare origin gains a trailing `/`, dot segments are removed, a space becomes
  `%20`, and `127.1` becomes `127.0.0.1`.
  (#207)

- **A sitemap-index child `<loc>` was fetched and deduplicated on its raw text instead of its
  parsed form.** A same-host absolute child address, and any child address when the sitemap
  index's own URL failed to parse, skipped the URL parser entirely, so two spellings of the
  same address (a default port, an upper-case scheme, a stray tab) were fetched as two separate
  documents. Every child address is now parsed and normalized before it is fetched and before
  it is used as the duplicate key, matching the resolver already used for redirect targets, and
  a child address that fails to parse is skipped instead of fetched as raw text. (#226)

- **`map()` returned sitemap `<loc>` entries as raw text and kept duplicates.** Two spellings of
  one page, such as `https://example.com/a` and `HTTPS://example.com:443/a`, came back as two
  entries, a relative `<loc>` came back as a bare path, and a `<loc>` that is not an address came
  back as text. Each `<loc>` is now resolved against the sitemap's own URL with the same parser as
  sitemap-index children and returned in its normalized form. A `<loc>` that does not parse is
  dropped. An address is returned once per `map()` call, even when several sitemaps list it, and a
  duplicate does not count toward `map_limit`. A relative `<loc>` is now subject to
  `exclude_paths`, like every other entry. `map_search` now matches the normalized address, so a
  search for a raw spelling, such as a default port or non-ASCII text in the path or host, no
  longer matches. A `<loc>` that is only a query, such as `?q=1`, is dropped instead of reported
  as a page, and so is a `<loc>` that resolves to the sitemap's own address once a fragment such
  as `#top` is ignored. (#323, #340)

- **A sitemap-index child differing only by a URL fragment was fetched twice.** The
  fragment never reaches the server, so `/a.xml` and `/a.xml#x` name the same document, but
  the host rewrite kept the fragment on a same-host child address before it was fetched
  and used as the duplicate key. A relative child address such as `a.xml#x` kept its fragment
  too. The fragment is now dropped from every child address, so both addresses fetch and dedupe
  as one document. (#324, #363)

- **`map()` resolved a relative sitemap `<loc>` against the address it requested, not the one that
  answered.** When `/sitemap.xml` redirected to `/nested/sitemap.xml`, `<loc>page</loc>` became
  `/page` instead of `/nested/page`. Urlset entries and sitemap-index children now resolve against
  the sitemap's URL after redirects. A redirected index that lists its own address, the one it
  answered from, is no longer fetched a second time. (#339, #374)

- **A sitemap index's children on other hosts were fetched from the index's own host.** An index
  at `https://example.com/sitemap.xml` that listed `https://blog.example.com/sitemap.xml` and
  `https://shop.example.com/sitemap.xml` had each child moved onto `example.com` with its path
  kept, so both became `https://example.com/sitemap.xml`, the index itself, and were skipped as a
  cycle. Their pages were missing from the result. Each child is now fetched from its own host,
  as the sitemaps.org protocol allows. The SSRF policy checks every child fetch, and the seed's
  credentials and custom headers still go only to the seed host. (#398)

- **A robots.txt `Sitemap:` line on another host was fetched from the seed's host.** A robots.txt
  on `example.com` that named `https://cdn.example.net/sitemap.xml` made `map()` fetch
  `https://example.com/sitemap.xml` instead, which is another document or none. The sitemaps.org
  protocol lets robots.txt name a sitemap on another host, so the line is now fetched from the
  host it names. The SSRF policy checks the fetch, and the seed's credentials and custom headers
  go only to the seed host. A relative `Sitemap:` line now resolves against the address that
  served robots.txt after its redirects, not the address being mapped. (#268, #349)

- **A non-ASCII `map_search` term never matched an address `map()` normalized.** `map()`
  returns each address in the URL parser's normalized form, which percent-encodes a non-ASCII
  path and encodes a non-ASCII host as punycode, so a search for `café` never found
  `https://example.com/caf%C3%A9` and a search for `bücher` never found the matching
  `xn--bcher-kva.example` host. `map_search` now also matches the decoded, human-readable form
  of the address, alongside the address text itself. The term and the address are compared after
  Unicode normalization and default case folding, so `café` typed with a combining accent finds
  `café`, and `STRASSE` finds `/Straße`. Case folding does not use a locale, so the Turkish dotted
  and dotless `i` do not match their Turkish case partners. (#338)

- **The `search` field of `POST /v1/map` never matched a non-ASCII term.** The REST handler kept
  its own lower-case substring check against the returned address, so `café` never found
  `https://example.com/caf%C3%A9`. It now sets `map_search` for the call, so the endpoint matches
  a term the same way as the CLI and the MCP `map` tool. (#362)

- **`map()` resolved a redirected HTML page's links against the address it requested, not the
  one that answered.** When `/start` redirected to `/dir/page.html`, a link to `x.html` on that
  page came back as `/x.html` instead of `/dir/x.html`. Every other branch of a direct `map()`
  fetch (a urlset, a sitemap index, a gzipped sitemap) already resolved against the URL after
  redirects; the HTML link branch now does too, matching the crawl engine. (#360)

- **One look-around pattern refused the whole configuration.** `include_paths` and `exclude_paths`
  compiled on an engine without look-around or backreferences, so a single `(?!...)` pattern made
  `create_engine` reject every pattern in the list. A pattern that engine accepts still compiles
  there, with the same meaning. A pattern compiles with `fancy-regex` only when the `regex` crate's
  first error is an unsupported look-around or a numbered backreference, so look-around and
  numbered backreferences such as `\1` work. A pattern whose first error is anything else, such as
  `a{2,1}`, still refuses the configuration and names the pattern. When a look-around comes before
  a malformed part in the same pattern, the look-around is the first error and the pattern still
  goes to `fancy-regex` (#283).
  A look-around or backreference pattern is evaluated only on a matched text (the path by default)
  of up to 2048 bytes, and gives up after 100,000 backtracks. A URL whose text is longer, or that
  hits that limit, stays out of the crawl: an exclude pattern counts as a match, an include pattern
  as no match, and one warning per crawl names the pattern. The seed is exempt from the include
  check. The REST API refuses a look-around or backreference pattern in `includePaths` or
  `excludePaths` with a 400. (#78)

- **`DownloadedDocument` printed every response header value under `{:?}`.** The type derived
  `Debug` over `headers`, so a `Set-Cookie` or an echoed `Authorization` reached any debug render
  of a scrape or crawl page result — the value itself, not just the name. `DownloadedDocument` now
  has a hand-written `Debug` that prints `***` for every header on the shared sensitive list
  (`Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`, `X-Api-Key`,
  `X-Amz-Security-Token` and `Authentication-Info`), matching names without case; every header
  name and every other value stays
  visible. Output is unchanged for a document crawlberg produced itself, because no path in the
  core populates `headers` yet — the leak was reachable through a deserialised or caller-built
  value. The Elixir and Ruby binding mirrors keep their own derived `Debug` over their own header
  map and are not covered by this. (#159)

- **A custom retry policy could not read the status of a failed attempt.** `AttemptOutcome.status`
  was always empty when the attempt ended in an error, so a policy written outside crawlberg saw
  the error but not the 503 or 500 behind it. The field now holds the status for every status the
  built-in mapping turns into an error itself (401, 404, 408, 410, 429, 500, 502, 503, 504). It
  stays empty when no response caused the error, such as a connection failure. (#99)

### Changed

- **Upgraded `html-to-markdown-rs` to 3.16.0.** A comma inside a parenthesised `srcset`
  descriptor no longer starts a new candidate, so `a.png (x, b.png 3x ), c.png 2x` shows `c.png`
  and not the `b.png` written inside the descriptor (#320). The front matter shows the base
  address with its character references decoded, `it's` rather than `it&#x27;s` (#103).

### Internal

- **The test that failed when `html-to-markdown-rs` reached 3.15 is replaced.** It checked that
  the converter had no `base_url` option. Tests now hold the behaviours that matter: a
  fragment-only link stays as written, which `base_url` would change, and the link pre-pass
  keeps an empty source empty and strips userinfo from a link. (#190)

## [1.8.0] - 2026-09-27

Includes twelve issues raised by an external evaluation, ten of them in the crawl path. Most were
defects a green e2e suite could not see: the fixtures covering the affected behaviours passed with
the bugs fully present, and the assertion vocabulary cannot express request counts or elapsed time
at all, so the whole "how many requests did we send, and how long did we wait" class was invisible
by construction.

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
  `javascript:` and `data:` addresses stay as written. Because resolved links are longer,
  `fit_content` can now drop a line of relative links that it kept before, the same way it
  already treated absolute links. (#63)
- **The markdown front matter showed the base address as written.** A page with
  `<base href="/other/">` got `base: /other/`. The front matter now shows the resolved base,
  the same address that relative links resolve against. (#94)


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

- **The vendored C header gate failed for lag rather than for a defect.** It required each
  prebuilt platform bundle's `crawlberg.h` to declare exactly the same C API as the canonical
  header, but a vendored copy ships beside a dylib from the last release, so it legitimately
  lacks whatever the canonical header has gained since — adding two `CrawlPageResult` getters
  for #135 turned `main` red for that reason alone. The comparison is now one-directional: a
  declaration the vendored copy has and the canonical header does not still fails, because that
  means a prebuilt bundle promising a symbol HEAD removed or re-signed, while declarations the
  copy is merely missing are reported as lag. (#162)

- **A whitespace-only favicon `href` or image `src` reported the page as its own favicon or image.**
  The guard was `is_empty()`, which is false for `"  "`, and resolving a whitespace-only reference
  against a base yields the base itself, so `<link rel="icon" href="  ">`, `<img src="  ">` and a
  blank `og:image`/`twitter:image` `content` all listed the page URL. Such an address is now
  skipped, via a shared `is_blank_address` helper. Only ASCII whitespace counts as blank, because
  HTML strips nothing else from a URL attribute — an NBSP-only reference is a real value and is
  percent-encoded (#191). Canonical (#137) and hreflang (#126) leak the raw value instead, because
  they do not resolve at all. (#220)

- **A browser fetch reported no response headers at all on the crawl path.**
  `browser_http_to_crawl` built an empty header map, so every header a browser backend had
  collected was discarded before the crawl or the escalation path could read it — `ETag`,
  `Cache-Control` and `X-Robots-Tag` reached no caller and no WAF classifier, however faithfully the
  backend reported them. This is why a `nofollow` sent only as an `X-Robots-Tag` header had no effect
  in browser mode even after the crawl learned to honour it. Headers are now carried through. The
  chromiumoxide backend still hardcodes its own status, content type and headers, so this reaches
  callers today on the native backend only; #166 covers the rest. (#148)

- **Dropping a one-shot browser fetch ran no teardown at all.** Teardown was straight-line code
  after the fetch, reached only once the fetch had finished, so a caller that dropped the future
  while it ran — a cancelled request, a `select!` that lost, a deadline above crawlberg — got none
  of it. Against a `browser.endpoint` Chrome that left crawlberg's CDP websocket open, and the tab
  it had opened open with it: a connected `chromiumoxide::Browser` owns no child process, so
  dropping it does nothing, and its handler loop never ends by itself. Against a launched Chrome
  the process went with the dropped handle, but its `--user-data-dir` stayed on disk. Teardown now
  belongs to a value whose `Drop` runs it, so a dropped fetch and a finished one take the same
  path, and the profile directory is owned by a guard from the moment it is created rather than
  from the moment the launch succeeds — a fetch cancelled mid-launch never had a session to tear
  down. One window remains open: the Chrome process that `Browser::launch` is still building
  cannot be reaped from outside it, so a fetch cancelled during the launch can leave that process
  behind, and it recreates the directory it was just removed from (#198). (#131)

- **Teardown waited five seconds for the CDP handler after killing a hung Chrome.** Killing the
  process does not end the task that runs its CDP handler: chromiumoxide's handler loop returns
  only when a `Browser.close` response reaches it, and a closed websocket merely parks the loop, so
  the wait could never do anything but expire in full and abort the task anyway — about five
  seconds added to every teardown that had to kill a Chrome that had stopped responding. The close
  now reports whether the process exited or had to be killed, and the handler is aborted at once in
  the killed case. A browser that closed cleanly is unchanged, still given the same grace period to
  wind its handler down. (#146)

- **A pooled browser fetch that hit its overall deadline leaked its page.** `overall_timeout`
  wrapped the whole pooled fetch, so expiry dropped that future before it could release the page it
  had borrowed from the shared browser — and `chromiumoxide::Page` has no closing `Drop`, so the CDP
  target stayed open for the rest of the process's life, still running scripts. The deadline now
  bounds page acquisition and navigation individually and the release runs on every path, the
  deadline one included. That release is bounded by `shutdown_timeout` rather than by the overall
  deadline, so a browser too wedged to close a page cannot hold a fetch open, and an
  already-computed result is no longer replaced by a timeout error because teardown was slow.
  Closing a timed-out page's popups is not covered here. (#179)

- **A refused URL's credentials reached the error text.** The browser navigation path and interact
  mode built the SSRF violation error with a struct literal instead of the redacting constructor, so
  a request Chrome was refused at a redirect — `https://user:secret@10.0.0.1/` — carried its
  `user:pass@` userinfo into the error message, and from there into API error bodies, MCP error
  payloads and tracing fields. Both sites now build the error through `CrawlError::ssrf_violation`,
  which redacts the userinfo before it is stored. The pre-navigation seed check and the HTTP
  redirect path already used the redacting path and are unchanged. (#180)

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
- **Two IPv6 deny reasons named only the first address in their prefix.** `classify_private_ip`
  matched `fe80::/10` and `fc00::/7` by exact first-hextet equality, so `feaa::1` and `fd12::1` were
  reported as `private_network` rather than `link_local` and `unique_local` — and `fd12::` is the
  common case, since RFC 4193 randomises the unique-local global id. Both prefixes are now matched
  as ranges. These addresses were refused before and are refused now; only the reason string in the
  error and the log field changes. (#205)


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
