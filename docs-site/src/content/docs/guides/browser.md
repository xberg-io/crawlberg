---
title: "Browser Automation"
---

Crawlberg includes browser-backed rendering for JavaScript-heavy pages. The `browser` feature enables the Chromiumoxide CDP backend; `browser-native` enables the in-process native backend with `BrowserExtras` and network-event capture.

## Browser modes

The `BrowserMode` enum controls when the headless browser is used instead of a plain HTTP fetch.

| Mode             | Behaviour                                                                                                                                                     |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Auto` (default) | Crawlberg first tries an HTTP fetch. If the response looks like it needs JS rendering (e.g. WAF challenge page), it automatically falls back to the browser. |
| `Always`         | Every request goes through the headless browser. Useful for single-page applications or sites that rely entirely on client-side rendering.                    |
| `Never`          | The browser is never launched. Only plain HTTP fetches are performed.                                                                                         |
| `Stealth`        | Every request goes through the browser tier with stealth surfaces enabled.                                                                                    |

Set the mode in `CrawlConfig`:

```rust
use crawlberg::{CrawlConfig, BrowserMode};

let config = CrawlConfig {
    browser: crawlberg::BrowserConfig {
        mode: BrowserMode::Always,
        ..Default::default()
    },
    ..Default::default()
};
```

## Browser backends <span class="version-badge">v0.3</span>

Choose the backend with `BrowserConfig::backend`:

| Backend | Feature | Behavior |
| ------- | ------- | -------- |
| `BrowserBackend::Chromiumoxide` | `browser` | Controls Chrome/Chromium through CDP. Supports external `endpoint` connections and compositor screenshots. |
| `BrowserBackend::Native` | `browser-native` | Uses the in-process native backend. Supports `block_url_patterns`, `eval_script` scrape results, `robots_user_agent`, `capture_network_events`, and `BrowserExtras`. |

```rust
use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig};

let config = CrawlConfig {
    browser: BrowserConfig {
        backend: BrowserBackend::Native,
        mode: BrowserMode::Always,
        capture_network_events: true,
        ..Default::default()
    },
    ..Default::default()
};
```

`BrowserExtras` is populated on `ScrapeResult.browser` only when the native backend handled the request. It can contain the `eval_script` return value, captured network events, and cookies from the browser session.

## Browser pooling

The Chromiumoxide backend keeps a Chrome instance alive across requests; tabs are handed out lazily, the pool auto-recovers if Chrome crashes, and concurrent tabs are bounded by `CrawlConfig::max_concurrent`. No additional configuration is required.

## Browser pool and native executor (Rust)

**This section covers Rust-only injection APIs. Language bindings (Python, Node, Ruby, etc.) rely on built-in pooling configured through static `CrawlConfig` fields only.**

For long-lived Rust processes (e.g. a worker service handling many crawl jobs), amortise Chrome startup cost by constructing and warming a shared `BrowserPool` once, then injecting it into each `CrawlEngine`:

```rust
use crawlberg::{BrowserPool, BrowserPoolConfig, CrawlEngineBuilder};

// Build and warm the pool at startup. `BrowserPool::new` returns an `Arc<BrowserPool>`.
let pool = BrowserPool::new(BrowserPoolConfig {
    max_pages: 8,
    ..Default::default()
});
pool.warm().await?;

// Inject into engines; they reuse the same Chrome instance.
let engine = CrawlEngineBuilder::new()
    .with_browser_pool(pool)
    .build()?;
```

Requires the `browser` Cargo feature.

For the native browser backend (`BrowserBackend::Native`), inject a pre-built `NativeBrowserExecutor` to avoid spawning worker threads per engine:

```rust
use crawlberg::{BrowserBackend, BrowserConfig, NativeBrowserExecutor, NativeBrowserExecutorConfig, CrawlEngineBuilder};

let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig::default())?;

let engine = CrawlEngineBuilder::new()
    .with_native_executor(std::sync::Arc::new(executor))
    .build()?;
```

Requires the `browser-native` Cargo feature.

**Session affinity** keeps same-domain browser sessions alive across requests, preserving cookies and fingerprints. Enable via `BrowserConfig::session_affinity` (defaults to `true`). For custom session routing, use `BrowserSessionPool` (Rust-only):

```rust
use crawlberg::BrowserSessionPool;

let session_pool = BrowserSessionPool::new();
// Sessions are keyed by domain; reuse is automatic across crawl operations.
```

## Connecting to an external browser

Point the Chromiumoxide backend at an already-running Chrome via its CDP WebSocket endpoint instead of launching one locally:

```rust
use crawlberg::{BrowserConfig, CrawlConfig};

let config = CrawlConfig {
    browser: BrowserConfig {
        endpoint: Some("ws://127.0.0.1:9222/devtools/browser/...".into()),
        ..Default::default()
    },
    ..Default::default()
};
```

This is the recommended pattern when running Chrome in a sidecar container or a remote debugging session. `endpoint` is rejected when `BrowserBackend::Native` is selected.

## Browser profiles

Persistent browser profiles retain cookies, localStorage, and other browser state across crawl sessions. Configure them through `CrawlConfig::browser_profile` (named profile to attach) and `CrawlConfig::save_browser_profile` (persist changes on exit):

```rust
use crawlberg::CrawlConfig;

let config = CrawlConfig {
    browser_profile: Some("my-session".into()),
    save_browser_profile: true,
    ..Default::default()
};
```

Profile names are validated against path-traversal — only ASCII alphanumerics, hyphens, underscores, and dots are allowed (max 255 characters). Profiles are stored under `<data_dir>/crawlberg/profiles/<name>` and, on Unix, are created with mode `0o700`.

`browser_profile` and `save_browser_profile` are chromiumoxide-only. The native backend runs an in-process JavaScript engine with no Chrome process and therefore no profile directory to persist; setting either field with `BrowserBackend::Native` logs a warning and is ignored.

## Screenshots

Capture a PNG screenshot of the rendered page by setting `CrawlConfig::capture_screenshot`:

```rust
use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig};

let config = CrawlConfig {
    capture_screenshot: true,
    browser: BrowserConfig {
        backend: BrowserBackend::Chromiumoxide,
        mode: BrowserMode::Always,
        ..BrowserConfig::default()
    },
    ..CrawlConfig::default()
};
```

`capture_screenshot` only takes effect for `scrape()` with `BrowserBackend::Chromiumoxide` and `BrowserMode::Always` or `Stealth`; the result is delivered as a base64-encoded PNG in `ScrapeResult::screenshot_base64`. It is not carried on `crawl()` results at all -- a multi-thousand-page crawl holding one screenshot per page in memory is not a safe default -- and it has no effect with `BrowserMode::Auto`/`Never` or with `BrowserBackend::Native`. Any of those combinations logs a warning instead of silently doing nothing. Screenshots taken during `interact()` (via `PageAction::Screenshot`) are similarly delivered as base64 in `InteractionResult::screenshot_base64`, populated only when that action actually ran.

## WAF detection

Crawlberg detects WAF and bot-mitigation signals with a built-in TOML fingerprint classifier. When a fingerprint matches, the error path includes `CrawlError::WafBlocked { vendor, .. }`; generic or unrecognized blocks may report `unknown` or `generic`. In `Auto` browser mode, those signals can trigger automatic browser escalation. This is not a guarantee that a challenge can be bypassed.

## Wait strategies

After the browser navigates to a URL, it needs to wait for the page to finish rendering.
The `BrowserWait` enum controls this behaviour.

| Strategy                | Behaviour                                                                                                         | Default wait |
| ----------------------- | ----------------------------------------------------------------------------------------------------------------- | ------------ |
| `NetworkIdle` (default) | Waits for a 500 ms settle period after initial page load, giving client-side JS time to execute.                  | 500 ms       |
| `Selector`              | Waits until a specific CSS selector appears in the DOM. Falls back to 500 ms if no `wait_selector` is configured. | Varies       |
| `Fixed`                 | Waits a fixed 2-second duration after navigation completes.                                                       | 2 s          |

Configure in `BrowserConfig`:

```rust
use crawlberg::{BrowserConfig, BrowserWait};

let browser = BrowserConfig {
    wait: BrowserWait::Selector,
    wait_selector: Some("#main-content".into()),
    extra_wait: Some(std::time::Duration::from_millis(200)),
    timeout: std::time::Duration::from_secs(30),
    ..Default::default()
};
```

The `extra_wait` field adds additional sleep time _after_ the wait condition is met.
The `timeout` field is the hard cap on the entire navigation-plus-wait cycle; if exceeded,
`CrawlError::BrowserTimeout` is returned.

:::tip[Choosing a strategy]
Use `NetworkIdle` for most sites. Switch to `Selector` when you know the exact element
that signals the page is ready (e.g. a data table or main content div). Use `Fixed`
only as a last resort for unpredictable sites.
:::
