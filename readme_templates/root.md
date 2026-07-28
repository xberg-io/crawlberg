<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://cdn.jsdelivr.net/gh/xberg-io/assets@v1/banner/readme-banner-dark.svg">
    <img alt="Xberg" width="420" src="https://cdn.jsdelivr.net/gh/xberg-io/assets@v1/banner/readme-banner-light.svg">
  </picture>
</p>

# Crawlberg

{% include 'partials/_badges.md' %}

Turn any website into clean, structured data. Point Crawlberg at a URL and get back Markdown, metadata, and links — from a single page or a whole site — in the language you already use.

## What and Why?

You need data that lives on the web, and raw HTML is not it. Crawlberg does the crawling, scraping, and cleanup end-to-end: it fetches pages, follows links, converts each one to Markdown, and hands you structured metadata (titles, links, images, social-card and JSON-LD data) — so you skip the parsing and go straight to the content.

It runs from a single Rust core with identical results across 14 language bindings, and it handles the awkward parts for you: JavaScript-heavy pages fall back to a real headless browser, bot filters are detected and worked around, robots and sitemaps are respected, requests are throttled per domain, and requests to private or internal addresses are refused by default. Drive it from your code, an AI agent, a REST service, or the CLI.

Every part of the pipeline is a trait you can swap — the crawl frontier, rate limiter, storage, event stream, and content filters — so you can plug in your own behavior. Managed extras like proxy pools, tuned bot-evasion, authenticated sessions, scheduling, and billing live in [xberg-enterprise](https://github.com/xberg-io/xberg-enterprise).

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

Install the Crawlberg plugin from [`xberg-io/crawlberg`](https://github.com/xberg-io/crawlberg). It ships the Crawlberg agent skills (site crawling, HTML→Markdown scraping, headless-Chrome fallback) plus the `crawlberg` MCP server, and works with every major coding agent — expand your harness below.

<details open>
<summary><strong>Claude Code</strong></summary>

```text
/plugin marketplace add xberg-io/crawlberg
/plugin install crawlberg@crawlberg
```

</details>

<details>
<summary><strong>Codex CLI</strong></summary>

```text
/plugins add https://github.com/xberg-io/crawlberg
```

Then search for `crawlberg` and select **Install Plugin**.

</details>

<details>
<summary><strong>Cursor</strong></summary>

Settings → Plugins → Add from URL → `https://github.com/xberg-io/crawlberg`, then select **crawlberg**.

</details>

<details>
<summary><strong>Gemini CLI</strong></summary>

```text
gemini extensions install https://github.com/xberg-io/crawlberg
```

</details>

<details>
<summary><strong>Factory Droid</strong></summary>

```text
droid plugin marketplace add https://github.com/xberg-io/crawlberg
droid plugin install crawlberg@crawlberg
```

</details>

<details>
<summary><strong>GitHub Copilot CLI</strong></summary>

```text
copilot plugin marketplace add https://github.com/xberg-io/crawlberg
copilot plugin install crawlberg@crawlberg
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

- [Xberg](https://github.com/xberg-io/xberg) — document intelligence: text, tables, metadata from 98+ formats with optional OCR.
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
