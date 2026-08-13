```rust title="Rust"
use crawlberg::{crawl, create_engine, scrape, CrawlConfig, CrawlError};

#[tokio::main]
async fn main() -> Result<(), CrawlError> {
    // Simplest case: scrape a single page with default settings.
    let engine = create_engine(None)?;
    let result = scrape(&engine, "https://example.com/").await?;
    println!("Title: {}", result.metadata.title.as_deref().unwrap_or(""));
    println!("Status: {}", result.status_code);
    println!("Links found: {}", result.links.len());

    // Crawl from a seed URL, limited to one hop and a handful of pages.
    let config = CrawlConfig::builder().max_depth(1).max_pages(5).build();
    let crawl_engine = create_engine(Some(config))?;
    let crawl_result = crawl(&crawl_engine, "https://en.wikipedia.org/wiki/Web_scraping").await?;
    println!("Pages crawled: {}", crawl_result.pages.len());

    Ok(())
}
```
