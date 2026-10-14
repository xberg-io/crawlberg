"""Hand-written native constructor calls from v1.10.3, checked against the current stub.

Keep these released call shapes unchanged when regenerating bindings; update them for a new released baseline.
Type checking preserves call shapes; runtime assertions verify their field values.
"""

from crawlberg import _crawlberg as native


def released_action_result() -> native.ActionResult:
    return native.ActionResult(1, "action_type-sentinel", True, "data-optional", "error-optional")


def released_article_metadata() -> native.ArticleMetadata:
    return native.ArticleMetadata(
        [], "published_time-optional", "modified_time-optional", "author-optional", "section-optional"
    )


def released_batch_crawl_result() -> native.BatchCrawlResult:
    return native.BatchCrawlResult("url-sentinel", None, "error-optional")


def released_batch_crawl_results() -> native.BatchCrawlResults:
    return native.BatchCrawlResults([], 2, 3, 4)


def released_batch_crawl_stream_request() -> native.BatchCrawlStreamRequest:
    return native.BatchCrawlStreamRequest([])


def released_batch_scrape_result() -> native.BatchScrapeResult:
    return native.BatchScrapeResult("url-sentinel", None, "error-optional")


def released_batch_scrape_results() -> native.BatchScrapeResults:
    return native.BatchScrapeResults([], 2, 3, 4)


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
        "endpoint-optional",
        "wait_selector-optional",
        None,
        None,
        "eval_script-optional",
        "robots_user_agent-optional",
        "chrome_path-optional",
    )


def released_browser_extras() -> native.BrowserExtras:
    return native.BrowserExtras([], [], "eval_result-optional")


def released_citation_reference() -> native.CitationReference:
    return native.CitationReference(1, "url-sentinel", "text-sentinel")


def released_citation_result() -> native.CitationResult:
    return native.CitationResult("content-sentinel", [])


def released_content_config() -> native.ContentConfig:
    return native.ContentConfig(
        "output_format-sentinel",
        "preprocessing_preset-sentinel",
        True,
        False,
        [],
        [],
        ["noscript"],
        False,
        False,
        10,
        True,
        True,
        None,
    )


def released_cookie_info() -> native.CookieInfo:
    return native.CookieInfo("session", "value", None, None)


def released_crawl_config() -> native.CrawlConfig:
    return native.CrawlConfig(max_depth=7, max_pages=19, respect_robots_txt=False)


def released_crawl_page_result() -> native.CrawlPageResult:
    return native.CrawlPageResult(
        "url-sentinel",
        "normalized_url-sentinel",
        207,
        "content_type-sentinel",
        "html-sentinel",
        6,
        None,
        [],
        [],
        [],
        [],
        12,
        True,
        False,
        False,
        False,
        "final_url-sentinel",
        18,
        False,
        False,
        [],
        "detected_charset-optional",
        None,
        "extracted_data-optional",
        None,
        None,
    )


def released_crawl_result() -> native.CrawlResult:
    return native.CrawlResult([], "final_url-sentinel", 3, False, [], True, False, "error-optional")


def released_crawl_stream_request() -> native.CrawlStreamRequest:
    return native.CrawlStreamRequest("url-sentinel")


def released_downloaded_asset() -> native.DownloadedAsset:
    return native.DownloadedAsset(
        "url-sentinel", "content_hash-sentinel", 3, native.AssetCategory(0), "mime_type-optional", "html_tag-optional"
    )


def released_downloaded_document() -> native.DownloadedDocument:
    return native.DownloadedDocument(
        "url-sentinel",
        "mime_type-sentinel",
        3,
        "content_hash-sentinel",
        {},
        False,
        "filename-optional",
        "content_path-optional",
        "content_base64-optional",
    )


def released_extraction_meta() -> native.ExtractionMeta:
    return native.ExtractionMeta(1, None, None, None, "model-optional")


def released_favicon_info() -> native.FaviconInfo:
    return native.FaviconInfo("url-sentinel", "rel-sentinel", "sizes-optional", "mime_type-optional")


def released_feed_info() -> native.FeedInfo:
    return native.FeedInfo("url-sentinel", native.FeedType(0), "title-optional")


def released_heading_info() -> native.HeadingInfo:
    return native.HeadingInfo(1, "text-sentinel")


def released_hreflang_entry() -> native.HreflangEntry:
    return native.HreflangEntry("lang-sentinel", "url-sentinel")


def released_image_info() -> native.ImageInfo:
    return native.ImageInfo("url-sentinel", native.ImageSource(0), "alt-optional", None, None)


def released_interaction_result() -> native.InteractionResult:
    return native.InteractionResult([], "final_html-sentinel", "final_url-sentinel", [], "screenshot_base64-optional")


def released_json_ld_entry() -> native.JsonLdEntry:
    return native.JsonLdEntry("schema_type-sentinel", "raw-sentinel", "name-optional")


def released_link_info() -> native.LinkInfo:
    return native.LinkInfo("url-sentinel", "text-sentinel", native.LinkType(0), False, "rel-optional")


def released_map_result() -> native.MapResult:
    return native.MapResult([])


def released_markdown_result() -> native.MarkdownResult:
    return native.MarkdownResult(
        "content-sentinel", [], [], False, "document_structure-optional", "fit_content-optional"
    )


def released_page_metadata() -> native.PageMetadata:
    return native.PageMetadata(title="title-sentinel", description="description-sentinel")


def released_proxy_config() -> native.ProxyConfig:
    return native.ProxyConfig("url-sentinel", "username-optional", "password-optional")


def released_response_meta() -> native.ResponseMeta:
    return native.ResponseMeta(
        "etag-optional",
        "last_modified-optional",
        "cache_control-optional",
        "server-optional",
        "x_powered_by-optional",
        "content_language-optional",
        "content_encoding-optional",
    )


def released_scrape_result() -> native.ScrapeResult:
    return native.ScrapeResult(status_code=207, final_url="https://final.example.test", html="<p>sentinel</p>")


def released_sitemap_url() -> native.SitemapUrl:
    return native.SitemapUrl("url-sentinel", "lastmod-optional", "changefreq-optional", "priority-optional")


def released_ssrf_policy() -> native.SsrfPolicy:
    return native.SsrfPolicy(True, [], [], 4, ["http", "https"])


def released_asset_category() -> native.AssetCategory:
    return native.AssetCategory(0)


def released_auth_config() -> native.AuthConfig:
    return native.AuthConfig({"type": "basic", "username": "user-sentinel", "password": "password-sentinel"})


def released_browser_backend() -> native.BrowserBackend:
    return native.BrowserBackend(0)


def released_browser_mode() -> native.BrowserMode:
    return native.BrowserMode(0)


def released_browser_wait() -> native.BrowserWait:
    return native.BrowserWait(0)


def released_content_filter_kind() -> native.ContentFilterKind:
    return native.ContentFilterKind(0)


def released_crawl_event() -> native.CrawlEvent:
    return native.CrawlEvent({"type": "complete", "pages_crawled": 17})


def released_crawl_strategy_kind() -> native.CrawlStrategyKind:
    return native.CrawlStrategyKind(0)


def released_document_content_encoding() -> native.DocumentContentEncoding:
    return native.DocumentContentEncoding(0)


def released_feed_type() -> native.FeedType:
    return native.FeedType(0)


def released_host_matcher() -> native.HostMatcher:
    return native.HostMatcher({"type": "exact", "value": "host.example.test"})


def released_image_source() -> native.ImageSource:
    return native.ImageSource(0)


def released_link_type() -> native.LinkType:
    return native.LinkType(0)


def released_page_action() -> native.PageAction:
    return native.PageAction({"type": "click", "selector": "#sentinel"})


def released_scroll_direction() -> native.ScrollDirection:
    return native.ScrollDirection(0)


def test_should_preserve_released_cookie_info_values() -> None:
    value = released_cookie_info()
    assert value.name == "session"
    assert value.value == "value"
    assert value.domain is None
    assert value.path is None


def test_should_preserve_released_action_result_values() -> None:
    value = released_action_result()
    assert value.action_index == 1
    assert value.action_type == "action_type-sentinel"
    assert value.success is True
    assert value.data == "data-optional"
    assert value.error == "error-optional"


def test_should_preserve_released_article_metadata_values() -> None:
    value = released_article_metadata()
    assert value.tags == []
    assert value.published_time == "published_time-optional"
    assert value.modified_time == "modified_time-optional"
    assert value.author == "author-optional"
    assert value.section == "section-optional"


def test_should_preserve_released_batch_crawl_result_values() -> None:
    value = released_batch_crawl_result()
    assert value.url == "url-sentinel"
    assert value.result is None
    assert value.error == "error-optional"


def test_should_preserve_released_batch_crawl_results_values() -> None:
    value = released_batch_crawl_results()
    assert value.results == []
    assert value.total_count == 2
    assert value.completed_count == 3
    assert value.failed_count == 4


def test_should_preserve_released_batch_crawl_stream_request_values() -> None:
    value = released_batch_crawl_stream_request()
    assert value.urls == []


def test_should_preserve_released_batch_scrape_result_values() -> None:
    value = released_batch_scrape_result()
    assert value.url == "url-sentinel"
    assert value.result is None
    assert value.error == "error-optional"


def test_should_preserve_released_batch_scrape_results_values() -> None:
    value = released_batch_scrape_results()
    assert value.results == []
    assert value.total_count == 2
    assert value.completed_count == 3
    assert value.failed_count == 4


def test_should_preserve_released_browser_config_values() -> None:
    value = released_browser_config()
    assert value.mode == native.BrowserMode(0)
    assert value.backend == native.BrowserBackend(0)
    assert value.timeout is None
    assert value.overall_timeout is None
    assert value.shutdown_timeout is None
    assert value.wait == native.BrowserWait(0)
    assert value.block_url_patterns == []
    assert value.capture_network_events is False
    assert value.session_affinity is False
    assert value.chrome_args == []
    assert value.endpoint == "endpoint-optional"
    assert value.wait_selector == "wait_selector-optional"
    assert value.extra_wait is None
    assert value.proxy is None
    assert value.eval_script == "eval_script-optional"
    assert value.robots_user_agent == "robots_user_agent-optional"
    assert value.chrome_path == "chrome_path-optional"


def test_should_preserve_released_browser_extras_values() -> None:
    value = released_browser_extras()
    assert value.network_events == []
    assert value.cookies == []
    assert value.eval_result == "eval_result-optional"


def test_should_preserve_released_citation_reference_values() -> None:
    value = released_citation_reference()
    assert value.index == 1
    assert value.url == "url-sentinel"
    assert value.text == "text-sentinel"


def test_should_preserve_released_citation_result_values() -> None:
    value = released_citation_result()
    assert value.content == "content-sentinel"
    assert value.references == []


def test_should_preserve_released_content_config_values() -> None:
    value = released_content_config()
    assert value.output_format == "output_format-sentinel"
    assert value.preprocessing_preset == "preprocessing_preset-sentinel"
    assert value.remove_navigation is True
    assert value.remove_forms is False
    assert value.strip_tags == []
    assert value.preserve_tags == []
    assert value.exclude_selectors == ["noscript"]
    assert value.skip_images is False
    assert value.wrap is False
    assert value.wrap_width == 10
    assert value.include_document_structure is True
    assert value.extract_metadata is True
    assert value.max_depth is None


def test_should_preserve_released_crawl_config_values() -> None:
    value = released_crawl_config()
    assert value.max_depth == 7
    assert value.max_pages == 19
    assert value.respect_robots_txt is False


def test_should_preserve_released_crawl_page_result_values() -> None:
    value = released_crawl_page_result()
    assert value.url == "url-sentinel"
    assert value.normalized_url == "normalized_url-sentinel"
    assert value.status_code == 207
    assert value.content_type == "content_type-sentinel"
    assert value.html == "html-sentinel"
    assert value.body_size == 6
    assert value.links == []
    assert value.images == []
    assert value.feeds == []
    assert value.json_ld == []
    assert value.depth == 12
    assert value.stayed_on_domain is True
    assert value.was_skipped is False
    assert value.is_pdf is False
    assert value.browser_used is False
    assert value.final_url == "final_url-sentinel"
    assert value.redirect_count == 18
    assert value.noindex_detected is False
    assert value.nofollow_detected is False
    assert value.ssrf_refused_urls == []
    assert value.detected_charset == "detected_charset-optional"
    assert value.markdown is None
    assert value.extracted_data == "extracted_data-optional"
    assert value.extraction_meta is None
    assert value.downloaded_document is None


def test_should_preserve_released_crawl_result_values() -> None:
    value = released_crawl_result()
    assert value.pages == []
    assert value.final_url == "final_url-sentinel"
    assert value.redirect_count == 3
    assert value.was_skipped is False
    assert value.cookies == []
    assert value.stayed_on_domain is True
    assert value.browser_used is False
    assert value.error == "error-optional"


def test_should_preserve_released_crawl_stream_request_values() -> None:
    value = released_crawl_stream_request()
    assert value.url == "url-sentinel"


def test_should_preserve_released_downloaded_asset_values() -> None:
    value = released_downloaded_asset()
    assert value.url == "url-sentinel"
    assert value.content_hash == "content_hash-sentinel"
    assert value.size == 3
    assert value.asset_category == native.AssetCategory(0)
    assert value.mime_type == "mime_type-optional"
    assert value.html_tag == "html_tag-optional"


def test_should_preserve_released_downloaded_document_values() -> None:
    value = released_downloaded_document()
    assert value.url == "url-sentinel"
    assert value.mime_type == "mime_type-sentinel"
    assert value.size == 3
    assert value.content_hash == "content_hash-sentinel"
    assert value.headers == {}
    assert value.truncated is False
    assert value.filename == "filename-optional"
    assert value.content_path == "content_path-optional"
    assert value.content_base64 == "content_base64-optional"


def test_should_preserve_released_extraction_meta_values() -> None:
    value = released_extraction_meta()
    assert value.chunks_processed == 1
    assert value.cost is None
    assert value.prompt_tokens is None
    assert value.completion_tokens is None
    assert value.model == "model-optional"


def test_should_preserve_released_favicon_info_values() -> None:
    value = released_favicon_info()
    assert value.url == "url-sentinel"
    assert value.rel == "rel-sentinel"
    assert value.sizes == "sizes-optional"
    assert value.mime_type == "mime_type-optional"


def test_should_preserve_released_feed_info_values() -> None:
    value = released_feed_info()
    assert value.url == "url-sentinel"
    assert value.feed_type == native.FeedType(0)
    assert value.title == "title-optional"


def test_should_preserve_released_heading_info_values() -> None:
    value = released_heading_info()
    assert value.level == 1
    assert value.text == "text-sentinel"


def test_should_preserve_released_hreflang_entry_values() -> None:
    value = released_hreflang_entry()
    assert value.lang == "lang-sentinel"
    assert value.url == "url-sentinel"


def test_should_preserve_released_image_info_values() -> None:
    value = released_image_info()
    assert value.url == "url-sentinel"
    assert value.source == native.ImageSource(0)
    assert value.alt == "alt-optional"
    assert value.width is None
    assert value.height is None


def test_should_preserve_released_interaction_result_values() -> None:
    value = released_interaction_result()
    assert value.action_results == []
    assert value.final_html == "final_html-sentinel"
    assert value.final_url == "final_url-sentinel"
    assert value.ssrf_refused_urls == []
    assert value.screenshot_base64 == "screenshot_base64-optional"


def test_should_preserve_released_json_ld_entry_values() -> None:
    value = released_json_ld_entry()
    assert value.schema_type == "schema_type-sentinel"
    assert value.raw == "raw-sentinel"
    assert value.name == "name-optional"


def test_should_preserve_released_link_info_values() -> None:
    value = released_link_info()
    assert value.url == "url-sentinel"
    assert value.text == "text-sentinel"
    assert value.link_type == native.LinkType(0)
    assert value.nofollow is False
    assert value.rel == "rel-optional"


def test_should_preserve_released_map_result_values() -> None:
    value = released_map_result()
    assert value.urls == []


def test_should_preserve_released_markdown_result_values() -> None:
    value = released_markdown_result()
    assert value.content == "content-sentinel"
    assert value.tables == []
    assert value.warnings == []
    assert value.citations is False
    assert value.document_structure == "document_structure-optional"
    assert value.fit_content == "fit_content-optional"


def test_should_preserve_released_page_metadata_values() -> None:
    value = released_page_metadata()
    assert value.title == "title-sentinel"
    assert value.description == "description-sentinel"


def test_should_preserve_released_proxy_config_values() -> None:
    value = released_proxy_config()
    assert value.url == "url-sentinel"
    assert value.username == "username-optional"
    assert value.password == "password-optional"


def test_should_preserve_released_response_meta_values() -> None:
    value = released_response_meta()
    assert value.etag == "etag-optional"
    assert value.last_modified == "last_modified-optional"
    assert value.cache_control == "cache_control-optional"
    assert value.server == "server-optional"
    assert value.x_powered_by == "x_powered_by-optional"
    assert value.content_language == "content_language-optional"
    assert value.content_encoding == "content_encoding-optional"


def test_should_preserve_released_scrape_result_values() -> None:
    value = released_scrape_result()
    assert value.status_code == 207
    assert value.final_url == "https://final.example.test"
    assert value.html == "<p>sentinel</p>"


def test_should_preserve_released_sitemap_url_values() -> None:
    value = released_sitemap_url()
    assert value.url == "url-sentinel"
    assert value.lastmod == "lastmod-optional"
    assert value.changefreq == "changefreq-optional"
    assert value.priority == "priority-optional"


def test_should_preserve_released_ssrf_policy_values() -> None:
    value = released_ssrf_policy()
    assert value.deny_private is True
    assert value.allowlist == []
    assert value.denylist == []
    assert value.max_redirects == 4
    assert value.scheme_allowlist == ["http", "https"]


def test_should_preserve_released_auth_config_values() -> None:
    value = released_auth_config()
    assert value.type == "basic"


def test_should_preserve_released_crawl_event_values() -> None:
    value = released_crawl_event()
    assert value.type == "complete"
    assert str(value) == '{"pages_crawled":17,"type":"complete"}'


def test_should_preserve_released_host_matcher_values() -> None:
    value = released_host_matcher()
    assert value.type == "exact"
    assert str(value) == '{"type":"exact","value":"host.example.test"}'


def test_should_preserve_released_page_action_values() -> None:
    value = released_page_action()
    assert value.type == "click"


def test_should_preserve_released_enum_tags() -> None:
    assert released_asset_category() == native.AssetCategory.DOCUMENT
    assert released_browser_backend() == native.BrowserBackend.CHROMIUMOXIDE
    assert released_browser_mode() == native.BrowserMode.AUTO
    assert released_browser_wait() == native.BrowserWait.NETWORK_IDLE
    assert released_content_filter_kind() == native.ContentFilterKind.BM25
    assert released_crawl_strategy_kind() == native.CrawlStrategyKind.BFS
    assert released_document_content_encoding() == native.DocumentContentEncoding.BASE64
    assert released_feed_type() == native.FeedType.RSS
    assert released_image_source() == native.ImageSource.IMG
    assert released_link_type() == native.LinkType.INTERNAL
    assert released_scroll_direction() == native.ScrollDirection.UP


def test_should_preserve_released_content_factory_values() -> None:
    default = native.ContentConfig.default()
    assert default.output_format == "markdown"
    assert default.wrap_width == 80
    assert default.remove_navigation is True
    assert default.preprocessing_preset == "standard"
    assert default.remove_forms is True
    assert default.strip_tags == []
    assert default.preserve_tags == []
    assert default.exclude_selectors == ["noscript"]
    assert default.skip_images is False
    assert default.max_depth is None
    assert default.wrap is False
    assert default.include_document_structure is True
    assert default.extract_metadata is True
    value = native.ContentConfig.from_json(
        '{"output_format":"html","preprocessing_preset":"minimal","remove_navigation":false,'
        '"remove_forms":true,"wrap":true,"wrap_width":37,"exclude_selectors":[".ad"]}'
    )
    assert value.output_format == "html"
    assert value.preprocessing_preset == "minimal"
    assert value.remove_navigation is False
    assert value.remove_forms is True
    assert value.wrap is True
    assert value.wrap_width == 37
    assert value.exclude_selectors == [".ad"]
    try:
        native.ContentConfig.from_json("not-json")
    except ValueError:
        pass
    else:
        raise AssertionError("malformed factory input must raise ValueError")


def test_should_preserve_released_auth_factory_header_values() -> None:
    import asyncio
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from threading import Thread

    class HeaderEcho(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            body = (self.headers.get("Authorization", "") + "|" + self.headers.get("X-Sentinel", "")).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    async def check(url: str) -> None:
        cases = [
            (released_auth_config(), "Basic dXNlci1zZW50aW5lbDpwYXNzd29yZC1zZW50aW5lbA==|"),
            (native.AuthConfig.basic("user", "secret"), "Basic dXNlcjpzZWNyZXQ=|"),
            (native.AuthConfig.bearer("token-sentinel"), "Bearer token-sentinel|"),
            (native.AuthConfig.header("X-Sentinel", "value-sentinel"), "|value-sentinel"),
        ]
        for auth, expected in cases:
            engine = native.create_engine(native.CrawlConfig(auth=auth, ssrf=native.SsrfPolicy(False)))
            result = await native.scrape(engine, url)
            assert result.status_code == 200
            assert result.html == expected

    with ThreadingHTTPServer(("127.0.0.1", 0), HeaderEcho) as server:
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            asyncio.run(check(f"http://127.0.0.1:{server.server_port}/"))
        finally:
            server.shutdown()
            thread.join()
