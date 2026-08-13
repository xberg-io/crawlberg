```swift title="Swift"
import Foundation
import Crawlberg

@main
struct BasicUsage {
    static func main() async throws {
        // Simplest case: scrape a single page with default settings.
        let engine = try createEngine()
        let result = try await scrape(engine: engine, url: "https://example.com/")
        print("Title: \(result.metadata().title()?.toString() ?? "")")
        print("Status: \(result.statusCode())")
        print("Links found: \(result.links().count)")

        // Crawl from a seed URL, limited to one hop and a handful of pages.
        let crawlConfig = try crawlConfigFromJson("{\"max_depth\":1,\"max_pages\":5}")
        let crawlEngine = try createEngine(config: crawlConfig)
        let crawlResult = try await crawl(engine: crawlEngine, url: "https://en.wikipedia.org/wiki/Web_scraping")
        print("Pages crawled: \(crawlResult.pages().count)")
    }
}
```
