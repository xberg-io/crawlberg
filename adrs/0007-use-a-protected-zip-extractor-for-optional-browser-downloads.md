---
status: proposed
date: 2026-10-31
decision-makers: ["Na’aman Hirschfeld"]
---

# Use a protected ZIP extractor for optional browser downloads

## Context and Problem Statement

The independently published browser driver exposes optional fetcher types and both legacy ZIP
feature names. Its upstream fetcher pins vulnerable ZIP versions and overrides extraction with
an unchecked symlink walker, so a lockfile-only upgrade would retain the vulnerable behavior.

## Considered Options

* Wait for an upstream fetcher release with protected extraction.
* Maintain a small provenance-verified fetcher fork using ZIP 9 extraction.
* Upgrade ZIP while retaining the custom extraction implementation.

## Decision Outcome

Chosen option: "Maintain a small provenance-verified fetcher fork using ZIP 9 extraction", because
no fixed upstream release exists and the public optional download feature must remain available.
This proposal requires review and runtime regression verification before adoption or publication.

Retain `zip0` and `zip8` as feature aliases for one protected backend, including both together.
Preserve valid in-root Chromium symlinks, TLS options and the downloader API. Delegate extraction
to ZIP 9 instead of maintaining another pathname/symlink resolver.

Publish the new fetcher before its driver consumer. The driver requires a new 0.10 release:
ZIP 9 raises its declared minimum Rust version from 1.85 to 1.88, and replacing the publicly
reexported upstream fetcher types changes crate type identity. Do not describe this as a
compatible 0.9 patch or silently establish a conditional-feature minimum-version exception.
The candidate assigns fetcher version 0.9.2 and driver version 0.10.0 and orders
their publication before Core. Core retains registry driver 0.9.5 with default
features disabled, preserving its public Page identity and existing browser behavior.

### Consequences

* Good, because extraction uses ZIP's reviewed recursive symlink protections.
* Good, because all existing public optional feature spellings remain available.
* Bad, because the small fork requires independent maintenance and ordered publication.
* Bad, because independent driver users need to assess the Rust minimum and type identity change.

### Confirmation

The candidate `vendor/crawlberg-chromiumoxide-fetcher/src/runtime/extraction_tests.rs` exercises
real ZIP archives through the fetcher's actual extractor: ordinary Chromium-style files and
in-root symlinks, malicious symlink ordering, preexisting escaping symlinks and permissions.
The tests must first fail against the copied upstream implementation, then pass using ZIP 9.
The feature matrix and standalone advisory audit must pass before this record is accepted.
`.github/workflows/ci-fetcher-security.yaml` additionally exercises real Windows extraction
fixtures and the declared Rust 1.88 toolchain; local macOS results do not qualify those lanes.
