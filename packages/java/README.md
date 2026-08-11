<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://cdn.jsdelivr.net/gh/xberg-io/assets@v1/banner/readme-banner-dark.svg">
    <img alt="Xberg" width="420" src="https://cdn.jsdelivr.net/gh/xberg-io/assets@v1/banner/readme-banner-light.svg">
  </picture>
</p>

# crawlberg

<div align="center" style="display: flex; flex-wrap: wrap; gap: 8px; justify-content: center; margin: 20px 0;">
  <a href="https://github.com/xberg-io/alef">
    <img src="https://img.shields.io/badge/built%20with-alef%20%D7%90-007ec6" alt="Built with alef">
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

Java bindings for **crawlberg** — a high-performance Rust web crawling engine. Uses the
Panama FFM API (Java 21+) for zero-overhead native calls with full type safety.

## What This Package Provides

- **Same crawler as every binding** — one Rust engine behind Python, Node.js, Ruby, Go, Java, .NET, PHP, Elixir, Dart, Kotlin Android, Swift, Zig, WASM, and C FFI.
- **Structured scrape output** — HTML, Markdown, metadata, links, assets, response headers, and extraction warnings with consistent field names.
- **Crawl controls** — depth, page limits, concurrency, URL filters, robots/sitemap handling, rate limits, and partial failure reporting.
- **Rendering path** — optional browser rendering for JavaScript-heavy pages; direct HTTP path for fast static pages.
- **Java package** — Panama FFM binding for direct native calls.

## Installation

```bash
<dependency>
    <groupId>io.xberg.crawlberg</groupId>
    <artifactId>crawlberg</artifactId>
    <version>1.2.0</version>
</dependency>

```

## Agent plugin

The `crawlberg` plugin is available via the `xberg-io/plugins` marketplace.

```text
/plugin marketplace add xberg-io/plugins
/plugin install crawlberg@xberg
```

Works with Claude Code, Codex, Cursor, Gemini CLI, Factory Droid, GitHub Copilot CLI, and opencode. See [the marketplace README](https://github.com/xberg-io/plugins) for harness-specific install instructions.

## Quick Start

```java title="Java"
import java.util.Optional;

import io.xberg.crawlberg.CrawlConfig;
import io.xberg.crawlberg.CrawlEngineHandle;
import io.xberg.crawlberg.CrawlResult;
import io.xberg.crawlberg.Crawlberg;
import io.xberg.crawlberg.ScrapeResult;

public final class BasicUsage {
    private BasicUsage() { }

    public static void main(final String[] args) throws Exception {
        // Simplest case: scrape a single page with default settings.
        CrawlEngineHandle engine = Crawlberg.createEngine();
        ScrapeResult result = Crawlberg.scrape(engine, "https://example.com/");
        System.out.println("Title: " + result.metadata().title());
        System.out.println("Status: " + result.statusCode());
        System.out.println("Links found: " + result.links().size());

        // Crawl from a seed URL, limited to one hop and a handful of pages.
        CrawlConfig config = CrawlConfig.builder()
            .withMaxDepth(Optional.of(1L))
            .withMaxPages(Optional.of(5L))
            .build();
        CrawlEngineHandle crawlEngine = Crawlberg.createEngine(config);
        CrawlResult crawlResult = Crawlberg.crawl(
            crawlEngine,
            "https://en.wikipedia.org/wiki/Web_scraping"
        );
        System.out.println("Pages crawled: " + crawlResult.pages().size());
    }
}
```

## API Reference

Full API documentation is available at [docs.crawlberg.xberg.io](https://docs.crawlberg.xberg.io).

Key functions:

- `create_engine(config?)` — Create a crawl engine with optional configuration
- `scrape(engine, url)` — Scrape a single URL
- `crawl(engine, url)` — Crawl a website following links
- `map_urls(engine, url)` — Discover all pages on a site
- `batch_scrape(engine, urls)` — Scrape multiple URLs concurrently
- `batch_crawl(engine, urls)` — Crawl multiple seed URLs concurrently

## Contributing

Contributions are welcome! Please see our [Contributing Guide](https://github.com/xberg-io/crawlberg/blob/main/CONTRIBUTING.md) for details.

## Part of Xberg.io

- [Xberg](https://github.com/xberg-io/xberg) — the open-source content-intelligence engine: text, tables, and metadata from 101 formats (115 file extensions), with OCR, transcription, and code intelligence. MIT.
- [Xberg Pro](https://xberg.io) — a complete self-hosted content-intelligence backend in a single container. Commercial.
- [Xberg Enterprise](https://xberg.io) — the distributed, governed content-intelligence platform, scaled on Kubernetes with team governance and support. Commercial.
- [crawlberg](https://github.com/xberg-io/crawlberg) — web crawling and scraping with HTML→Markdown and headless-Chrome fallback.
- [html-to-markdown](https://github.com/xberg-io/html-to-markdown) — fast, lossless HTML→Markdown engine.
- [liter-llm](https://github.com/xberg-io/liter-llm) — universal LLM API client with native bindings for 14 languages and 165 providers.
- [tree-sitter-language-pack](https://github.com/xberg-io/tree-sitter-language-pack) — tree-sitter grammars and code-intelligence primitives.
- [alef](https://github.com/xberg-io/alef) — the polyglot binding generator that produces every per-language binding across the 5 polyglot repos.
- [Discord](https://discord.gg/xt9WY3GnKR) — community, roadmap, announcements.

## License

This project is licensed under [MIT License](https://github.com/xberg-io/crawlberg/blob/main/LICENSE).

## Links

- [Documentation](https://docs.crawlberg.xberg.io)
- [GitHub Repository](https://github.com/xberg-io/crawlberg)
- [Issue Tracker](https://github.com/xberg-io/crawlberg/issues)
