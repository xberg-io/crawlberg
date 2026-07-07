```python title="Python"
import asyncio

from crawlberg import CrawlConfig, create_engine, crawl, scrape


async def main() -> None:
    # Simplest case: scrape a single page with default settings.
    engine = create_engine()
    result = await scrape(engine, "https://example.com/")
    print(f"Title: {result.metadata.title}")
    print(f"Status: {result.status_code}")
    print(f"Links found: {len(result.links)}")

    # Crawl from a seed URL, limited to one hop and a handful of pages.
    crawl_engine = create_engine(CrawlConfig(max_depth=1, max_pages=5))
    crawl_result = await crawl(crawl_engine, "https://en.wikipedia.org/wiki/Web_scraping")
    print(f"Pages crawled: {len(crawl_result.pages)}")


if __name__ == "__main__":
    asyncio.run(main())
```
