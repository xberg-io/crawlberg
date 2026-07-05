<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://cdn.jsdelivr.net/gh/xberg-io/assets@v1/banner/readme-banner-dark.svg">
    <img alt="Xberg" width="420" src="https://cdn.jsdelivr.net/gh/xberg-io/assets@v1/banner/readme-banner-light.svg">
  </picture>
</p>

# Crawlberg

<div align="center" style="display: flex; flex-wrap: wrap; gap: 8px; justify-content: center; margin: 20px 0;">
  <a href="https://github.com/xberg-io/alef">
    <img src="https://img.shields.io/badge/Bindings-alef%20%D7%90-007ec6" alt="Bindings">
  </a>
  <!-- Language Bindings -->
  <a href="https://crates.io/crates/crawlberg">
    <img src="https://img.shields.io/crates/v/crawlberg?label=Rust&color=007ec6" alt="Rust">
  </a>
  <a href="https://pypi.org/project/crawlberg/">
    <img src="https://img.shields.io/pypi/v/crawlberg?label=Python&color=007ec6" alt="Python">
  </a>
  <a href="https://www.npmjs.com/package/@xberg-io/crawlberg">
    <img src="https://img.shields.io/npm/v/@xberg-io/crawlberg?label=Node.js&color=007ec6" alt="Node.js">
  </a>
  <a href="https://www.npmjs.com/package/@xberg-io/crawlberg-wasm">
    <img src="https://img.shields.io/npm/v/@xberg-io/crawlberg-wasm?label=WASM&color=007ec6" alt="WASM">
  </a>
  <a href="https://central.sonatype.com/artifact/io.xberg.crawlberg/crawlberg">
    <img src="https://img.shields.io/maven-central/v/io.xberg.crawlberg/crawlberg?label=Java&color=007ec6" alt="Java">
  </a>
  <a href="https://pkg.go.dev/github.com/xberg-io/crawlberg/packages/go">
    <img src="https://img.shields.io/github/v/tag/xberg-io/crawlberg?label=Go&color=007ec6" alt="Go">
  </a>
  <a href="https://www.nuget.org/packages/XbergIo.Crawlberg/">
    <img src="https://img.shields.io/nuget/v/XbergIo.Crawlberg?label=C%23&color=007ec6" alt="C#">
  </a>
  <a href="https://packagist.org/packages/xberg-io/crawlberg">
    <img src="https://img.shields.io/packagist/v/xberg-io/crawlberg?label=PHP&color=007ec6" alt="PHP">
  </a>
  <a href="https://rubygems.org/gems/crawlberg">
    <img src="https://img.shields.io/gem/v/crawlberg?label=Ruby&color=007ec6" alt="Ruby">
  </a>
  <a href="https://hex.pm/packages/crawlberg">
    <img src="https://img.shields.io/hexpm/v/crawlberg?label=Elixir&color=007ec6" alt="Elixir">
  </a>
  <a href="https://pub.dev/packages/crawlberg">
    <img src="https://img.shields.io/pub/v/crawlberg?label=Dart&color=007ec6" alt="Dart">
  </a>
  <a href="https://central.sonatype.com/artifact/io.xberg.crawlberg.android/crawlberg-android">
    <img src="https://img.shields.io/maven-central/v/io.xberg.crawlberg.android/crawlberg-android?label=Kotlin&color=007ec6" alt="Kotlin">
  </a>
  <a href="https://github.com/xberg-io/crawlberg/tree/main/packages/swift">
    <img src="https://img.shields.io/badge/Swift-SPM-007ec6" alt="Swift">
  </a>
  <a href="https://github.com/xberg-io/crawlberg/tree/main/packages/zig">
    <img src="https://img.shields.io/badge/Zig-package-007ec6" alt="Zig">
  </a>
  <a href="https://github.com/xberg-io/crawlberg/releases">
    <img src="https://img.shields.io/badge/C-FFI-007ec6" alt="C FFI">
  </a>
  <a href="https://github.com/xberg-io/crawlberg/pkgs/container/crawlberg">
    <img src="https://img.shields.io/badge/Docker-ghcr.io-007ec6?logo=docker&logoColor=white" alt="Docker">
  </a>

  <!-- Project Info -->
  <a href="https://github.com/xberg-io/crawlberg/blob/main/LICENSE">
    <img src="https://img.shields.io/badge/License-MIT-007ec6" alt="License">
  </a>
  <a href="https://docs.crawlberg.xberg.io">
    <img src="https://img.shields.io/badge/Docs-crawlberg-007ec6" alt="Documentation">
  </a>
</div>

<div align="center" style="display: flex; flex-wrap: wrap; gap: 12px; justify-content: center; margin: 28px 0 24px;">
  <a href="https://discord.gg/xt9WY3GnKR">
    <img height="22" src="https://img.shields.io/badge/Discord-Chat-007ec6?logo=discord&logoColor=white" alt="Join Discord">
  </a>
</div>

High-performance Rust web crawling engine for structured data extraction. Scrape, crawl, and map websites with native bindings for 14 languages — same engine, identical results across every runtime.

## What and Why?

Crawlberg is the crawling **substrate**: everything you need to scrape and crawl a site end-to-end from a single Rust core — HTML→Markdown, headless-Chrome fallback, robots/sitemap parsing, per-domain throttling, and an SSRF-safe policy — with identical results across 14 language bindings.

### Features

| Feature | Description |
| ------- | ----------- |
| **Structured extraction** | Text, metadata, links, images, assets, JSON-LD, Open Graph, hreflang, favicons, headings, response headers |
| **Markdown conversion** | Clean Markdown with citations, document structure, and fit-content mode |
| **Concurrent crawling** | Depth-first, breadth-first, or best-first traversal with configurable depth, page limits, and concurrency |
| **14 language bindings** | Rust, Python, Node.js, Ruby, Go, Java, Kotlin (Android), C#, PHP, Elixir, Dart, Swift, Zig, and WebAssembly |
| **Smart filtering** | BM25 relevance scoring, URL include/exclude patterns, robots.txt compliance, sitemap discovery |
| **Browser rendering** | Optional headless browser for JavaScript-heavy SPAs with WAF detection and bypass |
| **Batch & streaming** | Scrape or crawl hundreds of URLs concurrently; real-time crawl events via async streams |
| **SSRF-safe by default** | Refuses loopback, private, link-local, and cloud-metadata addresses; opt out via env var or `CrawlConfig` |
| **Auth & rate limiting** | HTTP Basic, Bearer, and custom-header auth with cookie jars; per-domain request throttling |
| **MCP server & REST API** | Model Context Protocol integration for AI agents plus an HTTP server with OpenAPI spec |

### Supported Platforms

Precompiled binaries for Linux (x86_64/aarch64), macOS (ARM64), and Windows (x64) across every binding. See the [platform support reference](https://docs.crawlberg.xberg.io) for the full matrix.

<div align="center">
  <a href="https://github.com/xberg-io/crawlberg/stargazers">
    <img src="docs/assets/star.gif" alt="Star Crawlberg on GitHub" width="640">
  </a>
</div>

<p align="center"><strong>⭐ Star this repo to show your support — it helps others discover Crawlberg.</strong></p>

## Quick Start

### Language Packages

<details open>
<summary><strong>Python</strong></summary>

```sh
pip install crawlberg
```

See [Python README](https://github.com/xberg-io/crawlberg/tree/main/packages/python) for full documentation.

</details>

<details>
<summary><strong>Node.js</strong></summary>

```sh
npm install @xberg-io/crawlberg
```

See [Node.js README](https://github.com/xberg-io/crawlberg/tree/main/crates/crawlberg-node) for full documentation.

</details>

<details>
<summary><strong>Rust</strong></summary>

```sh
cargo add crawlberg
```

See [Rust README](https://github.com/xberg-io/crawlberg/tree/main/crates/crawlberg) for full documentation.

</details>

<details>
<summary><strong>Go</strong></summary>

```sh
go get github.com/xberg-io/crawlberg/packages/go
```

See [Go README](https://github.com/xberg-io/crawlberg/tree/main/packages/go) for full documentation.

</details>

<details>
<summary><strong>Java</strong></summary>

Available on Maven Central as `io.xberg.crawlberg:crawlberg`. See [Java README](https://github.com/xberg-io/crawlberg/tree/main/packages/java) for the dependency snippet and current version.

</details>

<details>
<summary><strong>C#</strong></summary>

```sh
dotnet add package Crawlberg
```

See [C# README](https://github.com/xberg-io/crawlberg/tree/main/packages/csharp) for full documentation.

</details>

<details>
<summary><strong>Ruby</strong></summary>

```sh
gem install crawlberg
```

See [Ruby README](https://github.com/xberg-io/crawlberg/tree/main/packages/ruby) for full documentation.

</details>

<details>
<summary><strong>PHP</strong></summary>

```sh
composer require xberg-io/crawlberg
```

See [PHP README](https://github.com/xberg-io/crawlberg/tree/main/packages/php) for full documentation.

</details>

<details>
<summary><strong>Elixir</strong></summary>

Add `{:crawlberg, "~> 0.3"}` to your `mix.exs` dependencies. See [Elixir README](https://github.com/xberg-io/crawlberg/tree/main/packages/elixir) for full documentation.

</details>

<details>
<summary><strong>Dart / Flutter</strong></summary>

```sh
dart pub add crawlberg
```

See [Dart README](https://github.com/xberg-io/crawlberg/tree/main/packages/dart) for full documentation.

</details>

<details>
<summary><strong>Kotlin (Android)</strong></summary>

Available on Maven Central as `io.xberg.crawlberg.android:crawlberg-android`. See [Kotlin README](https://github.com/xberg-io/crawlberg/tree/main/packages/kotlin-android) for the dependency snippet and current version.

</details>

<details>
<summary><strong>Swift</strong></summary>

Add via Swift Package Manager. See [Swift README](https://github.com/xberg-io/crawlberg/tree/main/packages/swift) for full documentation.

</details>

<details>
<summary><strong>Zig</strong></summary>

See [Zig README](https://github.com/xberg-io/crawlberg/tree/main/packages/zig) for installation and usage.

</details>

<details>
<summary><strong>WebAssembly</strong></summary>

```sh
npm install @xberg-io/crawlberg-wasm
```

See [WebAssembly README](https://github.com/xberg-io/crawlberg/tree/main/crates/crawlberg-wasm) for full documentation.

</details>

<details>
<summary><strong>C/C++ (FFI)</strong></summary>

C header + shared library from [GitHub Releases](https://github.com/xberg-io/crawlberg/releases). See [FFI crate](https://github.com/xberg-io/crawlberg/tree/main/crates/crawlberg-ffi) for full documentation.

</details>

<details>
<summary><strong>CLI</strong></summary>

```sh
cargo install crawlberg-cli
```

```sh
brew install xberg-io/tap/crawlberg
```

See [CLI README](https://github.com/xberg-io/crawlberg/tree/main/crates/crawlberg-cli) for full documentation.

</details>

### AI Coding Assistants

Install the Crawlberg plugin from the [`xberg-io/plugins`](https://github.com/xberg-io/plugins) marketplace. It ships the Crawlberg agent skills (site crawling, HTML→Markdown scraping, headless-Chrome fallback) plus the `crawlberg` MCP server, and works with every major coding agent — expand your harness below.

<details open>
<summary><strong>Claude Code</strong></summary>

```text
/plugin marketplace add xberg-io/plugins
/plugin install crawlberg@xberg
```

</details>

<details>
<summary><strong>Codex CLI</strong></summary>

```text
/plugins add https://github.com/xberg-io/plugins
```

Then search for `crawlberg` and select **Install Plugin**.

</details>

<details>
<summary><strong>Cursor</strong></summary>

Settings → Plugins → Add from URL → `https://github.com/xberg-io/plugins`, then select **crawlberg**.

</details>

<details>
<summary><strong>Gemini CLI</strong></summary>

```text
gemini extensions install https://github.com/xberg-io/plugins
```

</details>

<details>
<summary><strong>Factory Droid</strong></summary>

```text
droid plugin marketplace add https://github.com/xberg-io/plugins
droid plugin install crawlberg@xberg
```

</details>

<details>
<summary><strong>GitHub Copilot CLI</strong></summary>

```text
copilot plugin marketplace add https://github.com/xberg-io/plugins
copilot plugin install crawlberg@xberg
```

</details>

<details>
<summary><strong>opencode</strong></summary>

Add the package to `opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["@xberg-io/opencode-crawlberg"]
}
```

</details>

## Documentation

Full guides, per-language API references, the substrate/operational model, antibot strategy, and observability live at **[docs.crawlberg.xberg.io](https://docs.crawlberg.xberg.io)**.

## Contributing

Contributions are welcome! See our [Contributing Guide](https://github.com/xberg-io/crawlberg/blob/main/CONTRIBUTING.md).

## Part of Xberg.dev

- [Xberg](https://github.com/xberg-io/xberg) — document intelligence: text, tables, metadata from 91+ formats with optional OCR.
- [Xberg Enterprise](https://github.com/xberg-io/xberg-enterprise) — managed extraction API with SDKs, dashboards, and observability.
- [crawlberg](https://github.com/xberg-io/crawlberg) — web crawling and scraping with HTML→Markdown and headless-Chrome fallback.
- [html-to-markdown](https://github.com/xberg-io/html-to-markdown) — fast, lossless HTML→Markdown engine.
- [liter-llm](https://github.com/xberg-io/liter-llm) — universal LLM API client with native bindings for 14 languages and 143 providers.
- [tree-sitter-language-pack](https://github.com/xberg-io/tree-sitter-language-pack) — tree-sitter grammars and code-intelligence primitives.
- [alef](https://github.com/xberg-io/alef) — the polyglot binding generator that produces every per-language binding across the 5 polyglot repos.

## License

[MIT License](https://github.com/xberg-io/crawlberg/blob/main/LICENSE)

## Links

- [Documentation](https://docs.crawlberg.xberg.io)
- [GitHub Repository](https://github.com/xberg-io/crawlberg)
- [Issue Tracker](https://github.com/xberg-io/crawlberg/issues)
- [Changelog](CHANGELOG.md)
- [Discord](https://discord.gg/xt9WY3GnKR) — community, roadmap, announcements.
