```javascript title="WASM"
import { CrawlConfig, crawl, createEngine, scrape } from "@xberg-io/crawlberg-wasm";

async function main() {
  // Simplest case: scrape a single page with default settings.
  const engine = createEngine();
  const result = await scrape(engine, "https://example.com/");
  console.log(`Title: ${result.metadata?.title ?? ""}`);
  console.log(`Status: ${result.statusCode}`);
  console.log(`Links found: ${result.links?.length ?? 0}`);

  // Crawl from a seed URL, limited to one hop and a handful of pages.
  const config = new CrawlConfig();
  config.maxDepth = 1;
  config.maxPages = 5;
  const crawlEngine = createEngine(config);
  const crawlResult = await crawl(crawlEngine, "https://en.wikipedia.org/wiki/Web_scraping");
  console.log(`Pages crawled: ${crawlResult.pages?.length ?? 0}`);
}

main().catch((error) => console.error(error));
```
