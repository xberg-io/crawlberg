# crawlberg Crystal — scrape a real website
require "../src/crawlberg"

# Minimal config — only the fields we want to override. Thanks to the
# type-based `@[JSON::Field(default: ...)]` added to the Crystal backend,
# bool/int/array/hash/String fields no longer need to be spelled out.
puts "Creating engine..."
config = Crawlberg::CrawlConfig.from_json(%({
  "content":{"output_format":"markdown"},
  "browser":{"mode":"Auto","backend":"Chromiumoxide","wait":"NetworkIdle"},
  "ssrf":{}
}))

engine = Crawlberg.create_engine(config)

url = "https://httpbin.org/html"
puts "Scraping #{url}..."
result = Crawlberg.scrape(engine, url)
puts "  Status: #{result.status_code}"
puts "  Title: #{result.metadata.title}"
puts "  Content: #{result.html[0..199]}"
puts "Crystal crawlberg bindings working!"
