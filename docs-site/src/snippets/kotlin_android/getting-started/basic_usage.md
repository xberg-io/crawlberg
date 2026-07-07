```kotlin title="Kotlin (Android)"
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.PropertyNamingStrategies
import com.fasterxml.jackson.module.kotlin.registerKotlinModule
import io.xberg.crawlberg.android.CrawlConfig
import io.xberg.crawlberg.android.Crawlberg
import kotlinx.coroutines.runBlocking

fun main() = runBlocking {
    // Simplest case: scrape a single page with default settings.
    val engine = Crawlberg.createEngine()
    val result = Crawlberg.scrapeAsync(engine, "https://example.com/")
    println("Title: ${result.metadata.title}")
    println("Status: ${result.statusCode}")
    println("Links found: ${result.links.size}")

    // Crawl from a seed URL, limited to one hop and a handful of pages.
    val mapper = ObjectMapper()
        .registerKotlinModule()
        .setPropertyNamingStrategy(PropertyNamingStrategies.SNAKE_CASE)
    val config = mapper.readValue(
        "{\"max_depth\":1,\"max_pages\":5}",
        CrawlConfig::class.java,
    )
    val crawlEngine = Crawlberg.createEngine(config)
    val crawlResult = Crawlberg.crawlAsync(
        crawlEngine,
        "https://en.wikipedia.org/wiki/Web_scraping",
    )
    println("Pages crawled: ${crawlResult.pages.size}")
}
```
