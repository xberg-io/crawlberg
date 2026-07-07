```csharp title="C#"
using System;
using System.Threading.Tasks;

using Crawlberg;

internal static class BasicUsage
{
    public static async Task Main()
    {
        // Simplest case: scrape a single page with default settings.
        var engine = CrawlbergLib.CreateEngine(null);
        var result = await CrawlbergLib.Scrape(engine, "https://example.com/");
        Console.WriteLine($"Title: {result.Metadata.Title}");
        Console.WriteLine($"Status: {result.StatusCode}");
        Console.WriteLine($"Links found: {result.Links.Count}");

        // Crawl from a seed URL, limited to one hop and a handful of pages.
        var config = new CrawlConfig
        {
            MaxDepth = 1,
            MaxPages = 5,
        };
        var crawlEngine = CrawlbergLib.CreateEngine(config);
        var crawlResult = await CrawlbergLib.Crawl(
            crawlEngine,
            "https://en.wikipedia.org/wiki/Web_scraping"
        );
        Console.WriteLine($"Pages crawled: {crawlResult.Pages.Count}");
    }
}
```
