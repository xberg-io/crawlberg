```elixir title="Elixir"
# Simplest case: scrape a single page with default settings.
{:ok, engine} = Crawlberg.create_engine()
{:ok, scrape_json} = Crawlberg.scrape_async(engine, "https://example.com/")
scrape = Jason.decode!(scrape_json)
IO.puts("Title: #{scrape["metadata"]["title"]}")
IO.puts("Status: #{scrape["status_code"]}")
IO.puts("Links found: #{length(scrape["links"] || [])}")

# Crawl from a seed URL, limited to one hop and a handful of pages.
config_json = Jason.encode!(%Crawlberg.CrawlConfig{max_depth: 1, max_pages: 5})
{:ok, crawl_engine} = Crawlberg.create_engine(config_json)
{:ok, crawl_json} =
  Crawlberg.crawl_async(crawl_engine, "https://en.wikipedia.org/wiki/Web_scraping")
crawl = Jason.decode!(crawl_json)
IO.puts("Pages crawled: #{length(crawl["pages"] || [])}")
```
