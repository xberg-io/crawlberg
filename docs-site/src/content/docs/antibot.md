---
title: "Antibot Strategy & Stealth Surfaces"
---

Crawlberg detects WAF/bot-mitigation signals, classifies them with hot-reloadable rules, and can escalate through the configured dispatch chain. Customize policy with `AntibotStrategy`, `DispatchProfile`, retry policy, domain state, and optional caller-supplied bypass providers.

## Architecture

Three layers compose the antibot system:

1. **WAF Classifier** — Matches HTTP responses against `crates/crawlberg/rules/waf_fingerprints.toml` and returns a `WafSignal` with `vendor`, `fingerprint_id`, and `weight`.
2. **Decision Hook** — `AntibotStrategy` trait pair: `pre_request` (warm external state) and `post_response` (inspect response, decide next action).
3. **Dispatch Policy** — `DispatchProfile` combines escalation strategy, retry policy, classifier, domain state, budget, and optional bypass provider.

When a WAF signal is detected, the engine consults the strategy and retry policy to decide whether to retry, escalate to the browser tier, route through a caller-supplied bypass provider, or stop. `Decision::RotateProxy` is present but not implemented; it logs and falls through to `Accept`.

## The `AntibotStrategy` trait

Implement this trait to intercept every request/response pair:

```rust
#[async_trait]
pub trait AntibotStrategy: Send + Sync {
    /// Called once per attempt, before the tower-stack fetch fires.
    /// Return Err to abort this attempt; the retry policy decides what happens.
    async fn pre_request(&self, url: &str) -> Result<(), AntibotError>;

    /// Called once per successful HTTP response, after WAF classification.
    /// Return a Decision that overrides the retry policy for this attempt.
    async fn post_response(
        &self,
        response: &HttpResponse,
        waf_signal: Option<&WafSignal>,
    ) -> Decision;
}
```

The `Decision` enum controls what happens next:

```rust
pub enum Decision {
    /// Continue normally — hand the response to the retry policy.
    Accept,
    /// Retry the same tier after `backoff` (Duration).
    Retry { backoff: Duration },
    /// Rotate to a different proxy and retry (not yet implemented).
    /// Logs a warn!() and falls through to Accept for now.
    RotateProxy,
    /// Force escalation to the Browser tier, bypassing the retry policy.
    EscalateBrowser,
}
```

Errors during strategy execution are returned as:

```rust
pub enum AntibotError {
    /// The pre_request hook failed.
    PreRequest(String),
    /// The post_response hook failed.
    PostResponse(String),
}
```

## `BrowserMode` variants

The `BrowserConfig.mode` field controls when and how the browser is used. `BrowserMode::Stealth` is the new variant:

| Variant | Escalation | Stealth surfaces | Use case |
| ------- | ---------- | ---------------- | -------- |
| `Auto` (default) | HTTP first, escalate to browser on WAF/403 | None | Balanced; handles most sites |
| `Always` | Skip HTTP entirely | None | JS-heavy SPAs |
| `Never` | No browser fallback | None | Performance-critical; no WAF expected |
| `Stealth` | Browser tier for every request | JS patches, native TLS spoofing, UA/viewport defaults | Stealth-hardened mode for challenging sites |

`BrowserMode::Stealth` behaves like `Always` for request routing (every page goes through the browser tier) but additionally enables:

- chromiumoxide JS patches (`crate::stealth::apply_stealth_patches`) that spoof `navigator.webdriver`, `navigator.chromeFlags`, and `navigator.plugins`.
- Native-backend TLS fingerprint spoofing (JA3 randomization).
- Stealth-aware default user-agent when no explicit user-agent is set.
- Forced viewport (1920×1080) to avoid detection via unusual screen sizes.

Previously, a `BrowserConfig.stealth: bool` field existed but had a bug: JS patches always ran regardless of the flag. This field has been removed pre-v1. Use `BrowserMode::Stealth` instead.

## Worked example: per-vendor backoff

Wrap `DefaultAntibotStrategy` to inject custom backoff rules per WAF vendor:

```rust
use std::time::Duration;
use async_trait::async_trait;
use crawlberg::{
    AntibotStrategy, Decision, DefaultAntibotStrategy,
    AntibotError, http::HttpResponse, WafSignal,
};

#[derive(Debug)]
struct VendorBackoffStrategy {
    inner: DefaultAntibotStrategy,
}

#[async_trait]
impl AntibotStrategy for VendorBackoffStrategy {
    async fn pre_request(&self, url: &str) -> Result<(), AntibotError> {
        // Delegate to default (no-op)
        self.inner.pre_request(url).await
    }

    async fn post_response(
        &self,
        resp: &HttpResponse,
        signal: Option<&WafSignal>,
    ) -> Decision {
        match signal.map(|s| s.vendor.as_str()) {
            Some("cloudflare") => {
                // Cloudflare: aggressive backoff
                Decision::Retry {
                    backoff: Duration::from_secs(10),
                }
            }
            Some("datadome") => {
                // DataDome: skip to browser
                Decision::EscalateBrowser
            }
            _ => {
                // Everything else: use default logic
                self.inner.post_response(resp, signal).await
            }
        }
    }
}

// Wire it up
let strategy = std::sync::Arc::new(VendorBackoffStrategy {
    inner: DefaultAntibotStrategy::new(),
});

let profile = DispatchProfile::builder()
    .antibot_strategy(strategy)
    .build();
```

## Defaults

Without an attached strategy, the engine uses `DefaultAntibotStrategy` (defined at `crates/crawlberg/src/types/antibot.rs:132-164`):

- `pre_request` is a no-op.
- `post_response` returns `Decision::EscalateBrowser` when a WAF signal is present, `Decision::Accept` otherwise.

This matches the pre-Cluster-5 escalation logic, so existing code continues to work unchanged.

## WAF detection corpus <span class="version-badge">v0.3</span>

Crawlberg classifies WAF fingerprints via `TomlClassifier` at `crates/crawlberg/rules/waf_fingerprints.toml`. The rules currently cover Cloudflare, DataDome, PerimeterX/HUMAN Security, Imperva, AWS WAF, Akamai, F5, and generic block patterns.

Fingerprints match response headers, body substrings, or status code/header combinations. The classifier supports hot reload via `TomlClassifier::watch()` so rule updates can land without restarting the process.

## Dispatch and EWMA state <span class="version-badge">v0.3</span>

The default retry and dispatch layer can combine WAF signals, transient errors, and `EwmaDomainState` observations. EWMA state lets callers track per-domain outcomes and feed a `LearningRetryPolicy` without requiring a database.

## Tuning dispatch and escalation (Rust)

`DispatchProfile` exposes fields for fine-grained control over the retry and escalation loop. This section is Rust-only — language bindings use the built-in defaults.

Build a profile using the fluent builder, which accepts optional trait-object implementations:

```rust
use std::sync::Arc;
use crawlberg::{
    CrawlConfig, DispatchProfile, EscalationStrategy,
    FixedBudget, EwmaDomainState, LearningRetryPolicy,
    TomlClassifier,
};

let profile = DispatchProfile::builder()
    // Start with a custom escalation strategy: HTTP → Bypass → Browser
    .strategy(EscalationStrategy::BypassThenBrowser)

    // Use a learning retry policy backed by per-domain EWMA state
    .domain_state(Arc::new(EwmaDomainState::new()))
    .retry_policy(Arc::new(LearningRetryPolicy::new(
        Arc::new(EwmaDomainState::new())
    )))

    // Cap escalations at $2.50 per job
    .escalation_budget(Arc::new(FixedBudget::new(250)))

    // Limit total attempts across all tiers
    .max_total_attempts(15)

    // Use a custom WAF classifier (or omit to use the built-in corpus)
    .waf_classifier(Arc::new(TomlClassifier::builtin()))

    .build();

// Attach to the crawl config
let config = CrawlConfig::builder()
    .dispatch(profile)
    .build();
```

Key tuning parameters:

- **`strategy`**: Select the escalation chain (`None`, `BrowserOnly`, `BypassFirst`, `BypassOnly`, `BypassThenBrowser`). See the strategy table above.
- **`retry_policy`**: Implement [`RetryPolicy`](https://docs.rs/crawlberg/latest/crawlberg/trait.RetryPolicy.html) for custom per-attempt decisions. The default `SimpleRetryPolicy` uses static error-to-directive mappings; `LearningRetryPolicy` consults a `DomainStatePort` for priors. Both live in `crate::defaults::dispatch`.
- **`domain_state`**: Track per-domain block rates via `DomainStatePort`. The in-process `EwmaDomainState` is provided; xberg-enterprise supplies a Postgres-backed impl for multi-process deployments.
- **`escalation_budget`**: Enforce per-job spend caps via `EscalationBudget`. `FixedBudget` tracks atomic counters; `UnlimitedBudget` is the default.
- **`max_total_attempts`**: Hard limit on fetch attempts across all tiers. Guards against buggy retry policies. Default: 10.

## Custom WAF rules and hot-reload

The canonical WAF fingerprint corpus lives in `crates/crawlberg/rules/waf_fingerprints.toml`. Load the builtin rules at compile time, or supply custom TOML files for testing or rule updates without restarting.

### Loading rules

Use `TomlClassifier` for TOML-backed fingerprinting (Rust-only):

```rust
use crawlberg::waf::TomlClassifier;
use crawlberg::waf_rules_from_path;
use std::sync::Arc;

// Built-in canonical corpus
let classifier = Arc::new(TomlClassifier::builtin());

// Custom rules from a file
let custom_rules = waf_rules_from_path("custom_waf_rules.toml")?;
let classifier = Arc::new(TomlClassifier::from_rules(custom_rules));
```

### Hot-reload rules on file change

Watch a TOML file for changes and atomically swap rules without restarting:

```rust
use std::sync::Arc;
use crawlberg::waf::TomlClassifier;
use crawlberg::waf_rules_from_path;

let classifier = Arc::new(TomlClassifier::builtin());

// Start watching for file changes, debounced 500 ms
let _watcher = classifier.watch("/etc/crawlberg/waf_rules.toml")?;

// Swap rules atomically; concurrent classifiers see new rules on next call
let new_rules = waf_rules_from_path("/etc/crawlberg/waf_rules.toml")?;
classifier.swap(new_rules);
```

The `WatchHandle` returned by `watch` manages the background file watcher. Dropping it stops watching. Keep it alive for as long as you want to receive updates.

This pattern is Kubernetes-friendly: mount a `ConfigMap` as a file, watch it, and reload rules when the cluster operator updates the ConfigMap. The atomic-swap semantics mean in-flight classifications continue with the old rules; new requests use the new rules.

## Bypass providers

Crawlberg exposes the `BypassProvider` trait and `BypassResponse` type for caller-owned integrations. Providers are responsible for authentication, request shaping, response decoding, cost metadata, and error mapping.

Crawlberg does not ship Bright Data, Zyte, ScrapingBee, or other vendor adapters in the core crate.
