<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://cdn.jsdelivr.net/gh/xberg-io/assets@v1/banner/readme-banner-dark.svg">
    <img alt="Xberg" width="420" src="https://cdn.jsdelivr.net/gh/xberg-io/assets@v1/banner/readme-banner-light.svg">
  </picture>
</p>

# crawlberg

{% include 'partials/_badges.md' %}

{{ description | trim }}

## What This Package Provides

- **Same crawler as every binding** — one Rust engine behind Python, Node.js, Ruby, Go, Java, .NET, PHP, Elixir, Dart, Kotlin Android, Swift, Zig, WASM, and C FFI.
- **Structured scrape output** — HTML, Markdown, metadata, links, assets, response headers, and extraction warnings with consistent field names.
- **Crawl controls** — depth, page limits, concurrency, URL filters, robots/sitemap handling, rate limits, and partial failure reporting.
- **Rendering path** — optional browser rendering for JavaScript-heavy pages; direct HTTP path for fast static pages.
  {% if language == "typescript" %}
- **Node-first TypeScript API** — NAPI-RS package with typed options/results and async/await.
  {% elif language == "python" %}
- **Python package** — PyO3 bindings for async crawler workflows and data pipelines.
  {% elif language == "ruby" %}
- **Ruby package** — Magnus-backed native extension with Ruby objects for crawl results.
  {% elif language == "php" %}
- **PHP extension** — typed PHP 8.2+ surface for crawl and scrape calls.
  {% elif language == "go" %}
- **Go module** — cgo-backed binding with ordinary Go structs and errors.
  {% elif language == "java" %}
- **Java package** — Panama FFM binding for direct native calls.
  {% elif language == "csharp" %}
- **.NET package** — P/Invoke binding with async-friendly result handling.
  {% elif language == "elixir" %}
- **BEAM package** — Rustler NIF binding with OTP-compatible error tuples.
  {% elif language == "wasm" %}
- **WASM package** — browser and edge-compatible binding for environments where native libraries are unavailable.
  {% elif language == "kotlin_android" %}
- **Android AAR** — JNI-backed Android package with bundled native libraries.
  {% elif language == "swift" %}
- **SwiftPM package** — swift-bridge API for macOS and Linux clients.
  {% elif language == "dart" %}
- **Dart package** — flutter_rust_bridge Future APIs for Dart and Flutter.
  {% elif language == "zig" %}
- **Zig package** — wrapper over the C FFI with explicit errors and memory ownership.
  {% endif %}

## Installation

{% include 'partials/_installation.md' %}

## Agent plugin

The `crawlberg` plugin is available via the `xberg-io/crawlberg` marketplace.

```text
/plugin marketplace add xberg-io/crawlberg
/plugin install crawlberg@crawlberg
```

See [the plugin guide](https://github.com/xberg-io/crawlberg/blob/main/plugin/README.md) for all supported runtimes, CLI prerequisites and standalone public skills.

## Quick Start

{% include 'partials/_quick_start.md' %}

## API Reference

{% include 'partials/_api_reference.md' %}

{% include 'partials/_footer.md' %}
