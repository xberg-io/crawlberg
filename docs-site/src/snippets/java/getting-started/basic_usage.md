```java title="Java"
import java.util.Optional;

import io.xberg.crawlberg.CrawlConfig;
import io.xberg.crawlberg.CrawlEngineHandle;
import io.xberg.crawlberg.CrawlResult;
import io.xberg.crawlberg.Crawlberg;
import io.xberg.crawlberg.ScrapeResult;

public final class BasicUsage {
    private BasicUsage() { }

    public static void main(final String[] args) throws Exception {
        // Simplest case: scrape a single page with default settings.
        CrawlEngineHandle engine = Crawlberg.createEngine();
        ScrapeResult result = Crawlberg.scrape(engine, "https://example.com/");
        System.out.println("Title: " + result.metadata().title());
        System.out.println("Status: " + result.statusCode());
        System.out.println("Links found: " + result.links().size());

        // Crawl from a seed URL, limited to one hop and a handful of pages.
        CrawlConfig config = CrawlConfig.builder()
            .withMaxDepth(Optional.of(1L))
            .withMaxPages(Optional.of(5L))
            .build();
        CrawlEngineHandle crawlEngine = Crawlberg.createEngine(config);
        CrawlResult crawlResult = Crawlberg.crawl(
            crawlEngine,
            "https://en.wikipedia.org/wiki/Web_scraping"
        );
        System.out.println("Pages crawled: " + crawlResult.pages().size());
    }
}
```
