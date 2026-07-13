```crystal title="Crystal"
require "crawlberg"

# Simplest case: scrape a single page with default settings.
config = Crawlberg::CrawlConfig.from_json(%({
  "content":{"output_format":"markdown"},
  "browser":{"mode":"Auto","backend":"Chromiumoxide","wait":"NetworkIdle"},
  "ssrf":{}
}))
engine = Crawlberg.create_engine(config)
result = Crawlberg.scrape(engine, "https://example.com/")
puts "Title: #{result.metadata.title}"
puts "Status: #{result.status_code}"
puts "Links found: #{result.links.size}"

# Crawl from a seed URL, limited to one hop.
crawl_config = Crawlberg::CrawlConfig.from_json(%({
  "max_depth":1,"max_pages":5,
  "content":{"output_format":"markdown"},
  "browser":{"mode":"Auto","backend":"Chromiumoxide","wait":"NetworkIdle"},
  "ssrf":{}
}))
crawl_engine = Crawlberg.create_engine(crawl_config)
crawl_result = Crawlberg.crawl(crawl_engine, "https://en.wikipedia.org/wiki/Web_scraping")
puts "Pages crawled: #{crawl_result.pages.size}"
```
