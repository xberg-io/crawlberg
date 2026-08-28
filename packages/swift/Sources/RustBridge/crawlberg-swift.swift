// swift-format-ignore-file
import RustBridgeC

public func crawlConfigValidateFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> RustString {
    try { let val = __swift_bridge__$crawl_config_validate_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlResultUniqueNormalizedUrlsFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> RustString {
    try { let val = __swift_bridge__$crawl_result_unique_normalized_urls_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawl_engine_handle_noop(_ client: CrawlEngineHandleRef) {
    __swift_bridge__$crawl_engine_handle_noop(client.ptr)
}
public func batchCrawl<GenericIntoRustString: IntoRustString>(_ engine: CrawlEngineHandle, _ urls: RustVec<GenericIntoRustString>) throws -> BatchCrawlResults {
    try { let val = __swift_bridge__$batch_crawl({engine.isOwned = false; return engine.ptr;}(), { let val = urls; val.isOwned = false; return val.ptr }()); if val.is_ok { return BatchCrawlResults(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func batchScrape<GenericIntoRustString: IntoRustString>(_ engine: CrawlEngineHandle, _ urls: RustVec<GenericIntoRustString>) throws -> BatchScrapeResults {
    try { let val = __swift_bridge__$batch_scrape({engine.isOwned = false; return engine.ptr;}(), { let val = urls; val.isOwned = false; return val.ptr }()); if val.is_ok { return BatchScrapeResults(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawl<GenericIntoRustString: IntoRustString>(_ engine: CrawlEngineHandle, _ url: GenericIntoRustString) throws -> CrawlResult {
    try { let val = __swift_bridge__$crawl({engine.isOwned = false; return engine.ptr;}(), { let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CrawlResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func createEngine(_ config: Optional<CrawlConfig>) throws -> CrawlEngineHandle {
    try { let val = __swift_bridge__$create_engine({ if let val = config { val.isOwned = false; return val.ptr } else { return nil } }()); if val.is_ok { return CrawlEngineHandle(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func generateCitations<GenericIntoRustString: IntoRustString>(_ markdown: GenericIntoRustString) -> CitationResult {
    CitationResult(ptr: __swift_bridge__$generate_citations({ let rustString = markdown.intoRustString(); rustString.isOwned = false; return rustString.ptr }()))
}
public func interact<GenericIntoRustString: IntoRustString>(_ engine: CrawlEngineHandle, _ url: GenericIntoRustString, _ actions: RustVec<GenericIntoRustString>) throws -> InteractionResult {
    try { let val = __swift_bridge__$interact({engine.isOwned = false; return engine.ptr;}(), { let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let val = actions; val.isOwned = false; return val.ptr }()); if val.is_ok { return InteractionResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func mapUrls<GenericIntoRustString: IntoRustString>(_ engine: CrawlEngineHandle, _ url: GenericIntoRustString) throws -> MapResult {
    try { let val = __swift_bridge__$map_urls({engine.isOwned = false; return engine.ptr;}(), { let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return MapResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func scrape<GenericIntoRustString: IntoRustString>(_ engine: CrawlEngineHandle, _ url: GenericIntoRustString) throws -> ScrapeResult {
    try { let val = __swift_bridge__$scrape({engine.isOwned = false; return engine.ptr;}(), { let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ScrapeResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlEngineHandleCrawlStreamStart(_ client: CrawlEngineHandleRef, _ req: CrawlStreamRequestRef) throws -> CrawlEngineHandleCrawlStreamStreamHandle {
    try { let val = __swift_bridge__$crawl_engine_handle_crawl_stream_start(client.ptr, req.ptr); if val.is_ok { return CrawlEngineHandleCrawlStreamStreamHandle(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlEngineHandleBatchCrawlStreamStart(_ client: CrawlEngineHandleRef, _ req: BatchCrawlStreamRequestRef) throws -> CrawlEngineHandleBatchCrawlStreamStreamHandle {
    try { let val = __swift_bridge__$crawl_engine_handle_batch_crawl_stream_start(client.ptr, req.ptr); if val.is_ok { return CrawlEngineHandleBatchCrawlStreamStreamHandle(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func actionResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ActionResult {
    try { let val = __swift_bridge__$action_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ActionResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func articleMetadataFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ArticleMetadata {
    try { let val = __swift_bridge__$article_metadata_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ArticleMetadata(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func batchCrawlResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BatchCrawlResult {
    try { let val = __swift_bridge__$batch_crawl_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BatchCrawlResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func batchCrawlResultsFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BatchCrawlResults {
    try { let val = __swift_bridge__$batch_crawl_results_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BatchCrawlResults(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func batchCrawlStreamRequestFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BatchCrawlStreamRequest {
    try { let val = __swift_bridge__$batch_crawl_stream_request_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BatchCrawlStreamRequest(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func batchScrapeResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BatchScrapeResult {
    try { let val = __swift_bridge__$batch_scrape_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BatchScrapeResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func batchScrapeResultsFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BatchScrapeResults {
    try { let val = __swift_bridge__$batch_scrape_results_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BatchScrapeResults(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func browserConfigFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BrowserConfig {
    try { let val = __swift_bridge__$browser_config_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BrowserConfig(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func browserExtrasFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BrowserExtras {
    try { let val = __swift_bridge__$browser_extras_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BrowserExtras(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func citationReferenceFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CitationReference {
    try { let val = __swift_bridge__$citation_reference_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CitationReference(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func citationResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CitationResult {
    try { let val = __swift_bridge__$citation_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CitationResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func contentConfigFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ContentConfig {
    try { let val = __swift_bridge__$content_config_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ContentConfig(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func cookieInfoFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CookieInfo {
    try { let val = __swift_bridge__$cookie_info_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CookieInfo(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlConfigFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CrawlConfig {
    try { let val = __swift_bridge__$crawl_config_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CrawlConfig(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlPageResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CrawlPageResult {
    try { let val = __swift_bridge__$crawl_page_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CrawlPageResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CrawlResult {
    try { let val = __swift_bridge__$crawl_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CrawlResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlStreamRequestFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CrawlStreamRequest {
    try { let val = __swift_bridge__$crawl_stream_request_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CrawlStreamRequest(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func downloadedAssetFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> DownloadedAsset {
    try { let val = __swift_bridge__$downloaded_asset_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return DownloadedAsset(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func downloadedDocumentFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> DownloadedDocument {
    try { let val = __swift_bridge__$downloaded_document_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return DownloadedDocument(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func extractionMetaFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ExtractionMeta {
    try { let val = __swift_bridge__$extraction_meta_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ExtractionMeta(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func faviconInfoFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> FaviconInfo {
    try { let val = __swift_bridge__$favicon_info_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return FaviconInfo(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func feedInfoFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> FeedInfo {
    try { let val = __swift_bridge__$feed_info_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return FeedInfo(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func headingInfoFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> HeadingInfo {
    try { let val = __swift_bridge__$heading_info_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return HeadingInfo(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func hreflangEntryFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> HreflangEntry {
    try { let val = __swift_bridge__$hreflang_entry_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return HreflangEntry(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func imageInfoFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ImageInfo {
    try { let val = __swift_bridge__$image_info_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ImageInfo(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func interactionResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> InteractionResult {
    try { let val = __swift_bridge__$interaction_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return InteractionResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func jsonLdEntryFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> JsonLdEntry {
    try { let val = __swift_bridge__$json_ld_entry_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return JsonLdEntry(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func linkInfoFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> LinkInfo {
    try { let val = __swift_bridge__$link_info_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return LinkInfo(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func mapResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> MapResult {
    try { let val = __swift_bridge__$map_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return MapResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func markdownResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> MarkdownResult {
    try { let val = __swift_bridge__$markdown_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return MarkdownResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func pageMetadataFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> PageMetadata {
    try { let val = __swift_bridge__$page_metadata_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return PageMetadata(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func proxyConfigFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ProxyConfig {
    try { let val = __swift_bridge__$proxy_config_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ProxyConfig(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func responseMetaFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ResponseMeta {
    try { let val = __swift_bridge__$response_meta_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ResponseMeta(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func scrapeResultFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ScrapeResult {
    try { let val = __swift_bridge__$scrape_result_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ScrapeResult(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func sitemapUrlFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> SitemapUrl {
    try { let val = __swift_bridge__$sitemap_url_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return SitemapUrl(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func ssrfPolicyFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> SsrfPolicy {
    try { let val = __swift_bridge__$ssrf_policy_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return SsrfPolicy(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func assetCategoryFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> AssetCategory {
    try { let val = __swift_bridge__$asset_category_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return AssetCategory(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func authConfigFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> AuthConfig {
    try { let val = __swift_bridge__$auth_config_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return AuthConfig(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func browserBackendFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BrowserBackend {
    try { let val = __swift_bridge__$browser_backend_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BrowserBackend(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func browserModeFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BrowserMode {
    try { let val = __swift_bridge__$browser_mode_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BrowserMode(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func browserWaitFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> BrowserWait {
    try { let val = __swift_bridge__$browser_wait_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return BrowserWait(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func contentFilterKindFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ContentFilterKind {
    try { let val = __swift_bridge__$content_filter_kind_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ContentFilterKind(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlEventFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CrawlEvent {
    try { let val = __swift_bridge__$crawl_event_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CrawlEvent(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func crawlStrategyKindFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> CrawlStrategyKind {
    try { let val = __swift_bridge__$crawl_strategy_kind_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return CrawlStrategyKind(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func documentContentEncodingFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> DocumentContentEncoding {
    try { let val = __swift_bridge__$document_content_encoding_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return DocumentContentEncoding(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func feedTypeFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> FeedType {
    try { let val = __swift_bridge__$feed_type_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return FeedType(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func hostMatcherFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> HostMatcher {
    try { let val = __swift_bridge__$host_matcher_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return HostMatcher(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func imageSourceFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ImageSource {
    try { let val = __swift_bridge__$image_source_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ImageSource(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func linkTypeFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> LinkType {
    try { let val = __swift_bridge__$link_type_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return LinkType(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func pageActionFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> PageAction {
    try { let val = __swift_bridge__$page_action_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return PageAction(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func scrollDirectionFromJson<GenericIntoRustString: IntoRustString>(_ json: GenericIntoRustString) throws -> ScrollDirection {
    try { let val = __swift_bridge__$scroll_direction_from_json({ let rustString = json.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return ScrollDirection(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func __alef_phantom_vec_action_result() -> RustVec<ActionResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_action_result())
}
public func __alef_phantom_vec_article_metadata() -> RustVec<ArticleMetadata> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_article_metadata())
}
public func __alef_phantom_vec_batch_crawl_result() -> RustVec<BatchCrawlResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_batch_crawl_result())
}
public func __alef_phantom_vec_batch_crawl_results() -> RustVec<BatchCrawlResults> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_batch_crawl_results())
}
public func __alef_phantom_vec_batch_scrape_result() -> RustVec<BatchScrapeResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_batch_scrape_result())
}
public func __alef_phantom_vec_batch_scrape_results() -> RustVec<BatchScrapeResults> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_batch_scrape_results())
}
public func __alef_phantom_vec_browser_config() -> RustVec<BrowserConfig> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_browser_config())
}
public func __alef_phantom_vec_browser_extras() -> RustVec<BrowserExtras> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_browser_extras())
}
public func __alef_phantom_vec_citation_reference() -> RustVec<CitationReference> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_citation_reference())
}
public func __alef_phantom_vec_citation_result() -> RustVec<CitationResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_citation_result())
}
public func __alef_phantom_vec_content_config() -> RustVec<ContentConfig> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_content_config())
}
public func __alef_phantom_vec_cookie_info() -> RustVec<CookieInfo> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_cookie_info())
}
public func __alef_phantom_vec_crawl_config() -> RustVec<CrawlConfig> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_crawl_config())
}
public func __alef_phantom_vec_crawl_page_result() -> RustVec<CrawlPageResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_crawl_page_result())
}
public func __alef_phantom_vec_crawl_result() -> RustVec<CrawlResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_crawl_result())
}
public func __alef_phantom_vec_downloaded_asset() -> RustVec<DownloadedAsset> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_downloaded_asset())
}
public func __alef_phantom_vec_downloaded_document() -> RustVec<DownloadedDocument> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_downloaded_document())
}
public func __alef_phantom_vec_extraction_meta() -> RustVec<ExtractionMeta> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_extraction_meta())
}
public func __alef_phantom_vec_favicon_info() -> RustVec<FaviconInfo> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_favicon_info())
}
public func __alef_phantom_vec_feed_info() -> RustVec<FeedInfo> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_feed_info())
}
public func __alef_phantom_vec_heading_info() -> RustVec<HeadingInfo> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_heading_info())
}
public func __alef_phantom_vec_hreflang_entry() -> RustVec<HreflangEntry> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_hreflang_entry())
}
public func __alef_phantom_vec_image_info() -> RustVec<ImageInfo> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_image_info())
}
public func __alef_phantom_vec_interaction_result() -> RustVec<InteractionResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_interaction_result())
}
public func __alef_phantom_vec_json_ld_entry() -> RustVec<JsonLdEntry> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_json_ld_entry())
}
public func __alef_phantom_vec_link_info() -> RustVec<LinkInfo> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_link_info())
}
public func __alef_phantom_vec_map_result() -> RustVec<MapResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_map_result())
}
public func __alef_phantom_vec_markdown_result() -> RustVec<MarkdownResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_markdown_result())
}
public func __alef_phantom_vec_page_metadata() -> RustVec<PageMetadata> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_page_metadata())
}
public func __alef_phantom_vec_proxy_config() -> RustVec<ProxyConfig> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_proxy_config())
}
public func __alef_phantom_vec_response_meta() -> RustVec<ResponseMeta> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_response_meta())
}
public func __alef_phantom_vec_scrape_result() -> RustVec<ScrapeResult> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_scrape_result())
}
public func __alef_phantom_vec_sitemap_url() -> RustVec<SitemapUrl> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_sitemap_url())
}
public func __alef_phantom_vec_ssrf_policy() -> RustVec<SsrfPolicy> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_ssrf_policy())
}
public func __alef_phantom_vec_asset_category() -> RustVec<AssetCategory> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_asset_category())
}
public func __alef_phantom_vec_auth_config() -> RustVec<AuthConfig> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_auth_config())
}
public func __alef_phantom_vec_browser_backend() -> RustVec<BrowserBackend> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_browser_backend())
}
public func __alef_phantom_vec_browser_mode() -> RustVec<BrowserMode> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_browser_mode())
}
public func __alef_phantom_vec_browser_wait() -> RustVec<BrowserWait> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_browser_wait())
}
public func __alef_phantom_vec_content_filter_kind() -> RustVec<ContentFilterKind> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_content_filter_kind())
}
public func __alef_phantom_vec_crawl_strategy_kind() -> RustVec<CrawlStrategyKind> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_crawl_strategy_kind())
}
public func __alef_phantom_vec_document_content_encoding() -> RustVec<DocumentContentEncoding> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_document_content_encoding())
}
public func __alef_phantom_vec_feed_type() -> RustVec<FeedType> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_feed_type())
}
public func __alef_phantom_vec_host_matcher() -> RustVec<HostMatcher> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_host_matcher())
}
public func __alef_phantom_vec_image_source() -> RustVec<ImageSource> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_image_source())
}
public func __alef_phantom_vec_link_type() -> RustVec<LinkType> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_link_type())
}
public func __alef_phantom_vec_page_action() -> RustVec<PageAction> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_page_action())
}
public func __alef_phantom_vec_scroll_direction() -> RustVec<ScrollDirection> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_scroll_direction())
}
public func __alef_phantom_vec_batch_crawl_stream_request() -> RustVec<BatchCrawlStreamRequest> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_batch_crawl_stream_request())
}
public func __alef_phantom_vec_crawl_stream_request() -> RustVec<CrawlStreamRequest> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_crawl_stream_request())
}
public func __alef_phantom_vec_crawl_event() -> RustVec<CrawlEvent> {
    RustVec(ptr: __swift_bridge__$__alef_phantom_vec_crawl_event())
}

public class ActionResult: ActionResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ActionResult$_free(ptr)
        }
    }
}
extension ActionResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ action_index: UInt, _ action_type: GenericIntoRustString, _ success: Bool, _ data: Optional<GenericIntoRustString>, _ error: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$ActionResult$new(action_index, { let rustString = action_type.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), success, { if let rustString = optionalStringIntoRustString(data) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(error) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class ActionResultRefMut: ActionResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ActionResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ActionResultRef {
    public func actionIndex() -> UInt {
        __swift_bridge__$ActionResult$action_index(ptr)
    }

    public func actionType() -> RustString {
        RustString(ptr: __swift_bridge__$ActionResult$action_type(ptr))
    }

    public func success() -> Bool {
        __swift_bridge__$ActionResult$success(ptr)
    }

    public func data() -> Optional<RustString> {
        { let val = __swift_bridge__$ActionResult$data(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func error() -> Optional<RustString> {
        { let val = __swift_bridge__$ActionResult$error(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension ActionResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ActionResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ActionResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ActionResult) {
        __swift_bridge__$Vec_ActionResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ActionResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ActionResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ActionResultRef> {
        let pointer = __swift_bridge__$Vec_ActionResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ActionResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ActionResultRefMut> {
        let pointer = __swift_bridge__$Vec_ActionResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ActionResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ActionResultRef> {
        UnsafePointer<ActionResultRef>(OpaquePointer(__swift_bridge__$Vec_ActionResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ActionResult$len(vecPtr)
    }
}


public class ArticleMetadata: ArticleMetadataRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ArticleMetadata$_free(ptr)
        }
    }
}
extension ArticleMetadata {
    public convenience init<GenericIntoRustString: IntoRustString>(_ published_time: Optional<GenericIntoRustString>, _ modified_time: Optional<GenericIntoRustString>, _ author: Optional<GenericIntoRustString>, _ section: Optional<GenericIntoRustString>, _ tags: RustVec<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$ArticleMetadata$new({ if let rustString = optionalStringIntoRustString(published_time) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(modified_time) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(author) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(section) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { let val = tags; val.isOwned = false; return val.ptr }()))
    }
}
public class ArticleMetadataRefMut: ArticleMetadataRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ArticleMetadataRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ArticleMetadataRef {
    public func publishedTime() -> Optional<RustString> {
        { let val = __swift_bridge__$ArticleMetadata$published_time(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func modifiedTime() -> Optional<RustString> {
        { let val = __swift_bridge__$ArticleMetadata$modified_time(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func author() -> Optional<RustString> {
        { let val = __swift_bridge__$ArticleMetadata$author(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func section() -> Optional<RustString> {
        { let val = __swift_bridge__$ArticleMetadata$section(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func tags() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$ArticleMetadata$tags(ptr))
    }
}
extension ArticleMetadata: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ArticleMetadata$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ArticleMetadata$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ArticleMetadata) {
        __swift_bridge__$Vec_ArticleMetadata$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ArticleMetadata$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ArticleMetadata(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ArticleMetadataRef> {
        let pointer = __swift_bridge__$Vec_ArticleMetadata$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ArticleMetadataRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ArticleMetadataRefMut> {
        let pointer = __swift_bridge__$Vec_ArticleMetadata$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ArticleMetadataRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ArticleMetadataRef> {
        UnsafePointer<ArticleMetadataRef>(OpaquePointer(__swift_bridge__$Vec_ArticleMetadata$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ArticleMetadata$len(vecPtr)
    }
}


public class BatchCrawlResult: BatchCrawlResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BatchCrawlResult$_free(ptr)
        }
    }
}
extension BatchCrawlResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ result: Optional<CrawlResult>, _ error: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$BatchCrawlResult$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let val = result { val.isOwned = false; return val.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(error) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class BatchCrawlResultRefMut: BatchCrawlResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BatchCrawlResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BatchCrawlResultRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$BatchCrawlResult$url(ptr))
    }

    public func result() -> Optional<CrawlResult> {
        { let val = __swift_bridge__$BatchCrawlResult$result(ptr); if val != nil { return CrawlResult(ptr: val!) } else { return nil } }()
    }

    public func error() -> Optional<RustString> {
        { let val = __swift_bridge__$BatchCrawlResult$error(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension BatchCrawlResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BatchCrawlResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BatchCrawlResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BatchCrawlResult) {
        __swift_bridge__$Vec_BatchCrawlResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BatchCrawlResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BatchCrawlResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchCrawlResultRef> {
        let pointer = __swift_bridge__$Vec_BatchCrawlResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchCrawlResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchCrawlResultRefMut> {
        let pointer = __swift_bridge__$Vec_BatchCrawlResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchCrawlResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BatchCrawlResultRef> {
        UnsafePointer<BatchCrawlResultRef>(OpaquePointer(__swift_bridge__$Vec_BatchCrawlResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BatchCrawlResult$len(vecPtr)
    }
}


public class BatchCrawlResults: BatchCrawlResultsRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BatchCrawlResults$_free(ptr)
        }
    }
}
extension BatchCrawlResults {
    public convenience init(_ results: RustVec<BatchCrawlResult>, _ total_count: UInt, _ completed_count: UInt, _ failed_count: UInt) {
        self.init(ptr: __swift_bridge__$BatchCrawlResults$new({ let val = results; val.isOwned = false; return val.ptr }(), total_count, completed_count, failed_count))
    }
}
public class BatchCrawlResultsRefMut: BatchCrawlResultsRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BatchCrawlResultsRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BatchCrawlResultsRef {
    public func results() -> RustVec<BatchCrawlResult> {
        RustVec(ptr: __swift_bridge__$BatchCrawlResults$results(ptr))
    }

    public func totalCount() -> UInt {
        __swift_bridge__$BatchCrawlResults$total_count(ptr)
    }

    public func completedCount() -> UInt {
        __swift_bridge__$BatchCrawlResults$completed_count(ptr)
    }

    public func failedCount() -> UInt {
        __swift_bridge__$BatchCrawlResults$failed_count(ptr)
    }
}
extension BatchCrawlResults: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BatchCrawlResults$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BatchCrawlResults$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BatchCrawlResults) {
        __swift_bridge__$Vec_BatchCrawlResults$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BatchCrawlResults$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BatchCrawlResults(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchCrawlResultsRef> {
        let pointer = __swift_bridge__$Vec_BatchCrawlResults$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchCrawlResultsRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchCrawlResultsRefMut> {
        let pointer = __swift_bridge__$Vec_BatchCrawlResults$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchCrawlResultsRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BatchCrawlResultsRef> {
        UnsafePointer<BatchCrawlResultsRef>(OpaquePointer(__swift_bridge__$Vec_BatchCrawlResults$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BatchCrawlResults$len(vecPtr)
    }
}


public class BatchCrawlStreamRequest: BatchCrawlStreamRequestRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BatchCrawlStreamRequest$_free(ptr)
        }
    }
}
extension BatchCrawlStreamRequest {
    public convenience init<GenericIntoRustString: IntoRustString>(_ urls: RustVec<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$BatchCrawlStreamRequest$new({ let val = urls; val.isOwned = false; return val.ptr }()))
    }
}
public class BatchCrawlStreamRequestRefMut: BatchCrawlStreamRequestRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BatchCrawlStreamRequestRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BatchCrawlStreamRequestRef {
    public func urls() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$BatchCrawlStreamRequest$urls(ptr))
    }
}
extension BatchCrawlStreamRequest: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BatchCrawlStreamRequest$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BatchCrawlStreamRequest$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BatchCrawlStreamRequest) {
        __swift_bridge__$Vec_BatchCrawlStreamRequest$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BatchCrawlStreamRequest$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BatchCrawlStreamRequest(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchCrawlStreamRequestRef> {
        let pointer = __swift_bridge__$Vec_BatchCrawlStreamRequest$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchCrawlStreamRequestRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchCrawlStreamRequestRefMut> {
        let pointer = __swift_bridge__$Vec_BatchCrawlStreamRequest$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchCrawlStreamRequestRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BatchCrawlStreamRequestRef> {
        UnsafePointer<BatchCrawlStreamRequestRef>(OpaquePointer(__swift_bridge__$Vec_BatchCrawlStreamRequest$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BatchCrawlStreamRequest$len(vecPtr)
    }
}


public class BatchScrapeResult: BatchScrapeResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BatchScrapeResult$_free(ptr)
        }
    }
}
extension BatchScrapeResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ result: Optional<ScrapeResult>, _ error: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$BatchScrapeResult$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let val = result { val.isOwned = false; return val.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(error) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class BatchScrapeResultRefMut: BatchScrapeResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BatchScrapeResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BatchScrapeResultRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$BatchScrapeResult$url(ptr))
    }

    public func result() -> Optional<ScrapeResult> {
        { let val = __swift_bridge__$BatchScrapeResult$result(ptr); if val != nil { return ScrapeResult(ptr: val!) } else { return nil } }()
    }

    public func error() -> Optional<RustString> {
        { let val = __swift_bridge__$BatchScrapeResult$error(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension BatchScrapeResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BatchScrapeResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BatchScrapeResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BatchScrapeResult) {
        __swift_bridge__$Vec_BatchScrapeResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BatchScrapeResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BatchScrapeResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchScrapeResultRef> {
        let pointer = __swift_bridge__$Vec_BatchScrapeResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchScrapeResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchScrapeResultRefMut> {
        let pointer = __swift_bridge__$Vec_BatchScrapeResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchScrapeResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BatchScrapeResultRef> {
        UnsafePointer<BatchScrapeResultRef>(OpaquePointer(__swift_bridge__$Vec_BatchScrapeResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BatchScrapeResult$len(vecPtr)
    }
}


public class BatchScrapeResults: BatchScrapeResultsRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BatchScrapeResults$_free(ptr)
        }
    }
}
extension BatchScrapeResults {
    public convenience init(_ results: RustVec<BatchScrapeResult>, _ total_count: UInt, _ completed_count: UInt, _ failed_count: UInt) {
        self.init(ptr: __swift_bridge__$BatchScrapeResults$new({ let val = results; val.isOwned = false; return val.ptr }(), total_count, completed_count, failed_count))
    }
}
public class BatchScrapeResultsRefMut: BatchScrapeResultsRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BatchScrapeResultsRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BatchScrapeResultsRef {
    public func results() -> RustVec<BatchScrapeResult> {
        RustVec(ptr: __swift_bridge__$BatchScrapeResults$results(ptr))
    }

    public func totalCount() -> UInt {
        __swift_bridge__$BatchScrapeResults$total_count(ptr)
    }

    public func completedCount() -> UInt {
        __swift_bridge__$BatchScrapeResults$completed_count(ptr)
    }

    public func failedCount() -> UInt {
        __swift_bridge__$BatchScrapeResults$failed_count(ptr)
    }
}
extension BatchScrapeResults: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BatchScrapeResults$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BatchScrapeResults$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BatchScrapeResults) {
        __swift_bridge__$Vec_BatchScrapeResults$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BatchScrapeResults$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BatchScrapeResults(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchScrapeResultsRef> {
        let pointer = __swift_bridge__$Vec_BatchScrapeResults$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchScrapeResultsRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BatchScrapeResultsRefMut> {
        let pointer = __swift_bridge__$Vec_BatchScrapeResults$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BatchScrapeResultsRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BatchScrapeResultsRef> {
        UnsafePointer<BatchScrapeResultsRef>(OpaquePointer(__swift_bridge__$Vec_BatchScrapeResults$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BatchScrapeResults$len(vecPtr)
    }
}


public class BrowserConfig: BrowserConfigRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BrowserConfig$_free(ptr)
        }
    }
}
extension BrowserConfig {
    public convenience init<GenericIntoRustString: IntoRustString>(_ mode: BrowserMode, _ backend: BrowserBackend, _ endpoint: Optional<GenericIntoRustString>, _ timeout: UInt64, _ wait: BrowserWait, _ wait_selector: Optional<GenericIntoRustString>, _ extra_wait: Optional<UInt64>, _ proxy: Optional<ProxyConfig>, _ block_url_patterns: RustVec<GenericIntoRustString>, _ eval_script: Optional<GenericIntoRustString>, _ robots_user_agent: Optional<GenericIntoRustString>, _ capture_network_events: Bool, _ session_affinity: Bool) {
        self.init(ptr: __swift_bridge__$BrowserConfig$new({mode.isOwned = false; return mode.ptr;}(), {backend.isOwned = false; return backend.ptr;}(), { if let rustString = optionalStringIntoRustString(endpoint) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), timeout, {wait.isOwned = false; return wait.ptr;}(), { if let rustString = optionalStringIntoRustString(wait_selector) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), extra_wait.intoFfiRepr(), { if let val = proxy { val.isOwned = false; return val.ptr } else { return nil } }(), { let val = block_url_patterns; val.isOwned = false; return val.ptr }(), { if let rustString = optionalStringIntoRustString(eval_script) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(robots_user_agent) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), capture_network_events, session_affinity))
    }
}
public class BrowserConfigRefMut: BrowserConfigRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BrowserConfigRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BrowserConfigRef {
    public func mode() -> RustString {
        RustString(ptr: __swift_bridge__$BrowserConfig$mode(ptr))
    }

    public func backend() -> RustString {
        RustString(ptr: __swift_bridge__$BrowserConfig$backend(ptr))
    }

    public func endpoint() -> Optional<RustString> {
        { let val = __swift_bridge__$BrowserConfig$endpoint(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func timeout() -> UInt64 {
        __swift_bridge__$BrowserConfig$timeout(ptr)
    }

    public func wait() -> RustString {
        RustString(ptr: __swift_bridge__$BrowserConfig$wait(ptr))
    }

    public func waitSelector() -> Optional<RustString> {
        { let val = __swift_bridge__$BrowserConfig$wait_selector(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func extraWait() -> Optional<UInt64> {
        __swift_bridge__$BrowserConfig$extra_wait(ptr).intoSwiftRepr()
    }

    public func proxy() -> Optional<ProxyConfig> {
        { let val = __swift_bridge__$BrowserConfig$proxy(ptr); if val != nil { return ProxyConfig(ptr: val!) } else { return nil } }()
    }

    public func blockUrlPatterns() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$BrowserConfig$block_url_patterns(ptr))
    }

    public func evalScript() -> Optional<RustString> {
        { let val = __swift_bridge__$BrowserConfig$eval_script(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func robotsUserAgent() -> Optional<RustString> {
        { let val = __swift_bridge__$BrowserConfig$robots_user_agent(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func captureNetworkEvents() -> Bool {
        __swift_bridge__$BrowserConfig$capture_network_events(ptr)
    }

    public func sessionAffinity() -> Bool {
        __swift_bridge__$BrowserConfig$session_affinity(ptr)
    }
}
extension BrowserConfig: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BrowserConfig$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BrowserConfig$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BrowserConfig) {
        __swift_bridge__$Vec_BrowserConfig$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BrowserConfig$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BrowserConfig(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserConfigRef> {
        let pointer = __swift_bridge__$Vec_BrowserConfig$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserConfigRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserConfigRefMut> {
        let pointer = __swift_bridge__$Vec_BrowserConfig$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserConfigRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BrowserConfigRef> {
        UnsafePointer<BrowserConfigRef>(OpaquePointer(__swift_bridge__$Vec_BrowserConfig$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BrowserConfig$len(vecPtr)
    }
}


public class BrowserExtras: BrowserExtrasRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BrowserExtras$_free(ptr)
        }
    }
}
extension BrowserExtras {
    public convenience init<GenericIntoRustString: IntoRustString>(_ eval_result: Optional<GenericIntoRustString>, _ network_events: RustVec<ResponseMeta>, _ cookies: RustVec<CookieInfo>) {
        self.init(ptr: __swift_bridge__$BrowserExtras$new({ if let rustString = optionalStringIntoRustString(eval_result) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { let val = network_events; val.isOwned = false; return val.ptr }(), { let val = cookies; val.isOwned = false; return val.ptr }()))
    }
}
public class BrowserExtrasRefMut: BrowserExtrasRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BrowserExtrasRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BrowserExtrasRef {
    public func evalResult() -> Optional<RustString> {
        { let val = __swift_bridge__$BrowserExtras$eval_result(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func networkEvents() -> RustVec<ResponseMeta> {
        RustVec(ptr: __swift_bridge__$BrowserExtras$network_events(ptr))
    }

    public func cookies() -> RustVec<CookieInfo> {
        RustVec(ptr: __swift_bridge__$BrowserExtras$cookies(ptr))
    }
}
extension BrowserExtras: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BrowserExtras$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BrowserExtras$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BrowserExtras) {
        __swift_bridge__$Vec_BrowserExtras$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BrowserExtras$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BrowserExtras(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserExtrasRef> {
        let pointer = __swift_bridge__$Vec_BrowserExtras$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserExtrasRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserExtrasRefMut> {
        let pointer = __swift_bridge__$Vec_BrowserExtras$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserExtrasRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BrowserExtrasRef> {
        UnsafePointer<BrowserExtrasRef>(OpaquePointer(__swift_bridge__$Vec_BrowserExtras$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BrowserExtras$len(vecPtr)
    }
}


public class CitationReference: CitationReferenceRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CitationReference$_free(ptr)
        }
    }
}
extension CitationReference {
    public convenience init<GenericIntoRustString: IntoRustString>(_ index: UInt, _ url: GenericIntoRustString, _ text: GenericIntoRustString) {
        self.init(ptr: __swift_bridge__$CitationReference$new(index, { let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = text.intoRustString(); rustString.isOwned = false; return rustString.ptr }()))
    }
}
public class CitationReferenceRefMut: CitationReferenceRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CitationReferenceRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CitationReferenceRef {
    public func index() -> UInt {
        __swift_bridge__$CitationReference$index(ptr)
    }

    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$CitationReference$url(ptr))
    }

    public func text() -> RustString {
        RustString(ptr: __swift_bridge__$CitationReference$text(ptr))
    }
}
extension CitationReference: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CitationReference$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CitationReference$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CitationReference) {
        __swift_bridge__$Vec_CitationReference$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CitationReference$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CitationReference(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CitationReferenceRef> {
        let pointer = __swift_bridge__$Vec_CitationReference$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CitationReferenceRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CitationReferenceRefMut> {
        let pointer = __swift_bridge__$Vec_CitationReference$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CitationReferenceRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CitationReferenceRef> {
        UnsafePointer<CitationReferenceRef>(OpaquePointer(__swift_bridge__$Vec_CitationReference$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CitationReference$len(vecPtr)
    }
}


public class CitationResult: CitationResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CitationResult$_free(ptr)
        }
    }
}
extension CitationResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ content: GenericIntoRustString, _ references: RustVec<CitationReference>) {
        self.init(ptr: __swift_bridge__$CitationResult$new({ let rustString = content.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let val = references; val.isOwned = false; return val.ptr }()))
    }
}
public class CitationResultRefMut: CitationResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CitationResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CitationResultRef {
    public func content() -> RustString {
        RustString(ptr: __swift_bridge__$CitationResult$content(ptr))
    }

    public func references() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$CitationResult$references(ptr))
    }
}
extension CitationResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CitationResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CitationResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CitationResult) {
        __swift_bridge__$Vec_CitationResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CitationResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CitationResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CitationResultRef> {
        let pointer = __swift_bridge__$Vec_CitationResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CitationResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CitationResultRefMut> {
        let pointer = __swift_bridge__$Vec_CitationResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CitationResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CitationResultRef> {
        UnsafePointer<CitationResultRef>(OpaquePointer(__swift_bridge__$Vec_CitationResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CitationResult$len(vecPtr)
    }
}


public class ContentConfig: ContentConfigRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ContentConfig$_free(ptr)
        }
    }
}
extension ContentConfig {
    public convenience init<GenericIntoRustString: IntoRustString>(_ output_format: GenericIntoRustString, _ preprocessing_preset: GenericIntoRustString, _ remove_navigation: Bool, _ remove_forms: Bool, _ strip_tags: RustVec<GenericIntoRustString>, _ preserve_tags: RustVec<GenericIntoRustString>, _ exclude_selectors: RustVec<GenericIntoRustString>, _ skip_images: Bool, _ max_depth: Optional<UInt>, _ wrap: Bool, _ wrap_width: UInt, _ include_document_structure: Bool) {
        self.init(ptr: __swift_bridge__$ContentConfig$new({ let rustString = output_format.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = preprocessing_preset.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), remove_navigation, remove_forms, { let val = strip_tags; val.isOwned = false; return val.ptr }(), { let val = preserve_tags; val.isOwned = false; return val.ptr }(), { let val = exclude_selectors; val.isOwned = false; return val.ptr }(), skip_images, max_depth.intoFfiRepr(), wrap, wrap_width, include_document_structure))
    }
}
public class ContentConfigRefMut: ContentConfigRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ContentConfigRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ContentConfigRef {
    public func outputFormat() -> RustString {
        RustString(ptr: __swift_bridge__$ContentConfig$output_format(ptr))
    }

    public func preprocessingPreset() -> RustString {
        RustString(ptr: __swift_bridge__$ContentConfig$preprocessing_preset(ptr))
    }

    public func removeNavigation() -> Bool {
        __swift_bridge__$ContentConfig$remove_navigation(ptr)
    }

    public func removeForms() -> Bool {
        __swift_bridge__$ContentConfig$remove_forms(ptr)
    }

    public func stripTags() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$ContentConfig$strip_tags(ptr))
    }

    public func preserveTags() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$ContentConfig$preserve_tags(ptr))
    }

    public func excludeSelectors() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$ContentConfig$exclude_selectors(ptr))
    }

    public func skipImages() -> Bool {
        __swift_bridge__$ContentConfig$skip_images(ptr)
    }

    public func maxDepth() -> Optional<UInt> {
        __swift_bridge__$ContentConfig$max_depth(ptr).intoSwiftRepr()
    }

    public func wrap() -> Bool {
        __swift_bridge__$ContentConfig$wrap(ptr)
    }

    public func wrapWidth() -> UInt {
        __swift_bridge__$ContentConfig$wrap_width(ptr)
    }

    public func includeDocumentStructure() -> Bool {
        __swift_bridge__$ContentConfig$include_document_structure(ptr)
    }
}
extension ContentConfig: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ContentConfig$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ContentConfig$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ContentConfig) {
        __swift_bridge__$Vec_ContentConfig$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ContentConfig$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ContentConfig(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ContentConfigRef> {
        let pointer = __swift_bridge__$Vec_ContentConfig$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ContentConfigRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ContentConfigRefMut> {
        let pointer = __swift_bridge__$Vec_ContentConfig$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ContentConfigRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ContentConfigRef> {
        UnsafePointer<ContentConfigRef>(OpaquePointer(__swift_bridge__$Vec_ContentConfig$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ContentConfig$len(vecPtr)
    }
}


public class CookieInfo: CookieInfoRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CookieInfo$_free(ptr)
        }
    }
}
extension CookieInfo {
    public convenience init<GenericIntoRustString: IntoRustString>(_ name: GenericIntoRustString, _ value: GenericIntoRustString, _ domain: Optional<GenericIntoRustString>, _ path: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$CookieInfo$new({ let rustString = name.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = value.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(domain) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(path) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class CookieInfoRefMut: CookieInfoRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CookieInfoRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CookieInfoRef {
    public func name() -> RustString {
        RustString(ptr: __swift_bridge__$CookieInfo$name(ptr))
    }

    public func value() -> RustString {
        RustString(ptr: __swift_bridge__$CookieInfo$value(ptr))
    }

    public func domain() -> Optional<RustString> {
        { let val = __swift_bridge__$CookieInfo$domain(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func path() -> Optional<RustString> {
        { let val = __swift_bridge__$CookieInfo$path(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension CookieInfo: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CookieInfo$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CookieInfo$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CookieInfo) {
        __swift_bridge__$Vec_CookieInfo$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CookieInfo$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CookieInfo(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CookieInfoRef> {
        let pointer = __swift_bridge__$Vec_CookieInfo$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CookieInfoRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CookieInfoRefMut> {
        let pointer = __swift_bridge__$Vec_CookieInfo$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CookieInfoRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CookieInfoRef> {
        UnsafePointer<CookieInfoRef>(OpaquePointer(__swift_bridge__$Vec_CookieInfo$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CookieInfo$len(vecPtr)
    }
}


public class CrawlConfig: CrawlConfigRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlConfig$_free(ptr)
        }
    }
}
public class CrawlConfigRefMut: CrawlConfigRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CrawlConfigRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlConfigRef {
    public func maxDepth() -> Optional<UInt> {
        __swift_bridge__$CrawlConfig$max_depth(ptr).intoSwiftRepr()
    }

    public func maxPages() -> Optional<UInt> {
        __swift_bridge__$CrawlConfig$max_pages(ptr).intoSwiftRepr()
    }

    public func maxLinksPerPage() -> Optional<UInt> {
        __swift_bridge__$CrawlConfig$max_links_per_page(ptr).intoSwiftRepr()
    }

    public func maxConcurrent() -> Optional<UInt> {
        __swift_bridge__$CrawlConfig$max_concurrent(ptr).intoSwiftRepr()
    }

    public func crawlStrategy() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlConfig$crawl_strategy(ptr))
    }

    public func contentFilter() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$content_filter(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func bm25Query() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$bm25_query(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func bm25Threshold() -> Optional<Double> {
        __swift_bridge__$CrawlConfig$bm25_threshold(ptr).intoSwiftRepr()
    }

    public func respectRobotsTxt() -> Bool {
        __swift_bridge__$CrawlConfig$respect_robots_txt(ptr)
    }

    public func softHttpErrors() -> Bool {
        __swift_bridge__$CrawlConfig$soft_http_errors(ptr)
    }

    public func userAgent() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$user_agent(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func stayOnDomain() -> Bool {
        __swift_bridge__$CrawlConfig$stay_on_domain(ptr)
    }

    public func allowSubdomains() -> Bool {
        __swift_bridge__$CrawlConfig$allow_subdomains(ptr)
    }

    public func includePaths() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$CrawlConfig$include_paths(ptr))
    }

    public func excludePaths() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$CrawlConfig$exclude_paths(ptr))
    }

    public func customHeaders() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlConfig$custom_headers(ptr))
    }

    public func requestTimeout() -> UInt64 {
        __swift_bridge__$CrawlConfig$request_timeout(ptr)
    }

    public func rateLimitMs() -> Optional<UInt64> {
        __swift_bridge__$CrawlConfig$rate_limit_ms(ptr).intoSwiftRepr()
    }

    public func maxRedirects() -> UInt {
        __swift_bridge__$CrawlConfig$max_redirects(ptr)
    }

    public func retryCount() -> UInt {
        __swift_bridge__$CrawlConfig$retry_count(ptr)
    }

    public func retryCodes() -> RustVec<UInt16> {
        RustVec(ptr: __swift_bridge__$CrawlConfig$retry_codes(ptr))
    }

    public func cookiesEnabled() -> Bool {
        __swift_bridge__$CrawlConfig$cookies_enabled(ptr)
    }

    public func auth() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$auth(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func maxBodySize() -> Optional<UInt> {
        __swift_bridge__$CrawlConfig$max_body_size(ptr).intoSwiftRepr()
    }

    public func removeTags() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$CrawlConfig$remove_tags(ptr))
    }

    public func content() -> ContentConfig {
        ContentConfig(ptr: __swift_bridge__$CrawlConfig$content(ptr))
    }

    public func mapLimit() -> Optional<UInt> {
        __swift_bridge__$CrawlConfig$map_limit(ptr).intoSwiftRepr()
    }

    public func mapSearch() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$map_search(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func downloadAssets() -> Bool {
        __swift_bridge__$CrawlConfig$download_assets(ptr)
    }

    public func assetTypes() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$CrawlConfig$asset_types(ptr))
    }

    public func maxAssetSize() -> Optional<UInt> {
        __swift_bridge__$CrawlConfig$max_asset_size(ptr).intoSwiftRepr()
    }

    public func browser() -> BrowserConfig {
        BrowserConfig(ptr: __swift_bridge__$CrawlConfig$browser(ptr))
    }

    public func proxy() -> Optional<ProxyConfig> {
        { let val = __swift_bridge__$CrawlConfig$proxy(ptr); if val != nil { return ProxyConfig(ptr: val!) } else { return nil } }()
    }

    public func userAgents() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$CrawlConfig$user_agents(ptr))
    }

    public func captureScreenshot() -> Bool {
        __swift_bridge__$CrawlConfig$capture_screenshot(ptr)
    }

    public func followDocumentUrls() -> Bool {
        __swift_bridge__$CrawlConfig$follow_document_urls(ptr)
    }

    public func documentUrlDepth() -> Optional<UInt32> {
        __swift_bridge__$CrawlConfig$document_url_depth(ptr).intoSwiftRepr()
    }

    public func downloadDocuments() -> Bool {
        __swift_bridge__$CrawlConfig$download_documents(ptr)
    }

    public func documentMaxSize() -> Optional<UInt> {
        __swift_bridge__$CrawlConfig$document_max_size(ptr).intoSwiftRepr()
    }

    public func documentMimeTypes() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$CrawlConfig$document_mime_types(ptr))
    }

    public func documentOutputDir() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$document_output_dir(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func documentContentEncoding() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$document_content_encoding(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func warcOutput() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$warc_output(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func browserProfile() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlConfig$browser_profile(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func saveBrowserProfile() -> Bool {
        __swift_bridge__$CrawlConfig$save_browser_profile(ptr)
    }

    public func ssrf() -> SsrfPolicy {
        SsrfPolicy(ptr: __swift_bridge__$CrawlConfig$ssrf(ptr))
    }

    public func ssrfDenyPrivateExplicit() -> Optional<Bool> {
        __swift_bridge__$CrawlConfig$ssrf_deny_private_explicit(ptr).intoSwiftRepr()
    }
}
extension CrawlConfig: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlConfig$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlConfig$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlConfig) {
        __swift_bridge__$Vec_CrawlConfig$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlConfig$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlConfig(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlConfigRef> {
        let pointer = __swift_bridge__$Vec_CrawlConfig$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlConfigRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlConfigRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlConfig$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlConfigRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlConfigRef> {
        UnsafePointer<CrawlConfigRef>(OpaquePointer(__swift_bridge__$Vec_CrawlConfig$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlConfig$len(vecPtr)
    }
}


public class CrawlPageResult: CrawlPageResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlPageResult$_free(ptr)
        }
    }
}
extension CrawlPageResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ normalized_url: GenericIntoRustString, _ status_code: UInt16, _ content_type: GenericIntoRustString, _ html: GenericIntoRustString, _ body_size: UInt, _ metadata: PageMetadata, _ links: RustVec<LinkInfo>, _ images: RustVec<ImageInfo>, _ feeds: RustVec<FeedInfo>, _ json_ld: RustVec<JsonLdEntry>, _ depth: UInt, _ stayed_on_domain: Bool, _ was_skipped: Bool, _ is_pdf: Bool, _ detected_charset: Optional<GenericIntoRustString>, _ markdown: Optional<MarkdownResult>, _ extracted_data: Optional<GenericIntoRustString>, _ extraction_meta: Optional<ExtractionMeta>, _ downloaded_document: Optional<DownloadedDocument>, _ browser_used: Bool) {
        self.init(ptr: __swift_bridge__$CrawlPageResult$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = normalized_url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), status_code, { let rustString = content_type.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = html.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), body_size, {metadata.isOwned = false; return metadata.ptr;}(), { let val = links; val.isOwned = false; return val.ptr }(), { let val = images; val.isOwned = false; return val.ptr }(), { let val = feeds; val.isOwned = false; return val.ptr }(), { let val = json_ld; val.isOwned = false; return val.ptr }(), depth, stayed_on_domain, was_skipped, is_pdf, { if let rustString = optionalStringIntoRustString(detected_charset) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let val = markdown { val.isOwned = false; return val.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(extracted_data) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let val = extraction_meta { val.isOwned = false; return val.ptr } else { return nil } }(), { if let val = downloaded_document { val.isOwned = false; return val.ptr } else { return nil } }(), browser_used))
    }
}
public class CrawlPageResultRefMut: CrawlPageResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CrawlPageResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlPageResultRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlPageResult$url(ptr))
    }

    public func normalizedUrl() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlPageResult$normalized_url(ptr))
    }

    public func statusCode() -> UInt16 {
        __swift_bridge__$CrawlPageResult$status_code(ptr)
    }

    public func contentType() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlPageResult$content_type(ptr))
    }

    public func html() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlPageResult$html(ptr))
    }

    public func bodySize() -> UInt {
        __swift_bridge__$CrawlPageResult$body_size(ptr)
    }

    public func metadata() -> PageMetadata {
        PageMetadata(ptr: __swift_bridge__$CrawlPageResult$metadata(ptr))
    }

    public func links() -> RustVec<LinkInfo> {
        RustVec(ptr: __swift_bridge__$CrawlPageResult$links(ptr))
    }

    public func images() -> RustVec<ImageInfo> {
        RustVec(ptr: __swift_bridge__$CrawlPageResult$images(ptr))
    }

    public func feeds() -> RustVec<FeedInfo> {
        RustVec(ptr: __swift_bridge__$CrawlPageResult$feeds(ptr))
    }

    public func jsonLd() -> RustVec<JsonLdEntry> {
        RustVec(ptr: __swift_bridge__$CrawlPageResult$json_ld(ptr))
    }

    public func depth() -> UInt {
        __swift_bridge__$CrawlPageResult$depth(ptr)
    }

    public func stayedOnDomain() -> Bool {
        __swift_bridge__$CrawlPageResult$stayed_on_domain(ptr)
    }

    public func wasSkipped() -> Bool {
        __swift_bridge__$CrawlPageResult$was_skipped(ptr)
    }

    public func isPdf() -> Bool {
        __swift_bridge__$CrawlPageResult$is_pdf(ptr)
    }

    public func detectedCharset() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlPageResult$detected_charset(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func markdown() -> Optional<MarkdownResult> {
        { let val = __swift_bridge__$CrawlPageResult$markdown(ptr); if val != nil { return MarkdownResult(ptr: val!) } else { return nil } }()
    }

    public func extractedData() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlPageResult$extracted_data(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func extractionMeta() -> Optional<ExtractionMeta> {
        { let val = __swift_bridge__$CrawlPageResult$extraction_meta(ptr); if val != nil { return ExtractionMeta(ptr: val!) } else { return nil } }()
    }

    public func downloadedDocument() -> Optional<DownloadedDocument> {
        { let val = __swift_bridge__$CrawlPageResult$downloaded_document(ptr); if val != nil { return DownloadedDocument(ptr: val!) } else { return nil } }()
    }

    public func browserUsed() -> Bool {
        __swift_bridge__$CrawlPageResult$browser_used(ptr)
    }
}
extension CrawlPageResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlPageResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlPageResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlPageResult) {
        __swift_bridge__$Vec_CrawlPageResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlPageResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlPageResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlPageResultRef> {
        let pointer = __swift_bridge__$Vec_CrawlPageResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlPageResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlPageResultRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlPageResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlPageResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlPageResultRef> {
        UnsafePointer<CrawlPageResultRef>(OpaquePointer(__swift_bridge__$Vec_CrawlPageResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlPageResult$len(vecPtr)
    }
}


public class CrawlResult: CrawlResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlResult$_free(ptr)
        }
    }
}
extension CrawlResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ pages: RustVec<CrawlPageResult>, _ final_url: GenericIntoRustString, _ redirect_count: UInt, _ was_skipped: Bool, _ error: Optional<GenericIntoRustString>, _ cookies: RustVec<CookieInfo>, _ stayed_on_domain: Bool, _ browser_used: Bool) {
        self.init(ptr: __swift_bridge__$CrawlResult$new({ let val = pages; val.isOwned = false; return val.ptr }(), { let rustString = final_url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), redirect_count, was_skipped, { if let rustString = optionalStringIntoRustString(error) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { let val = cookies; val.isOwned = false; return val.ptr }(), stayed_on_domain, browser_used))
    }
}
public class CrawlResultRefMut: CrawlResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CrawlResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlResultRef {
    public func pages() -> RustVec<CrawlPageResult> {
        RustVec(ptr: __swift_bridge__$CrawlResult$pages(ptr))
    }

    public func finalUrl() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlResult$final_url(ptr))
    }

    public func redirectCount() -> UInt {
        __swift_bridge__$CrawlResult$redirect_count(ptr)
    }

    public func wasSkipped() -> Bool {
        __swift_bridge__$CrawlResult$was_skipped(ptr)
    }

    public func error() -> Optional<RustString> {
        { let val = __swift_bridge__$CrawlResult$error(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func cookies() -> RustVec<CookieInfo> {
        RustVec(ptr: __swift_bridge__$CrawlResult$cookies(ptr))
    }

    public func stayedOnDomain() -> Bool {
        __swift_bridge__$CrawlResult$stayed_on_domain(ptr)
    }

    public func browserUsed() -> Bool {
        __swift_bridge__$CrawlResult$browser_used(ptr)
    }
}
extension CrawlResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlResult) {
        __swift_bridge__$Vec_CrawlResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlResultRef> {
        let pointer = __swift_bridge__$Vec_CrawlResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlResultRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlResultRef> {
        UnsafePointer<CrawlResultRef>(OpaquePointer(__swift_bridge__$Vec_CrawlResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlResult$len(vecPtr)
    }
}


public class CrawlStreamRequest: CrawlStreamRequestRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlStreamRequest$_free(ptr)
        }
    }
}
extension CrawlStreamRequest {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString) {
        self.init(ptr: __swift_bridge__$CrawlStreamRequest$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }()))
    }
}
public class CrawlStreamRequestRefMut: CrawlStreamRequestRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CrawlStreamRequestRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlStreamRequestRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlStreamRequest$url(ptr))
    }
}
extension CrawlStreamRequest: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlStreamRequest$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlStreamRequest$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlStreamRequest) {
        __swift_bridge__$Vec_CrawlStreamRequest$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlStreamRequest$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlStreamRequest(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlStreamRequestRef> {
        let pointer = __swift_bridge__$Vec_CrawlStreamRequest$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlStreamRequestRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlStreamRequestRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlStreamRequest$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlStreamRequestRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlStreamRequestRef> {
        UnsafePointer<CrawlStreamRequestRef>(OpaquePointer(__swift_bridge__$Vec_CrawlStreamRequest$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlStreamRequest$len(vecPtr)
    }
}


public class DownloadedAsset: DownloadedAssetRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$DownloadedAsset$_free(ptr)
        }
    }
}
extension DownloadedAsset {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ content_hash: GenericIntoRustString, _ mime_type: Optional<GenericIntoRustString>, _ size: UInt, _ asset_category: AssetCategory, _ html_tag: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$DownloadedAsset$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = content_hash.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(mime_type) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), size, {asset_category.isOwned = false; return asset_category.ptr;}(), { if let rustString = optionalStringIntoRustString(html_tag) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class DownloadedAssetRefMut: DownloadedAssetRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class DownloadedAssetRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension DownloadedAssetRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$DownloadedAsset$url(ptr))
    }

    public func contentHash() -> RustString {
        RustString(ptr: __swift_bridge__$DownloadedAsset$content_hash(ptr))
    }

    public func mimeType() -> Optional<RustString> {
        { let val = __swift_bridge__$DownloadedAsset$mime_type(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func size() -> UInt {
        __swift_bridge__$DownloadedAsset$size(ptr)
    }

    public func assetCategory() -> RustString {
        RustString(ptr: __swift_bridge__$DownloadedAsset$asset_category(ptr))
    }

    public func htmlTag() -> Optional<RustString> {
        { let val = __swift_bridge__$DownloadedAsset$html_tag(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension DownloadedAsset: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_DownloadedAsset$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_DownloadedAsset$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: DownloadedAsset) {
        __swift_bridge__$Vec_DownloadedAsset$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_DownloadedAsset$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (DownloadedAsset(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<DownloadedAssetRef> {
        let pointer = __swift_bridge__$Vec_DownloadedAsset$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return DownloadedAssetRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<DownloadedAssetRefMut> {
        let pointer = __swift_bridge__$Vec_DownloadedAsset$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return DownloadedAssetRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<DownloadedAssetRef> {
        UnsafePointer<DownloadedAssetRef>(OpaquePointer(__swift_bridge__$Vec_DownloadedAsset$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_DownloadedAsset$len(vecPtr)
    }
}


public class DownloadedDocument: DownloadedDocumentRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$DownloadedDocument$_free(ptr)
        }
    }
}
public class DownloadedDocumentRefMut: DownloadedDocumentRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class DownloadedDocumentRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension DownloadedDocumentRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$DownloadedDocument$url(ptr))
    }

    public func mimeType() -> RustString {
        RustString(ptr: __swift_bridge__$DownloadedDocument$mime_type(ptr))
    }

    public func size() -> UInt {
        __swift_bridge__$DownloadedDocument$size(ptr)
    }

    public func filename() -> Optional<RustString> {
        { let val = __swift_bridge__$DownloadedDocument$filename(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func contentHash() -> RustString {
        RustString(ptr: __swift_bridge__$DownloadedDocument$content_hash(ptr))
    }

    public func headers() -> RustString {
        RustString(ptr: __swift_bridge__$DownloadedDocument$headers(ptr))
    }

    public func truncated() -> Bool {
        __swift_bridge__$DownloadedDocument$truncated(ptr)
    }

    public func contentPath() -> Optional<RustString> {
        { let val = __swift_bridge__$DownloadedDocument$content_path(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func contentBase64() -> Optional<RustString> {
        { let val = __swift_bridge__$DownloadedDocument$content_base64(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension DownloadedDocument: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_DownloadedDocument$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_DownloadedDocument$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: DownloadedDocument) {
        __swift_bridge__$Vec_DownloadedDocument$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_DownloadedDocument$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (DownloadedDocument(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<DownloadedDocumentRef> {
        let pointer = __swift_bridge__$Vec_DownloadedDocument$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return DownloadedDocumentRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<DownloadedDocumentRefMut> {
        let pointer = __swift_bridge__$Vec_DownloadedDocument$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return DownloadedDocumentRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<DownloadedDocumentRef> {
        UnsafePointer<DownloadedDocumentRef>(OpaquePointer(__swift_bridge__$Vec_DownloadedDocument$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_DownloadedDocument$len(vecPtr)
    }
}


public class ExtractionMeta: ExtractionMetaRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ExtractionMeta$_free(ptr)
        }
    }
}
extension ExtractionMeta {
    public convenience init<GenericIntoRustString: IntoRustString>(_ cost: Optional<Double>, _ prompt_tokens: Optional<UInt64>, _ completion_tokens: Optional<UInt64>, _ model: Optional<GenericIntoRustString>, _ chunks_processed: UInt) {
        self.init(ptr: __swift_bridge__$ExtractionMeta$new(cost.intoFfiRepr(), prompt_tokens.intoFfiRepr(), completion_tokens.intoFfiRepr(), { if let rustString = optionalStringIntoRustString(model) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), chunks_processed))
    }
}
public class ExtractionMetaRefMut: ExtractionMetaRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ExtractionMetaRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ExtractionMetaRef {
    public func cost() -> Optional<Double> {
        __swift_bridge__$ExtractionMeta$cost(ptr).intoSwiftRepr()
    }

    public func promptTokens() -> Optional<UInt64> {
        __swift_bridge__$ExtractionMeta$prompt_tokens(ptr).intoSwiftRepr()
    }

    public func completionTokens() -> Optional<UInt64> {
        __swift_bridge__$ExtractionMeta$completion_tokens(ptr).intoSwiftRepr()
    }

    public func model() -> Optional<RustString> {
        { let val = __swift_bridge__$ExtractionMeta$model(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func chunksProcessed() -> UInt {
        __swift_bridge__$ExtractionMeta$chunks_processed(ptr)
    }
}
extension ExtractionMeta: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ExtractionMeta$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ExtractionMeta$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ExtractionMeta) {
        __swift_bridge__$Vec_ExtractionMeta$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ExtractionMeta$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ExtractionMeta(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ExtractionMetaRef> {
        let pointer = __swift_bridge__$Vec_ExtractionMeta$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ExtractionMetaRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ExtractionMetaRefMut> {
        let pointer = __swift_bridge__$Vec_ExtractionMeta$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ExtractionMetaRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ExtractionMetaRef> {
        UnsafePointer<ExtractionMetaRef>(OpaquePointer(__swift_bridge__$Vec_ExtractionMeta$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ExtractionMeta$len(vecPtr)
    }
}


public class FaviconInfo: FaviconInfoRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$FaviconInfo$_free(ptr)
        }
    }
}
extension FaviconInfo {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ rel: GenericIntoRustString, _ sizes: Optional<GenericIntoRustString>, _ mime_type: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$FaviconInfo$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = rel.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(sizes) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(mime_type) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class FaviconInfoRefMut: FaviconInfoRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class FaviconInfoRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension FaviconInfoRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$FaviconInfo$url(ptr))
    }

    public func rel() -> RustString {
        RustString(ptr: __swift_bridge__$FaviconInfo$rel(ptr))
    }

    public func sizes() -> Optional<RustString> {
        { let val = __swift_bridge__$FaviconInfo$sizes(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func mimeType() -> Optional<RustString> {
        { let val = __swift_bridge__$FaviconInfo$mime_type(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension FaviconInfo: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_FaviconInfo$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_FaviconInfo$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: FaviconInfo) {
        __swift_bridge__$Vec_FaviconInfo$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_FaviconInfo$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (FaviconInfo(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<FaviconInfoRef> {
        let pointer = __swift_bridge__$Vec_FaviconInfo$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return FaviconInfoRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<FaviconInfoRefMut> {
        let pointer = __swift_bridge__$Vec_FaviconInfo$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return FaviconInfoRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<FaviconInfoRef> {
        UnsafePointer<FaviconInfoRef>(OpaquePointer(__swift_bridge__$Vec_FaviconInfo$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_FaviconInfo$len(vecPtr)
    }
}


public class FeedInfo: FeedInfoRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$FeedInfo$_free(ptr)
        }
    }
}
extension FeedInfo {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ title: Optional<GenericIntoRustString>, _ feed_type: FeedType) {
        self.init(ptr: __swift_bridge__$FeedInfo$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(title) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), {feed_type.isOwned = false; return feed_type.ptr;}()))
    }
}
public class FeedInfoRefMut: FeedInfoRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class FeedInfoRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension FeedInfoRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$FeedInfo$url(ptr))
    }

    public func title() -> Optional<RustString> {
        { let val = __swift_bridge__$FeedInfo$title(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func feedType() -> RustString {
        RustString(ptr: __swift_bridge__$FeedInfo$feed_type(ptr))
    }
}
extension FeedInfo: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_FeedInfo$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_FeedInfo$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: FeedInfo) {
        __swift_bridge__$Vec_FeedInfo$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_FeedInfo$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (FeedInfo(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<FeedInfoRef> {
        let pointer = __swift_bridge__$Vec_FeedInfo$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return FeedInfoRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<FeedInfoRefMut> {
        let pointer = __swift_bridge__$Vec_FeedInfo$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return FeedInfoRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<FeedInfoRef> {
        UnsafePointer<FeedInfoRef>(OpaquePointer(__swift_bridge__$Vec_FeedInfo$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_FeedInfo$len(vecPtr)
    }
}


public class HeadingInfo: HeadingInfoRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$HeadingInfo$_free(ptr)
        }
    }
}
extension HeadingInfo {
    public convenience init<GenericIntoRustString: IntoRustString>(_ level: UInt8, _ text: GenericIntoRustString) {
        self.init(ptr: __swift_bridge__$HeadingInfo$new(level, { let rustString = text.intoRustString(); rustString.isOwned = false; return rustString.ptr }()))
    }
}
public class HeadingInfoRefMut: HeadingInfoRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class HeadingInfoRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension HeadingInfoRef {
    public func level() -> UInt8 {
        __swift_bridge__$HeadingInfo$level(ptr)
    }

    public func text() -> RustString {
        RustString(ptr: __swift_bridge__$HeadingInfo$text(ptr))
    }
}
extension HeadingInfo: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_HeadingInfo$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_HeadingInfo$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: HeadingInfo) {
        __swift_bridge__$Vec_HeadingInfo$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_HeadingInfo$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (HeadingInfo(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<HeadingInfoRef> {
        let pointer = __swift_bridge__$Vec_HeadingInfo$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return HeadingInfoRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<HeadingInfoRefMut> {
        let pointer = __swift_bridge__$Vec_HeadingInfo$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return HeadingInfoRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<HeadingInfoRef> {
        UnsafePointer<HeadingInfoRef>(OpaquePointer(__swift_bridge__$Vec_HeadingInfo$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_HeadingInfo$len(vecPtr)
    }
}


public class HreflangEntry: HreflangEntryRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$HreflangEntry$_free(ptr)
        }
    }
}
extension HreflangEntry {
    public convenience init<GenericIntoRustString: IntoRustString>(_ lang: GenericIntoRustString, _ url: GenericIntoRustString) {
        self.init(ptr: __swift_bridge__$HreflangEntry$new({ let rustString = lang.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }()))
    }
}
public class HreflangEntryRefMut: HreflangEntryRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class HreflangEntryRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension HreflangEntryRef {
    public func lang() -> RustString {
        RustString(ptr: __swift_bridge__$HreflangEntry$lang(ptr))
    }

    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$HreflangEntry$url(ptr))
    }
}
extension HreflangEntry: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_HreflangEntry$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_HreflangEntry$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: HreflangEntry) {
        __swift_bridge__$Vec_HreflangEntry$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_HreflangEntry$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (HreflangEntry(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<HreflangEntryRef> {
        let pointer = __swift_bridge__$Vec_HreflangEntry$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return HreflangEntryRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<HreflangEntryRefMut> {
        let pointer = __swift_bridge__$Vec_HreflangEntry$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return HreflangEntryRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<HreflangEntryRef> {
        UnsafePointer<HreflangEntryRef>(OpaquePointer(__swift_bridge__$Vec_HreflangEntry$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_HreflangEntry$len(vecPtr)
    }
}


public class ImageInfo: ImageInfoRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ImageInfo$_free(ptr)
        }
    }
}
extension ImageInfo {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ alt: Optional<GenericIntoRustString>, _ width: Optional<UInt32>, _ height: Optional<UInt32>, _ source: ImageSource) {
        self.init(ptr: __swift_bridge__$ImageInfo$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(alt) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), width.intoFfiRepr(), height.intoFfiRepr(), {source.isOwned = false; return source.ptr;}()))
    }
}
public class ImageInfoRefMut: ImageInfoRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ImageInfoRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ImageInfoRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$ImageInfo$url(ptr))
    }

    public func alt() -> Optional<RustString> {
        { let val = __swift_bridge__$ImageInfo$alt(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func width() -> Optional<UInt32> {
        __swift_bridge__$ImageInfo$width(ptr).intoSwiftRepr()
    }

    public func height() -> Optional<UInt32> {
        __swift_bridge__$ImageInfo$height(ptr).intoSwiftRepr()
    }

    public func source() -> RustString {
        RustString(ptr: __swift_bridge__$ImageInfo$source(ptr))
    }
}
extension ImageInfo: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ImageInfo$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ImageInfo$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ImageInfo) {
        __swift_bridge__$Vec_ImageInfo$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ImageInfo$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ImageInfo(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ImageInfoRef> {
        let pointer = __swift_bridge__$Vec_ImageInfo$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ImageInfoRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ImageInfoRefMut> {
        let pointer = __swift_bridge__$Vec_ImageInfo$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ImageInfoRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ImageInfoRef> {
        UnsafePointer<ImageInfoRef>(OpaquePointer(__swift_bridge__$Vec_ImageInfo$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ImageInfo$len(vecPtr)
    }
}


public class InteractionResult: InteractionResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$InteractionResult$_free(ptr)
        }
    }
}
extension InteractionResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ action_results: RustVec<ActionResult>, _ final_html: GenericIntoRustString, _ final_url: GenericIntoRustString, _ screenshot_base64: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$InteractionResult$new({ let val = action_results; val.isOwned = false; return val.ptr }(), { let rustString = final_html.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = final_url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(screenshot_base64) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class InteractionResultRefMut: InteractionResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class InteractionResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension InteractionResultRef {
    public func actionResults() -> RustVec<ActionResult> {
        RustVec(ptr: __swift_bridge__$InteractionResult$action_results(ptr))
    }

    public func finalHtml() -> RustString {
        RustString(ptr: __swift_bridge__$InteractionResult$final_html(ptr))
    }

    public func finalUrl() -> RustString {
        RustString(ptr: __swift_bridge__$InteractionResult$final_url(ptr))
    }

    public func screenshotBase64() -> Optional<RustString> {
        { let val = __swift_bridge__$InteractionResult$screenshot_base64(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension InteractionResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_InteractionResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_InteractionResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: InteractionResult) {
        __swift_bridge__$Vec_InteractionResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_InteractionResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (InteractionResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<InteractionResultRef> {
        let pointer = __swift_bridge__$Vec_InteractionResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return InteractionResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<InteractionResultRefMut> {
        let pointer = __swift_bridge__$Vec_InteractionResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return InteractionResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<InteractionResultRef> {
        UnsafePointer<InteractionResultRef>(OpaquePointer(__swift_bridge__$Vec_InteractionResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_InteractionResult$len(vecPtr)
    }
}


public class JsonLdEntry: JsonLdEntryRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$JsonLdEntry$_free(ptr)
        }
    }
}
extension JsonLdEntry {
    public convenience init<GenericIntoRustString: IntoRustString>(_ schema_type: GenericIntoRustString, _ name: Optional<GenericIntoRustString>, _ raw: GenericIntoRustString) {
        self.init(ptr: __swift_bridge__$JsonLdEntry$new({ let rustString = schema_type.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(name) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { let rustString = raw.intoRustString(); rustString.isOwned = false; return rustString.ptr }()))
    }
}
public class JsonLdEntryRefMut: JsonLdEntryRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class JsonLdEntryRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension JsonLdEntryRef {
    public func schemaType() -> RustString {
        RustString(ptr: __swift_bridge__$JsonLdEntry$schema_type(ptr))
    }

    public func name() -> Optional<RustString> {
        { let val = __swift_bridge__$JsonLdEntry$name(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func raw() -> RustString {
        RustString(ptr: __swift_bridge__$JsonLdEntry$raw(ptr))
    }
}
extension JsonLdEntry: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_JsonLdEntry$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_JsonLdEntry$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: JsonLdEntry) {
        __swift_bridge__$Vec_JsonLdEntry$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_JsonLdEntry$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (JsonLdEntry(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<JsonLdEntryRef> {
        let pointer = __swift_bridge__$Vec_JsonLdEntry$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return JsonLdEntryRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<JsonLdEntryRefMut> {
        let pointer = __swift_bridge__$Vec_JsonLdEntry$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return JsonLdEntryRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<JsonLdEntryRef> {
        UnsafePointer<JsonLdEntryRef>(OpaquePointer(__swift_bridge__$Vec_JsonLdEntry$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_JsonLdEntry$len(vecPtr)
    }
}


public class LinkInfo: LinkInfoRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$LinkInfo$_free(ptr)
        }
    }
}
extension LinkInfo {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ text: GenericIntoRustString, _ link_type: LinkType, _ rel: Optional<GenericIntoRustString>, _ nofollow: Bool) {
        self.init(ptr: __swift_bridge__$LinkInfo$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = text.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), {link_type.isOwned = false; return link_type.ptr;}(), { if let rustString = optionalStringIntoRustString(rel) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), nofollow))
    }
}
public class LinkInfoRefMut: LinkInfoRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class LinkInfoRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension LinkInfoRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$LinkInfo$url(ptr))
    }

    public func text() -> RustString {
        RustString(ptr: __swift_bridge__$LinkInfo$text(ptr))
    }

    public func linkType() -> RustString {
        RustString(ptr: __swift_bridge__$LinkInfo$link_type(ptr))
    }

    public func rel() -> Optional<RustString> {
        { let val = __swift_bridge__$LinkInfo$rel(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func nofollow() -> Bool {
        __swift_bridge__$LinkInfo$nofollow(ptr)
    }
}
extension LinkInfo: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_LinkInfo$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_LinkInfo$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: LinkInfo) {
        __swift_bridge__$Vec_LinkInfo$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_LinkInfo$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (LinkInfo(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<LinkInfoRef> {
        let pointer = __swift_bridge__$Vec_LinkInfo$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return LinkInfoRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<LinkInfoRefMut> {
        let pointer = __swift_bridge__$Vec_LinkInfo$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return LinkInfoRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<LinkInfoRef> {
        UnsafePointer<LinkInfoRef>(OpaquePointer(__swift_bridge__$Vec_LinkInfo$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_LinkInfo$len(vecPtr)
    }
}


public class MapResult: MapResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$MapResult$_free(ptr)
        }
    }
}
extension MapResult {
    public convenience init(_ urls: RustVec<SitemapUrl>) {
        self.init(ptr: __swift_bridge__$MapResult$new({ let val = urls; val.isOwned = false; return val.ptr }()))
    }
}
public class MapResultRefMut: MapResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class MapResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension MapResultRef {
    public func urls() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$MapResult$urls(ptr))
    }
}
extension MapResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_MapResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_MapResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: MapResult) {
        __swift_bridge__$Vec_MapResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_MapResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (MapResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<MapResultRef> {
        let pointer = __swift_bridge__$Vec_MapResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return MapResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<MapResultRefMut> {
        let pointer = __swift_bridge__$Vec_MapResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return MapResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<MapResultRef> {
        UnsafePointer<MapResultRef>(OpaquePointer(__swift_bridge__$Vec_MapResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_MapResult$len(vecPtr)
    }
}


public class MarkdownResult: MarkdownResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$MarkdownResult$_free(ptr)
        }
    }
}
extension MarkdownResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ content: GenericIntoRustString, _ document_structure: Optional<GenericIntoRustString>, _ tables: RustVec<GenericIntoRustString>, _ warnings: RustVec<GenericIntoRustString>, _ citations: Bool, _ fit_content: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$MarkdownResult$new({ let rustString = content.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(document_structure) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { let val = tables; val.isOwned = false; return val.ptr }(), { let val = warnings; val.isOwned = false; return val.ptr }(), citations, { if let rustString = optionalStringIntoRustString(fit_content) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class MarkdownResultRefMut: MarkdownResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class MarkdownResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension MarkdownResultRef {
    public func content() -> RustString {
        RustString(ptr: __swift_bridge__$MarkdownResult$content(ptr))
    }

    public func documentStructure() -> Optional<RustString> {
        { let val = __swift_bridge__$MarkdownResult$document_structure(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func tables() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$MarkdownResult$tables(ptr))
    }

    public func warnings() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$MarkdownResult$warnings(ptr))
    }

    public func citations() -> Bool {
        __swift_bridge__$MarkdownResult$citations(ptr)
    }

    public func fitContent() -> Optional<RustString> {
        { let val = __swift_bridge__$MarkdownResult$fit_content(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension MarkdownResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_MarkdownResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_MarkdownResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: MarkdownResult) {
        __swift_bridge__$Vec_MarkdownResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_MarkdownResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (MarkdownResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<MarkdownResultRef> {
        let pointer = __swift_bridge__$Vec_MarkdownResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return MarkdownResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<MarkdownResultRefMut> {
        let pointer = __swift_bridge__$Vec_MarkdownResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return MarkdownResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<MarkdownResultRef> {
        UnsafePointer<MarkdownResultRef>(OpaquePointer(__swift_bridge__$Vec_MarkdownResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_MarkdownResult$len(vecPtr)
    }
}


public class PageMetadata: PageMetadataRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$PageMetadata$_free(ptr)
        }
    }
}
public class PageMetadataRefMut: PageMetadataRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class PageMetadataRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension PageMetadataRef {
    public func title() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$title(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func description() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$description(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func canonicalUrl() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$canonical_url(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func keywords() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$keywords(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func author() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$author(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func viewport() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$viewport(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func themeColor() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$theme_color(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func generator() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$generator(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func robots() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$robots(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func htmlLang() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$html_lang(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func htmlDir() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$html_dir(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogTitle() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_title(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogType() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_type(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogImage() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_image(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogDescription() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_description(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogUrl() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_url(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogSiteName() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_site_name(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogLocale() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_locale(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogVideo() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_video(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogAudio() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$og_audio(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func ogLocaleAlternates() -> RustString {
        RustString(ptr: __swift_bridge__$PageMetadata$og_locale_alternates(ptr))
    }

    public func twitterCard() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$twitter_card(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func twitterTitle() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$twitter_title(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func twitterDescription() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$twitter_description(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func twitterImage() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$twitter_image(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func twitterSite() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$twitter_site(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func twitterCreator() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$twitter_creator(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcTitle() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_title(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcCreator() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_creator(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcSubject() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_subject(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcDescription() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_description(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcPublisher() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_publisher(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcDate() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_date(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcType() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_type(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcFormat() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_format(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcIdentifier() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_identifier(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcLanguage() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_language(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func dcRights() -> Optional<RustString> {
        { let val = __swift_bridge__$PageMetadata$dc_rights(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func article() -> Optional<ArticleMetadata> {
        { let val = __swift_bridge__$PageMetadata$article(ptr); if val != nil { return ArticleMetadata(ptr: val!) } else { return nil } }()
    }

    public func hreflangs() -> RustString {
        RustString(ptr: __swift_bridge__$PageMetadata$hreflangs(ptr))
    }

    public func favicons() -> RustString {
        RustString(ptr: __swift_bridge__$PageMetadata$favicons(ptr))
    }

    public func headings() -> RustString {
        RustString(ptr: __swift_bridge__$PageMetadata$headings(ptr))
    }

    public func wordCount() -> Optional<UInt> {
        __swift_bridge__$PageMetadata$word_count(ptr).intoSwiftRepr()
    }
}
extension PageMetadata: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_PageMetadata$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_PageMetadata$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: PageMetadata) {
        __swift_bridge__$Vec_PageMetadata$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_PageMetadata$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (PageMetadata(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PageMetadataRef> {
        let pointer = __swift_bridge__$Vec_PageMetadata$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PageMetadataRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PageMetadataRefMut> {
        let pointer = __swift_bridge__$Vec_PageMetadata$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PageMetadataRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<PageMetadataRef> {
        UnsafePointer<PageMetadataRef>(OpaquePointer(__swift_bridge__$Vec_PageMetadata$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_PageMetadata$len(vecPtr)
    }
}


public class ProxyConfig: ProxyConfigRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ProxyConfig$_free(ptr)
        }
    }
}
extension ProxyConfig {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ username: Optional<GenericIntoRustString>, _ password: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$ProxyConfig$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(username) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(password) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class ProxyConfigRefMut: ProxyConfigRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ProxyConfigRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ProxyConfigRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$ProxyConfig$url(ptr))
    }

    public func username() -> Optional<RustString> {
        { let val = __swift_bridge__$ProxyConfig$username(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func password() -> Optional<RustString> {
        { let val = __swift_bridge__$ProxyConfig$password(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension ProxyConfig: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ProxyConfig$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ProxyConfig$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ProxyConfig) {
        __swift_bridge__$Vec_ProxyConfig$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ProxyConfig$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ProxyConfig(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ProxyConfigRef> {
        let pointer = __swift_bridge__$Vec_ProxyConfig$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ProxyConfigRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ProxyConfigRefMut> {
        let pointer = __swift_bridge__$Vec_ProxyConfig$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ProxyConfigRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ProxyConfigRef> {
        UnsafePointer<ProxyConfigRef>(OpaquePointer(__swift_bridge__$Vec_ProxyConfig$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ProxyConfig$len(vecPtr)
    }
}


public class ResponseMeta: ResponseMetaRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ResponseMeta$_free(ptr)
        }
    }
}
extension ResponseMeta {
    public convenience init<GenericIntoRustString: IntoRustString>(_ etag: Optional<GenericIntoRustString>, _ last_modified: Optional<GenericIntoRustString>, _ cache_control: Optional<GenericIntoRustString>, _ server: Optional<GenericIntoRustString>, _ x_powered_by: Optional<GenericIntoRustString>, _ content_language: Optional<GenericIntoRustString>, _ content_encoding: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$ResponseMeta$new({ if let rustString = optionalStringIntoRustString(etag) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(last_modified) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(cache_control) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(server) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(x_powered_by) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(content_language) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(content_encoding) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class ResponseMetaRefMut: ResponseMetaRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ResponseMetaRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ResponseMetaRef {
    public func etag() -> Optional<RustString> {
        { let val = __swift_bridge__$ResponseMeta$etag(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func lastModified() -> Optional<RustString> {
        { let val = __swift_bridge__$ResponseMeta$last_modified(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func cacheControl() -> Optional<RustString> {
        { let val = __swift_bridge__$ResponseMeta$cache_control(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func server() -> Optional<RustString> {
        { let val = __swift_bridge__$ResponseMeta$server(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func xPoweredBy() -> Optional<RustString> {
        { let val = __swift_bridge__$ResponseMeta$x_powered_by(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func contentLanguage() -> Optional<RustString> {
        { let val = __swift_bridge__$ResponseMeta$content_language(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func contentEncoding() -> Optional<RustString> {
        { let val = __swift_bridge__$ResponseMeta$content_encoding(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension ResponseMeta: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ResponseMeta$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ResponseMeta$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ResponseMeta) {
        __swift_bridge__$Vec_ResponseMeta$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ResponseMeta$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ResponseMeta(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ResponseMetaRef> {
        let pointer = __swift_bridge__$Vec_ResponseMeta$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ResponseMetaRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ResponseMetaRefMut> {
        let pointer = __swift_bridge__$Vec_ResponseMeta$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ResponseMetaRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ResponseMetaRef> {
        UnsafePointer<ResponseMetaRef>(OpaquePointer(__swift_bridge__$Vec_ResponseMeta$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ResponseMeta$len(vecPtr)
    }
}


public class ScrapeResult: ScrapeResultRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ScrapeResult$_free(ptr)
        }
    }
}
extension ScrapeResult {
    public convenience init<GenericIntoRustString: IntoRustString>(_ status_code: UInt16, _ final_url: GenericIntoRustString, _ content_type: GenericIntoRustString, _ html: GenericIntoRustString, _ body_size: UInt, _ metadata: PageMetadata, _ links: RustVec<LinkInfo>, _ images: RustVec<ImageInfo>, _ feeds: RustVec<FeedInfo>, _ json_ld: RustVec<JsonLdEntry>, _ is_allowed: Bool, _ crawl_delay: Optional<UInt64>, _ noindex_detected: Bool, _ nofollow_detected: Bool, _ x_robots_tag: Optional<GenericIntoRustString>, _ is_pdf: Bool, _ was_skipped: Bool, _ detected_charset: Optional<GenericIntoRustString>, _ auth_header_sent: Bool, _ response_meta: Optional<ResponseMeta>, _ assets: RustVec<DownloadedAsset>, _ js_render_hint: Bool, _ browser_used: Bool, _ markdown: Optional<MarkdownResult>, _ extracted_data: Optional<GenericIntoRustString>, _ extraction_meta: Optional<ExtractionMeta>, _ screenshot_base64: Optional<GenericIntoRustString>, _ downloaded_document: Optional<DownloadedDocument>, _ browser: Optional<BrowserExtras>) {
        self.init(ptr: __swift_bridge__$ScrapeResult$new(status_code, { let rustString = final_url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = content_type.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = html.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), body_size, {metadata.isOwned = false; return metadata.ptr;}(), { let val = links; val.isOwned = false; return val.ptr }(), { let val = images; val.isOwned = false; return val.ptr }(), { let val = feeds; val.isOwned = false; return val.ptr }(), { let val = json_ld; val.isOwned = false; return val.ptr }(), is_allowed, crawl_delay.intoFfiRepr(), noindex_detected, nofollow_detected, { if let rustString = optionalStringIntoRustString(x_robots_tag) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), is_pdf, was_skipped, { if let rustString = optionalStringIntoRustString(detected_charset) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), auth_header_sent, { if let val = response_meta { val.isOwned = false; return val.ptr } else { return nil } }(), { let val = assets; val.isOwned = false; return val.ptr }(), js_render_hint, browser_used, { if let val = markdown { val.isOwned = false; return val.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(extracted_data) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let val = extraction_meta { val.isOwned = false; return val.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(screenshot_base64) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let val = downloaded_document { val.isOwned = false; return val.ptr } else { return nil } }(), { if let val = browser { val.isOwned = false; return val.ptr } else { return nil } }()))
    }
}
public class ScrapeResultRefMut: ScrapeResultRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ScrapeResultRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ScrapeResultRef {
    public func statusCode() -> UInt16 {
        __swift_bridge__$ScrapeResult$status_code(ptr)
    }

    public func finalUrl() -> RustString {
        RustString(ptr: __swift_bridge__$ScrapeResult$final_url(ptr))
    }

    public func contentType() -> RustString {
        RustString(ptr: __swift_bridge__$ScrapeResult$content_type(ptr))
    }

    public func html() -> RustString {
        RustString(ptr: __swift_bridge__$ScrapeResult$html(ptr))
    }

    public func bodySize() -> UInt {
        __swift_bridge__$ScrapeResult$body_size(ptr)
    }

    public func metadata() -> PageMetadata {
        PageMetadata(ptr: __swift_bridge__$ScrapeResult$metadata(ptr))
    }

    public func links() -> RustVec<LinkInfo> {
        RustVec(ptr: __swift_bridge__$ScrapeResult$links(ptr))
    }

    public func images() -> RustVec<ImageInfo> {
        RustVec(ptr: __swift_bridge__$ScrapeResult$images(ptr))
    }

    public func feeds() -> RustVec<FeedInfo> {
        RustVec(ptr: __swift_bridge__$ScrapeResult$feeds(ptr))
    }

    public func jsonLd() -> RustVec<JsonLdEntry> {
        RustVec(ptr: __swift_bridge__$ScrapeResult$json_ld(ptr))
    }

    public func isAllowed() -> Bool {
        __swift_bridge__$ScrapeResult$is_allowed(ptr)
    }

    public func crawlDelay() -> Optional<UInt64> {
        __swift_bridge__$ScrapeResult$crawl_delay(ptr).intoSwiftRepr()
    }

    public func noindexDetected() -> Bool {
        __swift_bridge__$ScrapeResult$noindex_detected(ptr)
    }

    public func nofollowDetected() -> Bool {
        __swift_bridge__$ScrapeResult$nofollow_detected(ptr)
    }

    public func xRobotsTag() -> Optional<RustString> {
        { let val = __swift_bridge__$ScrapeResult$x_robots_tag(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func isPdf() -> Bool {
        __swift_bridge__$ScrapeResult$is_pdf(ptr)
    }

    public func wasSkipped() -> Bool {
        __swift_bridge__$ScrapeResult$was_skipped(ptr)
    }

    public func detectedCharset() -> Optional<RustString> {
        { let val = __swift_bridge__$ScrapeResult$detected_charset(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func authHeaderSent() -> Bool {
        __swift_bridge__$ScrapeResult$auth_header_sent(ptr)
    }

    public func responseMeta() -> Optional<ResponseMeta> {
        { let val = __swift_bridge__$ScrapeResult$response_meta(ptr); if val != nil { return ResponseMeta(ptr: val!) } else { return nil } }()
    }

    public func assets() -> RustVec<DownloadedAsset> {
        RustVec(ptr: __swift_bridge__$ScrapeResult$assets(ptr))
    }

    public func jsRenderHint() -> Bool {
        __swift_bridge__$ScrapeResult$js_render_hint(ptr)
    }

    public func browserUsed() -> Bool {
        __swift_bridge__$ScrapeResult$browser_used(ptr)
    }

    public func markdown() -> Optional<MarkdownResult> {
        { let val = __swift_bridge__$ScrapeResult$markdown(ptr); if val != nil { return MarkdownResult(ptr: val!) } else { return nil } }()
    }

    public func extractedData() -> Optional<RustString> {
        { let val = __swift_bridge__$ScrapeResult$extracted_data(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func extractionMeta() -> Optional<ExtractionMeta> {
        { let val = __swift_bridge__$ScrapeResult$extraction_meta(ptr); if val != nil { return ExtractionMeta(ptr: val!) } else { return nil } }()
    }

    public func screenshotBase64() -> Optional<RustString> {
        { let val = __swift_bridge__$ScrapeResult$screenshot_base64(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func downloadedDocument() -> Optional<DownloadedDocument> {
        { let val = __swift_bridge__$ScrapeResult$downloaded_document(ptr); if val != nil { return DownloadedDocument(ptr: val!) } else { return nil } }()
    }

    public func browser() -> Optional<BrowserExtras> {
        { let val = __swift_bridge__$ScrapeResult$browser(ptr); if val != nil { return BrowserExtras(ptr: val!) } else { return nil } }()
    }
}
extension ScrapeResult: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ScrapeResult$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ScrapeResult$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ScrapeResult) {
        __swift_bridge__$Vec_ScrapeResult$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ScrapeResult$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ScrapeResult(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ScrapeResultRef> {
        let pointer = __swift_bridge__$Vec_ScrapeResult$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ScrapeResultRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ScrapeResultRefMut> {
        let pointer = __swift_bridge__$Vec_ScrapeResult$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ScrapeResultRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ScrapeResultRef> {
        UnsafePointer<ScrapeResultRef>(OpaquePointer(__swift_bridge__$Vec_ScrapeResult$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ScrapeResult$len(vecPtr)
    }
}


public class SitemapUrl: SitemapUrlRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$SitemapUrl$_free(ptr)
        }
    }
}
extension SitemapUrl {
    public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, _ lastmod: Optional<GenericIntoRustString>, _ changefreq: Optional<GenericIntoRustString>, _ priority: Optional<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$SitemapUrl$new({ let rustString = url.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { if let rustString = optionalStringIntoRustString(lastmod) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(changefreq) { rustString.isOwned = false; return rustString.ptr } else { return nil } }(), { if let rustString = optionalStringIntoRustString(priority) { rustString.isOwned = false; return rustString.ptr } else { return nil } }()))
    }
}
public class SitemapUrlRefMut: SitemapUrlRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class SitemapUrlRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension SitemapUrlRef {
    public func url() -> RustString {
        RustString(ptr: __swift_bridge__$SitemapUrl$url(ptr))
    }

    public func lastmod() -> Optional<RustString> {
        { let val = __swift_bridge__$SitemapUrl$lastmod(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func changefreq() -> Optional<RustString> {
        { let val = __swift_bridge__$SitemapUrl$changefreq(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }

    public func priority() -> Optional<RustString> {
        { let val = __swift_bridge__$SitemapUrl$priority(ptr); if val != nil { return RustString(ptr: val!) } else { return nil } }()
    }
}
extension SitemapUrl: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_SitemapUrl$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_SitemapUrl$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: SitemapUrl) {
        __swift_bridge__$Vec_SitemapUrl$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_SitemapUrl$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (SitemapUrl(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<SitemapUrlRef> {
        let pointer = __swift_bridge__$Vec_SitemapUrl$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return SitemapUrlRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<SitemapUrlRefMut> {
        let pointer = __swift_bridge__$Vec_SitemapUrl$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return SitemapUrlRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<SitemapUrlRef> {
        UnsafePointer<SitemapUrlRef>(OpaquePointer(__swift_bridge__$Vec_SitemapUrl$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_SitemapUrl$len(vecPtr)
    }
}


public class SsrfPolicy: SsrfPolicyRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$SsrfPolicy$_free(ptr)
        }
    }
}
extension SsrfPolicy {
    public convenience init<GenericIntoRustString: IntoRustString>(_ deny_private: Bool, _ allowlist: RustVec<HostMatcher>, _ max_redirects: UInt8, _ scheme_allowlist: RustVec<GenericIntoRustString>) {
        self.init(ptr: __swift_bridge__$SsrfPolicy$new(deny_private, { let val = allowlist; val.isOwned = false; return val.ptr }(), max_redirects, { let val = scheme_allowlist; val.isOwned = false; return val.ptr }()))
    }
}
public class SsrfPolicyRefMut: SsrfPolicyRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class SsrfPolicyRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension SsrfPolicyRef {
    public func denyPrivate() -> Bool {
        __swift_bridge__$SsrfPolicy$deny_private(ptr)
    }

    public func allowlist() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$SsrfPolicy$allowlist(ptr))
    }

    public func maxRedirects() -> UInt8 {
        __swift_bridge__$SsrfPolicy$max_redirects(ptr)
    }

    public func schemeAllowlist() -> RustVec<RustString> {
        RustVec(ptr: __swift_bridge__$SsrfPolicy$scheme_allowlist(ptr))
    }
}
extension SsrfPolicy: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_SsrfPolicy$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_SsrfPolicy$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: SsrfPolicy) {
        __swift_bridge__$Vec_SsrfPolicy$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_SsrfPolicy$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (SsrfPolicy(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<SsrfPolicyRef> {
        let pointer = __swift_bridge__$Vec_SsrfPolicy$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return SsrfPolicyRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<SsrfPolicyRefMut> {
        let pointer = __swift_bridge__$Vec_SsrfPolicy$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return SsrfPolicyRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<SsrfPolicyRef> {
        UnsafePointer<SsrfPolicyRef>(OpaquePointer(__swift_bridge__$Vec_SsrfPolicy$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_SsrfPolicy$len(vecPtr)
    }
}


public class AssetCategory: AssetCategoryRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$AssetCategory$_free(ptr)
        }
    }
}
public class AssetCategoryRefMut: AssetCategoryRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class AssetCategoryRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension AssetCategoryRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$AssetCategory$to_string(ptr))
    }
}
extension AssetCategory: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_AssetCategory$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_AssetCategory$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: AssetCategory) {
        __swift_bridge__$Vec_AssetCategory$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_AssetCategory$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (AssetCategory(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<AssetCategoryRef> {
        let pointer = __swift_bridge__$Vec_AssetCategory$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return AssetCategoryRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<AssetCategoryRefMut> {
        let pointer = __swift_bridge__$Vec_AssetCategory$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return AssetCategoryRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<AssetCategoryRef> {
        UnsafePointer<AssetCategoryRef>(OpaquePointer(__swift_bridge__$Vec_AssetCategory$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_AssetCategory$len(vecPtr)
    }
}


public class AuthConfig: AuthConfigRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$AuthConfig$_free(ptr)
        }
    }
}
public class AuthConfigRefMut: AuthConfigRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class AuthConfigRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension AuthConfigRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$AuthConfig$to_string(ptr))
    }
}
extension AuthConfig: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_AuthConfig$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_AuthConfig$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: AuthConfig) {
        __swift_bridge__$Vec_AuthConfig$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_AuthConfig$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (AuthConfig(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<AuthConfigRef> {
        let pointer = __swift_bridge__$Vec_AuthConfig$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return AuthConfigRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<AuthConfigRefMut> {
        let pointer = __swift_bridge__$Vec_AuthConfig$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return AuthConfigRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<AuthConfigRef> {
        UnsafePointer<AuthConfigRef>(OpaquePointer(__swift_bridge__$Vec_AuthConfig$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_AuthConfig$len(vecPtr)
    }
}


public class BrowserBackend: BrowserBackendRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BrowserBackend$_free(ptr)
        }
    }
}
public class BrowserBackendRefMut: BrowserBackendRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BrowserBackendRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BrowserBackendRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$BrowserBackend$to_string(ptr))
    }
}
extension BrowserBackend: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BrowserBackend$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BrowserBackend$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BrowserBackend) {
        __swift_bridge__$Vec_BrowserBackend$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BrowserBackend$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BrowserBackend(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserBackendRef> {
        let pointer = __swift_bridge__$Vec_BrowserBackend$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserBackendRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserBackendRefMut> {
        let pointer = __swift_bridge__$Vec_BrowserBackend$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserBackendRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BrowserBackendRef> {
        UnsafePointer<BrowserBackendRef>(OpaquePointer(__swift_bridge__$Vec_BrowserBackend$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BrowserBackend$len(vecPtr)
    }
}


public class BrowserMode: BrowserModeRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BrowserMode$_free(ptr)
        }
    }
}
public class BrowserModeRefMut: BrowserModeRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BrowserModeRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BrowserModeRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$BrowserMode$to_string(ptr))
    }
}
extension BrowserMode: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BrowserMode$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BrowserMode$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BrowserMode) {
        __swift_bridge__$Vec_BrowserMode$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BrowserMode$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BrowserMode(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserModeRef> {
        let pointer = __swift_bridge__$Vec_BrowserMode$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserModeRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserModeRefMut> {
        let pointer = __swift_bridge__$Vec_BrowserMode$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserModeRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BrowserModeRef> {
        UnsafePointer<BrowserModeRef>(OpaquePointer(__swift_bridge__$Vec_BrowserMode$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BrowserMode$len(vecPtr)
    }
}


public class BrowserWait: BrowserWaitRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$BrowserWait$_free(ptr)
        }
    }
}
public class BrowserWaitRefMut: BrowserWaitRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class BrowserWaitRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension BrowserWaitRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$BrowserWait$to_string(ptr))
    }
}
extension BrowserWait: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_BrowserWait$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_BrowserWait$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: BrowserWait) {
        __swift_bridge__$Vec_BrowserWait$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_BrowserWait$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (BrowserWait(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserWaitRef> {
        let pointer = __swift_bridge__$Vec_BrowserWait$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserWaitRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<BrowserWaitRefMut> {
        let pointer = __swift_bridge__$Vec_BrowserWait$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return BrowserWaitRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<BrowserWaitRef> {
        UnsafePointer<BrowserWaitRef>(OpaquePointer(__swift_bridge__$Vec_BrowserWait$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_BrowserWait$len(vecPtr)
    }
}


public class ContentFilterKind: ContentFilterKindRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ContentFilterKind$_free(ptr)
        }
    }
}
public class ContentFilterKindRefMut: ContentFilterKindRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ContentFilterKindRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ContentFilterKindRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$ContentFilterKind$to_string(ptr))
    }
}
extension ContentFilterKind: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ContentFilterKind$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ContentFilterKind$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ContentFilterKind) {
        __swift_bridge__$Vec_ContentFilterKind$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ContentFilterKind$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ContentFilterKind(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ContentFilterKindRef> {
        let pointer = __swift_bridge__$Vec_ContentFilterKind$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ContentFilterKindRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ContentFilterKindRefMut> {
        let pointer = __swift_bridge__$Vec_ContentFilterKind$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ContentFilterKindRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ContentFilterKindRef> {
        UnsafePointer<ContentFilterKindRef>(OpaquePointer(__swift_bridge__$Vec_ContentFilterKind$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ContentFilterKind$len(vecPtr)
    }
}


public class CrawlEvent: CrawlEventRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlEvent$_free(ptr)
        }
    }
}
public class CrawlEventRefMut: CrawlEventRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CrawlEventRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlEventRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlEvent$to_string(ptr))
    }
}
extension CrawlEvent: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlEvent$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlEvent$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlEvent) {
        __swift_bridge__$Vec_CrawlEvent$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlEvent$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlEvent(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlEventRef> {
        let pointer = __swift_bridge__$Vec_CrawlEvent$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlEventRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlEventRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlEvent$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlEventRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlEventRef> {
        UnsafePointer<CrawlEventRef>(OpaquePointer(__swift_bridge__$Vec_CrawlEvent$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlEvent$len(vecPtr)
    }
}


public class CrawlStrategyKind: CrawlStrategyKindRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlStrategyKind$_free(ptr)
        }
    }
}
public class CrawlStrategyKindRefMut: CrawlStrategyKindRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CrawlStrategyKindRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlStrategyKindRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$CrawlStrategyKind$to_string(ptr))
    }
}
extension CrawlStrategyKind: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlStrategyKind$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlStrategyKind$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlStrategyKind) {
        __swift_bridge__$Vec_CrawlStrategyKind$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlStrategyKind$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlStrategyKind(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlStrategyKindRef> {
        let pointer = __swift_bridge__$Vec_CrawlStrategyKind$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlStrategyKindRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlStrategyKindRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlStrategyKind$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlStrategyKindRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlStrategyKindRef> {
        UnsafePointer<CrawlStrategyKindRef>(OpaquePointer(__swift_bridge__$Vec_CrawlStrategyKind$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlStrategyKind$len(vecPtr)
    }
}


public class DocumentContentEncoding: DocumentContentEncodingRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$DocumentContentEncoding$_free(ptr)
        }
    }
}
public class DocumentContentEncodingRefMut: DocumentContentEncodingRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class DocumentContentEncodingRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension DocumentContentEncodingRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$DocumentContentEncoding$to_string(ptr))
    }
}
extension DocumentContentEncoding: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_DocumentContentEncoding$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_DocumentContentEncoding$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: DocumentContentEncoding) {
        __swift_bridge__$Vec_DocumentContentEncoding$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_DocumentContentEncoding$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (DocumentContentEncoding(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<DocumentContentEncodingRef> {
        let pointer = __swift_bridge__$Vec_DocumentContentEncoding$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return DocumentContentEncodingRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<DocumentContentEncodingRefMut> {
        let pointer = __swift_bridge__$Vec_DocumentContentEncoding$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return DocumentContentEncodingRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<DocumentContentEncodingRef> {
        UnsafePointer<DocumentContentEncodingRef>(OpaquePointer(__swift_bridge__$Vec_DocumentContentEncoding$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_DocumentContentEncoding$len(vecPtr)
    }
}


public class FeedType: FeedTypeRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$FeedType$_free(ptr)
        }
    }
}
public class FeedTypeRefMut: FeedTypeRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class FeedTypeRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension FeedTypeRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$FeedType$to_string(ptr))
    }
}
extension FeedType: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_FeedType$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_FeedType$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: FeedType) {
        __swift_bridge__$Vec_FeedType$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_FeedType$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (FeedType(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<FeedTypeRef> {
        let pointer = __swift_bridge__$Vec_FeedType$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return FeedTypeRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<FeedTypeRefMut> {
        let pointer = __swift_bridge__$Vec_FeedType$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return FeedTypeRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<FeedTypeRef> {
        UnsafePointer<FeedTypeRef>(OpaquePointer(__swift_bridge__$Vec_FeedType$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_FeedType$len(vecPtr)
    }
}


public class HostMatcher: HostMatcherRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$HostMatcher$_free(ptr)
        }
    }
}
public class HostMatcherRefMut: HostMatcherRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class HostMatcherRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension HostMatcherRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$HostMatcher$to_string(ptr))
    }
}
extension HostMatcher: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_HostMatcher$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_HostMatcher$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: HostMatcher) {
        __swift_bridge__$Vec_HostMatcher$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_HostMatcher$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (HostMatcher(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<HostMatcherRef> {
        let pointer = __swift_bridge__$Vec_HostMatcher$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return HostMatcherRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<HostMatcherRefMut> {
        let pointer = __swift_bridge__$Vec_HostMatcher$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return HostMatcherRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<HostMatcherRef> {
        UnsafePointer<HostMatcherRef>(OpaquePointer(__swift_bridge__$Vec_HostMatcher$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_HostMatcher$len(vecPtr)
    }
}


public class ImageSource: ImageSourceRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ImageSource$_free(ptr)
        }
    }
}
public class ImageSourceRefMut: ImageSourceRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ImageSourceRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ImageSourceRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$ImageSource$to_string(ptr))
    }
}
extension ImageSource: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ImageSource$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ImageSource$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ImageSource) {
        __swift_bridge__$Vec_ImageSource$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ImageSource$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ImageSource(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ImageSourceRef> {
        let pointer = __swift_bridge__$Vec_ImageSource$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ImageSourceRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ImageSourceRefMut> {
        let pointer = __swift_bridge__$Vec_ImageSource$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ImageSourceRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ImageSourceRef> {
        UnsafePointer<ImageSourceRef>(OpaquePointer(__swift_bridge__$Vec_ImageSource$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ImageSource$len(vecPtr)
    }
}


public class LinkType: LinkTypeRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$LinkType$_free(ptr)
        }
    }
}
public class LinkTypeRefMut: LinkTypeRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class LinkTypeRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension LinkTypeRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$LinkType$to_string(ptr))
    }
}
extension LinkType: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_LinkType$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_LinkType$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: LinkType) {
        __swift_bridge__$Vec_LinkType$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_LinkType$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (LinkType(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<LinkTypeRef> {
        let pointer = __swift_bridge__$Vec_LinkType$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return LinkTypeRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<LinkTypeRefMut> {
        let pointer = __swift_bridge__$Vec_LinkType$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return LinkTypeRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<LinkTypeRef> {
        UnsafePointer<LinkTypeRef>(OpaquePointer(__swift_bridge__$Vec_LinkType$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_LinkType$len(vecPtr)
    }
}


public class PageAction: PageActionRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$PageAction$_free(ptr)
        }
    }
}
public class PageActionRefMut: PageActionRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class PageActionRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension PageActionRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$PageAction$to_string(ptr))
    }
}
extension PageAction: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_PageAction$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_PageAction$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: PageAction) {
        __swift_bridge__$Vec_PageAction$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_PageAction$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (PageAction(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PageActionRef> {
        let pointer = __swift_bridge__$Vec_PageAction$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PageActionRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PageActionRefMut> {
        let pointer = __swift_bridge__$Vec_PageAction$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PageActionRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<PageActionRef> {
        UnsafePointer<PageActionRef>(OpaquePointer(__swift_bridge__$Vec_PageAction$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_PageAction$len(vecPtr)
    }
}


public class ScrollDirection: ScrollDirectionRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$ScrollDirection$_free(ptr)
        }
    }
}
public class ScrollDirectionRefMut: ScrollDirectionRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class ScrollDirectionRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension ScrollDirectionRef {
    public func to_string() -> RustString {
        RustString(ptr: __swift_bridge__$ScrollDirection$to_string(ptr))
    }
}
extension ScrollDirection: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_ScrollDirection$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_ScrollDirection$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: ScrollDirection) {
        __swift_bridge__$Vec_ScrollDirection$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_ScrollDirection$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (ScrollDirection(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ScrollDirectionRef> {
        let pointer = __swift_bridge__$Vec_ScrollDirection$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ScrollDirectionRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<ScrollDirectionRefMut> {
        let pointer = __swift_bridge__$Vec_ScrollDirection$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return ScrollDirectionRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<ScrollDirectionRef> {
        UnsafePointer<ScrollDirectionRef>(OpaquePointer(__swift_bridge__$Vec_ScrollDirection$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_ScrollDirection$len(vecPtr)
    }
}


public class CrawlEngineHandle: CrawlEngineHandleRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlEngineHandle$_free(ptr)
        }
    }
}
public class CrawlEngineHandleRefMut: CrawlEngineHandleRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class CrawlEngineHandleRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlEngineHandle: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlEngineHandle$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlEngineHandle$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlEngineHandle) {
        __swift_bridge__$Vec_CrawlEngineHandle$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandle$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlEngineHandle(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlEngineHandleRef> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandle$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlEngineHandleRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlEngineHandleRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandle$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlEngineHandleRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlEngineHandleRef> {
        UnsafePointer<CrawlEngineHandleRef>(OpaquePointer(__swift_bridge__$Vec_CrawlEngineHandle$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlEngineHandle$len(vecPtr)
    }
}


public class CrawlEngineHandleBatchCrawlStreamStreamHandle: CrawlEngineHandleBatchCrawlStreamStreamHandleRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlEngineHandleBatchCrawlStreamStreamHandle$_free(ptr)
        }
    }
}
public class CrawlEngineHandleBatchCrawlStreamStreamHandleRefMut: CrawlEngineHandleBatchCrawlStreamStreamHandleRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
extension CrawlEngineHandleBatchCrawlStreamStreamHandleRefMut {
    public func next() throws -> RustString {
        try { let val = __swift_bridge__$CrawlEngineHandleBatchCrawlStreamStreamHandle$next(ptr); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
    }
}
public class CrawlEngineHandleBatchCrawlStreamStreamHandleRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlEngineHandleBatchCrawlStreamStreamHandle: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlEngineHandleBatchCrawlStreamStreamHandle$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlEngineHandleBatchCrawlStreamStreamHandle$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlEngineHandleBatchCrawlStreamStreamHandle) {
        __swift_bridge__$Vec_CrawlEngineHandleBatchCrawlStreamStreamHandle$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandleBatchCrawlStreamStreamHandle$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlEngineHandleBatchCrawlStreamStreamHandle(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlEngineHandleBatchCrawlStreamStreamHandleRef> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandleBatchCrawlStreamStreamHandle$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlEngineHandleBatchCrawlStreamStreamHandleRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlEngineHandleBatchCrawlStreamStreamHandleRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandleBatchCrawlStreamStreamHandle$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlEngineHandleBatchCrawlStreamStreamHandleRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlEngineHandleBatchCrawlStreamStreamHandleRef> {
        UnsafePointer<CrawlEngineHandleBatchCrawlStreamStreamHandleRef>(OpaquePointer(__swift_bridge__$Vec_CrawlEngineHandleBatchCrawlStreamStreamHandle$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlEngineHandleBatchCrawlStreamStreamHandle$len(vecPtr)
    }
}


public class CrawlEngineHandleCrawlStreamStreamHandle: CrawlEngineHandleCrawlStreamStreamHandleRefMut {
    public var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$CrawlEngineHandleCrawlStreamStreamHandle$_free(ptr)
        }
    }
}
public class CrawlEngineHandleCrawlStreamStreamHandleRefMut: CrawlEngineHandleCrawlStreamStreamHandleRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
extension CrawlEngineHandleCrawlStreamStreamHandleRefMut {
    public func next() throws -> RustString {
        try { let val = __swift_bridge__$CrawlEngineHandleCrawlStreamStreamHandle$next(ptr); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
    }
}
public class CrawlEngineHandleCrawlStreamStreamHandleRef {
    public var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension CrawlEngineHandleCrawlStreamStreamHandle: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_CrawlEngineHandleCrawlStreamStreamHandle$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_CrawlEngineHandleCrawlStreamStreamHandle$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: CrawlEngineHandleCrawlStreamStreamHandle) {
        __swift_bridge__$Vec_CrawlEngineHandleCrawlStreamStreamHandle$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandleCrawlStreamStreamHandle$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (CrawlEngineHandleCrawlStreamStreamHandle(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlEngineHandleCrawlStreamStreamHandleRef> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandleCrawlStreamStreamHandle$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlEngineHandleCrawlStreamStreamHandleRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<CrawlEngineHandleCrawlStreamStreamHandleRefMut> {
        let pointer = __swift_bridge__$Vec_CrawlEngineHandleCrawlStreamStreamHandle$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return CrawlEngineHandleCrawlStreamStreamHandleRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<CrawlEngineHandleCrawlStreamStreamHandleRef> {
        UnsafePointer<CrawlEngineHandleCrawlStreamStreamHandleRef>(OpaquePointer(__swift_bridge__$Vec_CrawlEngineHandleCrawlStreamStreamHandle$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_CrawlEngineHandleCrawlStreamStreamHandle$len(vecPtr)
    }
}
