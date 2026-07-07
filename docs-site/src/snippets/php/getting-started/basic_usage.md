```php title="PHP"
<?php
declare(strict_types=1);

use Crawlberg\CrawlConfig;
use Crawlberg\Crawlberg;

// Simplest case: scrape a single page with default settings.
$engine = Crawlberg::createEngine(null);
$result = Crawlberg::scrape($engine, "https://example.com/");
echo "Title: " . ($result->metadata->title ?? "") . "\n";
echo "Status: " . $result->statusCode . "\n";
echo "Links found: " . count($result->links) . "\n";

// Crawl from a seed URL, limited to one hop and a handful of pages.
$config = CrawlConfig::default();
$config->maxDepth = 1;
$config->maxPages = 5;
$crawlEngine = Crawlberg::createEngine($config);
$crawlResult = Crawlberg::crawl($crawlEngine, "https://en.wikipedia.org/wiki/Web_scraping");
echo "Pages crawled: " . count($crawlResult->pages) . "\n";
```
