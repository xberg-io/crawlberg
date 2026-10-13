"""Hand-written native constructor calls from v1.10.3, checked against the current stub.

Keep these calls unchanged when regenerating bindings; update them for a new released baseline.
This file is type-checked, not imported: it needs no native extension or running browser.
"""

from crawlberg import _crawlberg as native


def released_action_result() -> native.ActionResult:
    return native.ActionResult(0, "click", True, None, None)


def released_article_metadata() -> native.ArticleMetadata:
    return native.ArticleMetadata([], None, None, None, None)


def released_batch_crawl_result() -> native.BatchCrawlResult:
    return native.BatchCrawlResult("https://example.test", None, None)


def released_batch_crawl_results() -> native.BatchCrawlResults:
    return native.BatchCrawlResults([], 0, 0, 0)


def released_batch_crawl_stream_request() -> native.BatchCrawlStreamRequest:
    return native.BatchCrawlStreamRequest([])


def released_batch_scrape_result() -> native.BatchScrapeResult:
    return native.BatchScrapeResult("https://example.test", None, None)


def released_batch_scrape_results() -> native.BatchScrapeResults:
    return native.BatchScrapeResults([], 0, 0, 0)


def released_browser_config() -> native.BrowserConfig:
    return native.BrowserConfig(
        native.BrowserMode(0),
        native.BrowserBackend(0),
        None,
        None,
        None,
        native.BrowserWait(0),
        [],
        False,
        False,
        [],
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )


def released_browser_extras() -> native.BrowserExtras:
    return native.BrowserExtras([], [], None)


def released_citation_reference() -> native.CitationReference:
    return native.CitationReference(0, "https://example.test", "reference")


def released_citation_result() -> native.CitationResult:
    return native.CitationResult("content", [])


def released_content_config() -> native.ContentConfig:
    return native.ContentConfig(
        "markdown", "standard", True, True, [], [], ["noscript"], False, False, 80, True, True, None
    )


def released_cookie_info() -> native.CookieInfo:
    return native.CookieInfo("session", "value", None, None)


def released_crawl_config() -> native.CrawlConfig:
    return native.CrawlConfig()


def released_crawl_page_result() -> native.CrawlPageResult:
    return native.CrawlPageResult(
        "https://example.test",
        "https://example.test",
        200,
        "text/html",
        "",
        0,
        None,
        [],
        [],
        [],
        [],
        0,
        True,
        False,
        False,
        False,
        "https://example.test",
        0,
        False,
        False,
        [],
        None,
        None,
        None,
        None,
        None,
    )


def released_crawl_result() -> native.CrawlResult:
    return native.CrawlResult([], "https://example.test", 0, False, [], True, False, None)


def released_crawl_stream_request() -> native.CrawlStreamRequest:
    return native.CrawlStreamRequest("https://example.test")


def released_downloaded_asset() -> native.DownloadedAsset:
    return native.DownloadedAsset("https://example.test", "hash", 0, native.AssetCategory(0), None, None)


def released_downloaded_document() -> native.DownloadedDocument:
    return native.DownloadedDocument("https://example.test", "text/plain", 0, "hash", {}, False, None, None, None)


def released_extraction_meta() -> native.ExtractionMeta:
    return native.ExtractionMeta(0, None, None, None, None)


def released_favicon_info() -> native.FaviconInfo:
    return native.FaviconInfo("https://example.test", "icon", None, None)


def released_feed_info() -> native.FeedInfo:
    return native.FeedInfo("https://example.test", native.FeedType(0), None)


def released_heading_info() -> native.HeadingInfo:
    return native.HeadingInfo(1, "heading")


def released_hreflang_entry() -> native.HreflangEntry:
    return native.HreflangEntry("en", "https://example.test")


def released_image_info() -> native.ImageInfo:
    return native.ImageInfo("https://example.test", native.ImageSource(0), None, None, None)


def released_interaction_result() -> native.InteractionResult:
    return native.InteractionResult([], "", "https://example.test", [], None)


def released_json_ld_entry() -> native.JsonLdEntry:
    return native.JsonLdEntry("Article", "{}", None)


def released_link_info() -> native.LinkInfo:
    return native.LinkInfo("https://example.test", "link", native.LinkType(0), False, None)


def released_map_result() -> native.MapResult:
    return native.MapResult([])


def released_markdown_result() -> native.MarkdownResult:
    return native.MarkdownResult("content", [], [], False, None, None)


def released_page_metadata() -> native.PageMetadata:
    return native.PageMetadata()


def released_proxy_config() -> native.ProxyConfig:
    return native.ProxyConfig("http://proxy.example.test", None, None)


def released_response_meta() -> native.ResponseMeta:
    return native.ResponseMeta(None, None, None, None, None, None, None)


def released_scrape_result() -> native.ScrapeResult:
    return native.ScrapeResult()


def released_sitemap_url() -> native.SitemapUrl:
    return native.SitemapUrl("https://example.test", None, None, None)


def released_ssrf_policy() -> native.SsrfPolicy:
    return native.SsrfPolicy(True, [], [], 5, ["http", "https"])


def released_asset_category() -> native.AssetCategory:
    return native.AssetCategory(0)


def released_auth_config() -> native.AuthConfig:
    return native.AuthConfig()


def released_browser_backend() -> native.BrowserBackend:
    return native.BrowserBackend(0)


def released_browser_mode() -> native.BrowserMode:
    return native.BrowserMode(0)


def released_browser_wait() -> native.BrowserWait:
    return native.BrowserWait(0)


def released_content_filter_kind() -> native.ContentFilterKind:
    return native.ContentFilterKind(0)


def released_crawl_event() -> native.CrawlEvent:
    return native.CrawlEvent()


def released_crawl_strategy_kind() -> native.CrawlStrategyKind:
    return native.CrawlStrategyKind(0)


def released_document_content_encoding() -> native.DocumentContentEncoding:
    return native.DocumentContentEncoding(0)


def released_feed_type() -> native.FeedType:
    return native.FeedType(0)


def released_host_matcher() -> native.HostMatcher:
    return native.HostMatcher()


def released_image_source() -> native.ImageSource:
    return native.ImageSource(0)


def released_link_type() -> native.LinkType:
    return native.LinkType(0)


def released_page_action() -> native.PageAction:
    return native.PageAction()


def released_scroll_direction() -> native.ScrollDirection:
    return native.ScrollDirection(0)
