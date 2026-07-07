```ruby title="Ruby"
require "crawlberg"

# Simplest case: scrape a single page with default settings.
engine = Crawlberg.create_engine
result = Crawlberg.scrape(engine, "https://example.com/")
puts "Title: #{result.metadata.title}"
puts "Status: #{result.status_code}"
puts "Links found: #{result.links.length}"

# Crawl from a seed URL, limited to one hop and a handful of pages.
config = Crawlberg::CrawlConfig.new(max_depth: 1, max_pages: 5)
crawl_engine = Crawlberg.create_engine(config)
crawl_result = Crawlberg.crawl(crawl_engine, "https://en.wikipedia.org/wiki/Web_scraping")
puts "Pages crawled: #{crawl_result.pages.length}"
```
