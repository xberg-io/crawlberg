---
title: Streaming Crawls
---

## Streaming Crawls

Use streaming crawls for memory-bounded operations on large sites. Instead of accumulating all pages into a result buffer, a streaming crawl yields `CrawlEvent`s as pages are processed and discards them immediately, keeping peak memory usage constant regardless of crawl size.

### When to stream vs. collect

Use `crawl()` and `batch_crawl()` when you need the full result set at once — they return a collected `CrawlResult` or `BatchCrawlResults`. Use `crawl_stream()` and `batch_crawl_stream()` when crawling large sites, processing pages incrementally, or running on memory-constrained systems. A typical large crawl using `crawl()` consumes ≈2.5 GB peak memory; the same crawl via `crawl_stream()` stays below ≈20 MB.

Streaming trades result shape for memory efficiency: you receive `CrawlEvent`s one at a time instead of a single result object containing all pages.

### Rust streaming

Open a stream and iterate over events:

```rust
use futures::StreamExt;
use crawlberg::{CrawlConfig, CrawlEvent, batch_crawl_stream, create_engine, crawl_stream};

let engine = create_engine(Some(CrawlConfig {
    max_depth: Some(2),
    max_pages: Some(1000),
    ..Default::default()
}))?;

let mut stream = crawl_stream(&engine, "https://example.com").await?;
while let Some(event) = stream.next().await {
    match event? {
        CrawlEvent::Page { result } => {
            println!("{} (depth {}): {} bytes", result.url, result.depth, result.body_size);
            // Page is dropped here; memory is freed.
        }
        CrawlEvent::Error { url, error } => {
            eprintln!("Failed to crawl {url}: {error}");
        }
        CrawlEvent::Complete { pages_crawled } => {
            println!("Crawl completed. {pages_crawled} pages crawled.");
        }
    }
}
```

`crawl_stream` resolves to a stream of `Result<CrawlEvent, CrawlError>`; the `?` unwraps each item.
`CrawlEvent` has three struct variants:

- `Page { result }` — a successfully crawled page (`result` is a boxed `CrawlPageResult` with the same fields as in collected crawls).
- `Error { url, error }` — a failed page fetch with the URL and error message.
- `Complete { pages_crawled }` — terminal event signaling the crawl finished. Always emitted, even on seed-level errors.

Use `batch_crawl_stream()` for multiple seed URLs:

```rust
let mut stream = batch_crawl_stream(&engine, vec![
    "https://example.com".to_owned(),
    "https://other-site.org".to_owned(),
]).await?;

while let Some(event) = stream.next().await {
    match event? {
        CrawlEvent::Page { result } => { /* ... */ }
        CrawlEvent::Error { url, error } => { /* ... */ }
        CrawlEvent::Complete { pages_crawled } => { /* ... */ }
    }
}
```

### Python streaming

Python async iterators handle streaming naturally:

Each event carries a `type` discriminant (`"page"`, `"error"`, or `"complete"`) and exposes the matching
payload as a dict via the `page`, `error`, and `complete` getters (the others return `None`):

```python
import asyncio
from crawlberg import create_engine, crawl_stream, CrawlConfig

async def main():
    engine = create_engine(CrawlConfig(
        max_depth=2,
        max_pages=1000,
    ))

    async for event in crawl_stream(engine, "https://example.com"):
        if event.type == "page":
            page = event.page["result"]
            print(f"{page['url']} (depth {page['depth']}): {page['body_size']} bytes")
        elif event.type == "error":
            err = event.error
            print(f"Failed to crawl {err['url']}: {err['error']}")
        elif event.type == "complete":
            print(f"Crawl completed. {event.complete['pages_crawled']} pages crawled.")

asyncio.run(main())
```

For batch streaming, use `batch_crawl_stream()`:

```python
async for event in batch_crawl_stream(engine, [
    "https://example.com",
    "https://other-site.org",
]):
    # Same event.type dispatch as above
    ...
```

### Streaming in other languages

Streaming entry points (`crawl_stream`, `batch_crawl_stream`) are exposed in all language bindings. Each binding uses that language's native async-iterator or generator protocol:

- **Rust**: async stream via `futures::Stream`
- **Python**: async iterator via `AsyncIterator[CrawlEvent]`
- **TypeScript/Node.js**, **Go**, **Ruby**, **PHP**, **Java**, **C#**, **Elixir**, **Dart**, **Swift**, **Zig**: language-native streaming/async iteration patterns

Refer to the per-language API reference under **Reference** for exact method signatures and examples. Note that streaming completeness and memory efficiency vary by binding implementation — Rust and Python are fully streaming and memory-efficient; check your target language's reference for specific semantics.
