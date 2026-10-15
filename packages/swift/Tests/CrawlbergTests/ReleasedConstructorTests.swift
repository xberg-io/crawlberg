import Crawlberg
import Foundation
import RustBridge
import XCTest

// ~keep Hand-written calls from v1.10.3: 23 memberwise initializers and 26 Decoder initializers.
// ~keep Keep these released call shapes unchanged when regenerating bindings; update them only for a new released baseline.
final class ReleasedConstructorTests: XCTestCase {
  func testReleasedPayloadDecodersPreserveTheirValues() throws {
    let decoder = JSONDecoder()
    let auth = try decoder.decode(Crawlberg.AuthConfig.self,
      from: Data(#"{"type":"basic","username":"user-sentinel","password":"password-sentinel"}"#.utf8))
    XCTAssertEqual(auth, .basic(username: "user-sentinel", password: "password-sentinel"))
    let host = try decoder.decode(Crawlberg.HostMatcher.self,
      from: Data(#"{"type":"exact","value":"host.example.test"}"#.utf8))
    XCTAssertEqual(host, .exact(value: "host.example.test"))
    let action = try decoder.decode(Crawlberg.PageAction.self,
      from: Data(##"{"type":"click","selector":"#sentinel"}"##.utf8))
    XCTAssertEqual(action, .click(selector: "#sentinel"))
    XCTAssertThrowsError(try decoder.decode(Crawlberg.AuthConfig.self, from: Data(#"{"type":"unknown"}"#.utf8)))
  }

  func testReleasedContentFunctionReferenceValues() {
    // ~keep The released 13-argument reference needs an exact overload after new defaulted fields are added.
    let initializer:
    (String, String, Bool, Bool, [String], [String], [String], Bool, UInt?, Bool, UInt, Bool, Bool) -> Crawlberg.ContentConfig =
    Crawlberg.ContentConfig.init(outputFormat:preprocessingPreset:removeNavigation:removeForms:stripTags:preserveTags:excludeSelectors:skipImages:maxDepth:wrap:wrapWidth:includeDocumentStructure:extractMetadata:)
    let value = initializer("html", "minimal", false, true, ["script"], ["p"], [".ad"], true, 7, true, 37, false, true)
    XCTAssertEqual(value.outputFormat, "html")
    XCTAssertEqual(value.preprocessingPreset, "minimal")
    XCTAssertEqual(value.removeNavigation, false)
    XCTAssertEqual(value.removeForms, true)
    XCTAssertEqual(value.stripTags, ["script"])
    XCTAssertEqual(value.preserveTags, ["p"])
    XCTAssertEqual(value.excludeSelectors, [".ad"])
    XCTAssertEqual(value.maxDepth, 7)
    XCTAssertEqual(value.wrapWidth, 37)
    XCTAssertEqual(value.includeDocumentStructure, false)
    XCTAssertEqual(value.extractMetadata, true)
  }

  func testReleasedLinkFunctionReferenceValues() {
    // ~keep This exact released function reference must survive defaulted-parameter additions.
    let releasedLinkInitializer: (String, String, Crawlberg.LinkType, String?, Bool) -> Crawlberg.LinkInfo =
    Crawlberg.LinkInfo.init(url:text:linkType:rel:nofollow:)
    let value = releasedLinkInitializer("https://url.example.test", "text-sentinel", .external, "rel-sentinel", false)
    XCTAssertEqual(value.url, "https://url.example.test")
    XCTAssertEqual(value.text, "text-sentinel")
    XCTAssertEqual(value.rel, "rel-sentinel")
    XCTAssertEqual(value.nofollow, false)
  }

  func testReleasedFacadeArticleMetadataValues() throws {
    let value = Crawlberg.ArticleMetadata(publishedTime: nil, modifiedTime: nil, author: nil, section: nil, tags: [])
    XCTAssertEqual(value.publishedTime, nil)
    XCTAssertEqual(value.modifiedTime, nil)
    XCTAssertEqual(value.author, nil)
    XCTAssertEqual(value.section, nil)
    XCTAssertEqual(value.tags, [])
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.ArticleMetadata.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeBatchCrawlStreamRequestValues() throws {
    let value = Crawlberg.BatchCrawlStreamRequest(urls: [])
    XCTAssertEqual(value.urls, [])
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.BatchCrawlStreamRequest.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeCitationReferenceValues() throws {
    let value = Crawlberg.CitationReference(index: 0, url: "url-sentinel", text: "text-sentinel")
    XCTAssertEqual(value.index, 0)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.text, "text-sentinel")
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.CitationReference.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeCitationResultValues() throws {
    let value = Crawlberg.CitationResult(content: "content-sentinel", references: [])
    XCTAssertEqual(value.content, "content-sentinel")
    XCTAssertEqual(value.references, [])
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.CitationResult.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeContentConfigValues() throws {
    let value = Crawlberg.ContentConfig(outputFormat: "outputFormat-sentinel", preprocessingPreset: "preprocessingPreset-sentinel", removeNavigation: true, removeForms: true, stripTags: [], preserveTags: [], excludeSelectors: ["noscript"], skipImages: false, maxDepth: nil, wrap: false, wrapWidth: 80, includeDocumentStructure: true, extractMetadata: true)
    XCTAssertEqual(value.outputFormat, "outputFormat-sentinel")
    XCTAssertEqual(value.preprocessingPreset, "preprocessingPreset-sentinel")
    XCTAssertEqual(value.removeNavigation, true)
    XCTAssertEqual(value.removeForms, true)
    XCTAssertEqual(value.stripTags, [])
    XCTAssertEqual(value.preserveTags, [])
    XCTAssertEqual(value.excludeSelectors, ["noscript"])
    XCTAssertEqual(value.skipImages, false)
    XCTAssertEqual(value.maxDepth, nil)
    XCTAssertEqual(value.wrap, false)
    XCTAssertEqual(value.wrapWidth, 80)
    XCTAssertEqual(value.includeDocumentStructure, true)
    XCTAssertEqual(value.extractMetadata, true)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.ContentConfig.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeCookieInfoValues() throws {
    let value = Crawlberg.CookieInfo(name: "name-sentinel", value: "value-sentinel", domain: nil, path: nil)
    XCTAssertEqual(value.name, "name-sentinel")
    XCTAssertEqual(value.value, "value-sentinel")
    XCTAssertEqual(value.domain, nil)
    XCTAssertEqual(value.path, nil)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.CookieInfo.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeCrawlStreamRequestValues() throws {
    let value = Crawlberg.CrawlStreamRequest(url: "url-sentinel")
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.CrawlStreamRequest.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeDownloadedAssetValues() throws {
    let value = Crawlberg.DownloadedAsset(url: "url-sentinel", contentHash: "contentHash-sentinel", mimeType: nil, size: 0, assetCategory: .document, htmlTag: nil)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.contentHash, "contentHash-sentinel")
    XCTAssertEqual(value.mimeType, nil)
    XCTAssertEqual(value.size, 0)
    XCTAssertEqual(value.assetCategory, .document)
    XCTAssertEqual(value.htmlTag, nil)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.DownloadedAsset.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeDownloadedDocumentValues() throws {
    let value = Crawlberg.DownloadedDocument(url: "url-sentinel", mimeType: "mimeType-sentinel", size: 0, filename: nil, contentHash: "contentHash-sentinel", headers: [:], truncated: false, contentPath: nil, contentBase64: nil)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.mimeType, "mimeType-sentinel")
    XCTAssertEqual(value.size, 0)
    XCTAssertEqual(value.filename, nil)
    XCTAssertEqual(value.contentHash, "contentHash-sentinel")
    XCTAssertEqual(value.headers, [:])
    XCTAssertEqual(value.truncated, false)
    XCTAssertEqual(value.contentPath, nil)
    XCTAssertEqual(value.contentBase64, nil)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.DownloadedDocument.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeExtractionMetaValues() throws {
    let value = Crawlberg.ExtractionMeta(cost: nil, promptTokens: nil, completionTokens: nil, model: nil, chunksProcessed: 0)
    XCTAssertEqual(value.cost, nil)
    XCTAssertEqual(value.promptTokens, nil)
    XCTAssertEqual(value.completionTokens, nil)
    XCTAssertEqual(value.model, nil)
    XCTAssertEqual(value.chunksProcessed, 0)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.ExtractionMeta.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeFaviconInfoValues() throws {
    let value = Crawlberg.FaviconInfo(url: "url-sentinel", rel: "rel-sentinel", sizes: nil, mimeType: nil)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.rel, "rel-sentinel")
    XCTAssertEqual(value.sizes, nil)
    XCTAssertEqual(value.mimeType, nil)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.FaviconInfo.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeFeedInfoValues() throws {
    let value = Crawlberg.FeedInfo(url: "url-sentinel", title: nil, feedType: .rss)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.title, nil)
    XCTAssertEqual(value.feedType, .rss)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.FeedInfo.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeHeadingInfoValues() throws {
    let value = Crawlberg.HeadingInfo(level: 1, text: "text-sentinel")
    XCTAssertEqual(value.level, 1)
    XCTAssertEqual(value.text, "text-sentinel")
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.HeadingInfo.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeHreflangEntryValues() throws {
    let value = Crawlberg.HreflangEntry(lang: "lang-sentinel", url: "url-sentinel")
    XCTAssertEqual(value.lang, "lang-sentinel")
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.HreflangEntry.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeImageInfoValues() throws {
    let value = Crawlberg.ImageInfo(url: "url-sentinel", alt: nil, width: nil, height: nil, source: .img)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.alt, nil)
    XCTAssertEqual(value.width, nil)
    XCTAssertEqual(value.height, nil)
    XCTAssertEqual(value.source, .img)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.ImageInfo.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeJsonLdEntryValues() throws {
    let value = Crawlberg.JsonLdEntry(schemaType: "schemaType-sentinel", name: nil, raw: "raw-sentinel")
    XCTAssertEqual(value.schemaType, "schemaType-sentinel")
    XCTAssertEqual(value.name, nil)
    XCTAssertEqual(value.raw, "raw-sentinel")
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.JsonLdEntry.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeLinkInfoValues() throws {
    let value = Crawlberg.LinkInfo(url: "url-sentinel", text: "text-sentinel", linkType: .external, rel: nil, nofollow: false)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.text, "text-sentinel")
    XCTAssertEqual(value.linkType, .external)
    XCTAssertEqual(value.rel, nil)
    XCTAssertEqual(value.nofollow, false)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.LinkInfo.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeMapResultValues() throws {
    let value = Crawlberg.MapResult(urls: [])
    XCTAssertEqual(value.urls, [])
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.MapResult.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadePageMetadataValues() throws {
    let value = Crawlberg.PageMetadata(title: nil, description: nil, canonicalUrl: nil, keywords: nil, author: nil, viewport: nil, themeColor: nil, generator: nil, robots: nil, htmlLang: nil, htmlDir: nil, ogTitle: nil, ogType: nil, ogImage: nil, ogDescription: nil, ogUrl: nil, ogSiteName: nil, ogLocale: nil, ogVideo: nil, ogAudio: nil, ogLocaleAlternates: nil, twitterCard: nil, twitterTitle: nil, twitterDescription: nil, twitterImage: nil, twitterSite: nil, twitterCreator: nil, dcTitle: nil, dcCreator: nil, dcSubject: nil, dcDescription: nil, dcPublisher: nil, dcDate: nil, dcType: nil, dcFormat: nil, dcIdentifier: nil, dcLanguage: nil, dcRights: nil, article: nil, hreflangs: nil, favicons: nil, headings: nil, wordCount: nil)
    XCTAssertEqual(value.title, nil)
    XCTAssertEqual(value.description, nil)
    XCTAssertEqual(value.wordCount, nil)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.PageMetadata.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeProxyConfigValues() throws {
    let value = Crawlberg.ProxyConfig(url: "url-sentinel", username: nil, password: nil)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.username, nil)
    XCTAssertEqual(value.password, nil)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.ProxyConfig.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeResponseMetaValues() throws {
    let value = Crawlberg.ResponseMeta(etag: nil, lastModified: nil, cacheControl: nil, server: nil, xPoweredBy: nil, contentLanguage: nil, contentEncoding: nil)
    XCTAssertEqual(value.etag, nil)
    XCTAssertEqual(value.lastModified, nil)
    XCTAssertEqual(value.cacheControl, nil)
    XCTAssertEqual(value.server, nil)
    XCTAssertEqual(value.xPoweredBy, nil)
    XCTAssertEqual(value.contentLanguage, nil)
    XCTAssertEqual(value.contentEncoding, nil)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.ResponseMeta.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeSitemapUrlValues() throws {
    let value = Crawlberg.SitemapUrl(url: "url-sentinel", lastmod: nil, changefreq: nil, priority: nil)
    XCTAssertEqual(value.url, "url-sentinel")
    XCTAssertEqual(value.lastmod, nil)
    XCTAssertEqual(value.changefreq, nil)
    XCTAssertEqual(value.priority, nil)
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.SitemapUrl.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedFacadeSsrfPolicyValues() throws {
    let value = Crawlberg.SsrfPolicy(denyPrivate: true, allowlist: [], denylist: [], maxRedirects: 5, schemeAllowlist: ["http", "https"])
    XCTAssertEqual(value.denyPrivate, true)
    XCTAssertEqual(value.allowlist, [])
    XCTAssertEqual(value.denylist, [])
    XCTAssertEqual(value.maxRedirects, 5)
    XCTAssertEqual(value.schemeAllowlist, ["http", "https"])
    XCTAssertEqual(try JSONDecoder().decode(Crawlberg.SsrfPolicy.self, from: JSONEncoder().encode(value)), value)
  }

  func testReleasedDefaultsKeepTheirValues() {
    let content = Crawlberg.ContentConfig()
    XCTAssertEqual(content.outputFormat, "markdown")
    XCTAssertEqual(content.wrapWidth, 80)
    XCTAssertEqual(content.includeDocumentStructure, true)
    XCTAssertEqual(content.extractMetadata, true)
    let link = Crawlberg.LinkInfo(url: "https://example.test", text: "link", linkType: .external, nofollow: false)
    XCTAssertEqual(link.rel, nil)
    let policy = Crawlberg.SsrfPolicy()
    XCTAssertEqual(policy.denyPrivate, true)
    XCTAssertEqual(policy.maxRedirects, 5)
    XCTAssertEqual(policy.schemeAllowlist, ["http", "https"])
  }
}

// ~keep Swift checks this function's body even when no runtime Decoder is supplied. These are
// ~keep the actual public init(from:) calls, including the three payload enums, rather than substitute types.
private func releasedDecoderCalls(_ decoder: any Decoder) throws {
  _ = try Crawlberg.ArticleMetadata(from: decoder)
  _ = try Crawlberg.BatchCrawlStreamRequest(from: decoder)
  _ = try Crawlberg.CitationReference(from: decoder)
  _ = try Crawlberg.CitationResult(from: decoder)
  _ = try Crawlberg.ContentConfig(from: decoder)
  _ = try Crawlberg.CookieInfo(from: decoder)
  _ = try Crawlberg.CrawlStreamRequest(from: decoder)
  _ = try Crawlberg.DownloadedAsset(from: decoder)
  _ = try Crawlberg.DownloadedDocument(from: decoder)
  _ = try Crawlberg.ExtractionMeta(from: decoder)
  _ = try Crawlberg.FaviconInfo(from: decoder)
  _ = try Crawlberg.FeedInfo(from: decoder)
  _ = try Crawlberg.HeadingInfo(from: decoder)
  _ = try Crawlberg.HreflangEntry(from: decoder)
  _ = try Crawlberg.ImageInfo(from: decoder)
  _ = try Crawlberg.JsonLdEntry(from: decoder)
  _ = try Crawlberg.LinkInfo(from: decoder)
  _ = try Crawlberg.MapResult(from: decoder)
  _ = try Crawlberg.PageMetadata(from: decoder)
  _ = try Crawlberg.ProxyConfig(from: decoder)
  _ = try Crawlberg.ResponseMeta(from: decoder)
  _ = try Crawlberg.SitemapUrl(from: decoder)
  _ = try Crawlberg.SsrfPolicy(from: decoder)
  _ = try Crawlberg.AuthConfig(from: decoder)
  _ = try Crawlberg.HostMatcher(from: decoder)
  _ = try Crawlberg.PageAction(from: decoder)
}

// ~keep Released value constructors exercise real Rust allocations; raw-pointer ownership wrappers are excluded.
final class ReleasedConstructorTestsNative: XCTestCase {
  func testReleasedEmptyRustStringValues() {
    let value = RustBridge.RustString()
    XCTAssertEqual(value.toString(), "")
  }

  func testReleasedRustStringValues() {
    let value = RustBridge.RustString("string-sentinel")
    XCTAssertEqual(value.toString(), "string-sentinel")
  }

  func testReleasedRustVectorValues() {
    let value = RustBridge.RustVec<RustBridge.RustString>()
    value.push(value: RustBridge.RustString("vector-sentinel"))
    XCTAssertEqual(value.len(), 1)
    XCTAssertEqual(value.get(index: 0)?.as_str().toString(), "vector-sentinel")
  }

  func testReleasedCallbackSupportValue() {
    let value = RustBridge.__private__UncheckedSendable(37)
    XCTAssertEqual(value.value, 37)
  }

  func testReleasedNativeActionResultValues() throws {
    let value = RustBridge.ActionResult(
      1,
      RustBridge.RustString("action_type-sentinel"),
      false,
      RustBridge.RustString("data-optional"),
      RustBridge.RustString("error-optional"))
    XCTAssertEqual(value.actionIndex(), 1)
    XCTAssertEqual(value.actionType().toString(), "action_type-sentinel")
    XCTAssertEqual(value.success(), false)
    XCTAssertEqual(value.data()?.toString(), "\"data-optional\"")
    XCTAssertEqual(value.error()?.toString(), "error-optional")
  }

  func testReleasedNativeArticleMetadataValues() throws {
    let value = RustBridge.ArticleMetadata(
      RustBridge.RustString("published_time-optional"),
      RustBridge.RustString("modified_time-optional"),
      RustBridge.RustString("author-optional"),
      RustBridge.RustString("section-optional"),
      RustBridge.RustVec<RustBridge.RustString>())
    XCTAssertEqual(value.publishedTime()?.toString(), "published_time-optional")
    XCTAssertEqual(value.modifiedTime()?.toString(), "modified_time-optional")
    XCTAssertEqual(value.author()?.toString(), "author-optional")
    XCTAssertEqual(value.section()?.toString(), "section-optional")
    XCTAssertEqual(value.tags().len(), 0)
  }

  func testReleasedNativeBatchCrawlResultValues() throws {
    let value = RustBridge.BatchCrawlResult(
      RustBridge.RustString("url-sentinel"),
      nil,
      RustBridge.RustString("error-optional"))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.result() == nil, true)
    XCTAssertEqual(value.error()?.toString(), "error-optional")
  }

  func testReleasedNativeBatchCrawlResultsValues() throws {
    let value = RustBridge.BatchCrawlResults(
      RustBridge.RustVec<RustBridge.BatchCrawlResult>(),
      2,
      3,
      4)
    XCTAssertEqual(value.results().len(), 0)
    XCTAssertEqual(value.totalCount(), 2)
    XCTAssertEqual(value.completedCount(), 3)
    XCTAssertEqual(value.failedCount(), 4)
  }

  func testReleasedNativeBatchCrawlStreamRequestValues() throws {
    let value = RustBridge.BatchCrawlStreamRequest(
      RustBridge.RustVec<RustBridge.RustString>())
    XCTAssertEqual(value.urls().len(), 0)
  }

  func testReleasedNativeBatchScrapeResultValues() throws {
    let value = RustBridge.BatchScrapeResult(
      RustBridge.RustString("url-sentinel"),
      nil,
      RustBridge.RustString("error-optional"))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.result() == nil, true)
    XCTAssertEqual(value.error()?.toString(), "error-optional")
  }

  func testReleasedNativeBatchScrapeResultsValues() throws {
    let value = RustBridge.BatchScrapeResults(
      RustBridge.RustVec<RustBridge.BatchScrapeResult>(),
      2,
      3,
      4)
    XCTAssertEqual(value.results().len(), 0)
    XCTAssertEqual(value.totalCount(), 2)
    XCTAssertEqual(value.completedCount(), 3)
    XCTAssertEqual(value.failedCount(), 4)
  }

  func testReleasedNativeBrowserConfigValues() throws {
    let value = RustBridge.BrowserConfig(
      try RustBridge.browserModeFromJson("\"auto\""),
      try RustBridge.browserBackendFromJson("\"chromiumoxide\""),
      RustBridge.RustString("endpoint-optional"),
      4,
      5,
      6,
      try RustBridge.browserWaitFromJson("\"network_idle\""),
      RustBridge.RustString("wait_selector-optional"),
      9,
      nil,
      RustBridge.RustVec<RustBridge.RustString>(),
      RustBridge.RustString("eval_script-optional"),
      RustBridge.RustString("robots_user_agent-optional"),
      true,
      false,
      RustBridge.RustString("chrome_path-optional"),
      RustBridge.RustVec<RustBridge.RustString>())
    XCTAssertEqual(value.mode().toString(), "auto")
    XCTAssertEqual(value.backend().toString(), "chromiumoxide")
    XCTAssertEqual(value.endpoint()?.toString(), "endpoint-optional")
    XCTAssertEqual(value.timeout(), 4)
    XCTAssertEqual(value.overallTimeout(), 5)
    XCTAssertEqual(value.shutdownTimeout(), 6)
    XCTAssertEqual(value.wait().toString(), "network_idle")
    XCTAssertEqual(value.waitSelector()?.toString(), "wait_selector-optional")
    XCTAssertEqual(value.extraWait(), 9)
    XCTAssertEqual(value.proxy() == nil, true)
    XCTAssertEqual(value.blockUrlPatterns().len(), 0)
    XCTAssertEqual(value.evalScript()?.toString(), "eval_script-optional")
    XCTAssertEqual(value.robotsUserAgent()?.toString(), "robots_user_agent-optional")
    XCTAssertEqual(value.captureNetworkEvents(), true)
    XCTAssertEqual(value.sessionAffinity(), false)
    XCTAssertEqual(value.chromePath()?.toString(), "chrome_path-optional")
    XCTAssertEqual(value.chromeArgs().len(), 0)
  }

  func testReleasedNativeBrowserExtrasValues() throws {
    let value = RustBridge.BrowserExtras(
      RustBridge.RustString("eval_result-optional"),
      RustBridge.RustVec<RustBridge.ResponseMeta>(),
      RustBridge.RustVec<RustBridge.CookieInfo>())
    XCTAssertEqual(value.evalResult()?.toString(), "\"eval_result-optional\"")
    XCTAssertEqual(value.networkEvents().len(), 0)
    XCTAssertEqual(value.cookies().len(), 0)
  }

  func testReleasedNativeCitationReferenceValues() throws {
    let value = RustBridge.CitationReference(
      1,
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("text-sentinel"))
    XCTAssertEqual(value.index(), 1)
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.text().toString(), "text-sentinel")
  }

  func testReleasedNativeCitationResultValues() throws {
    let value = RustBridge.CitationResult(
      RustBridge.RustString("content-sentinel"),
      RustBridge.RustVec<RustBridge.CitationReference>())
    XCTAssertEqual(value.content().toString(), "content-sentinel")
    XCTAssertEqual(value.references().len(), 0)
  }

  func testReleasedNativeContentConfigValues() throws {
    let value = RustBridge.ContentConfig(
      RustBridge.RustString("output_format-sentinel"),
      RustBridge.RustString("preprocessing_preset-sentinel"),
      false,
      true,
      RustBridge.RustVec<RustBridge.RustString>(),
      RustBridge.RustVec<RustBridge.RustString>(),
      RustBridge.RustVec<RustBridge.RustString>(),
      true,
      9,
      true,
      11,
      true,
      false)
    XCTAssertEqual(value.outputFormat().toString(), "output_format-sentinel")
    XCTAssertEqual(value.preprocessingPreset().toString(), "preprocessing_preset-sentinel")
    XCTAssertEqual(value.removeNavigation(), false)
    XCTAssertEqual(value.removeForms(), true)
    XCTAssertEqual(value.stripTags().len(), 0)
    XCTAssertEqual(value.preserveTags().len(), 0)
    XCTAssertEqual(value.excludeSelectors().len(), 0)
    XCTAssertEqual(value.skipImages(), true)
    XCTAssertEqual(value.maxDepth(), 9)
    XCTAssertEqual(value.wrap(), true)
    XCTAssertEqual(value.wrapWidth(), 11)
    XCTAssertEqual(value.includeDocumentStructure(), true)
    XCTAssertEqual(value.extractMetadata(), false)
  }

  func testReleasedNativeCookieInfoValues() throws {
    let value = RustBridge.CookieInfo(
      RustBridge.RustString("name-sentinel"),
      RustBridge.RustString("value-sentinel"),
      RustBridge.RustString("domain-optional"),
      RustBridge.RustString("path-optional"))
    XCTAssertEqual(value.name().toString(), "name-sentinel")
    XCTAssertEqual(value.value().toString(), "value-sentinel")
    XCTAssertEqual(value.domain()?.toString(), "domain-optional")
    XCTAssertEqual(value.path()?.toString(), "path-optional")
  }

  func testReleasedNativeCrawlPageResultValues() throws {
    let value = RustBridge.CrawlPageResult(
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("normalized_url-sentinel"),
      207,
      RustBridge.RustString("content_type-sentinel"),
      RustBridge.RustString("html-sentinel"),
      6,
      try RustBridge.pageMetadataFromJson("{}"),
      RustBridge.RustVec<RustBridge.LinkInfo>(),
      RustBridge.RustVec<RustBridge.ImageInfo>(),
      RustBridge.RustVec<RustBridge.FeedInfo>(),
      RustBridge.RustVec<RustBridge.JsonLdEntry>(),
      12,
      false,
      true,
      false,
      RustBridge.RustString("detected_charset-optional"),
      nil,
      RustBridge.RustString("extracted_data-optional"),
      nil,
      nil,
      false,
      RustBridge.RustString("final_url-sentinel"),
      23,
      true,
      false,
      RustBridge.RustVec<RustBridge.RustString>())
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.normalizedUrl().toString(), "normalized_url-sentinel")
    XCTAssertEqual(value.statusCode(), 207)
    XCTAssertEqual(value.contentType().toString(), "content_type-sentinel")
    XCTAssertEqual(value.html().toString(), "html-sentinel")
    XCTAssertEqual(value.bodySize(), 6)
    XCTAssertEqual(value.metadata().title()?.toString(), nil)
    XCTAssertEqual(value.links().len(), 0)
    XCTAssertEqual(value.images().len(), 0)
    XCTAssertEqual(value.feeds().len(), 0)
    XCTAssertEqual(value.jsonLd().len(), 0)
    XCTAssertEqual(value.depth(), 12)
    XCTAssertEqual(value.stayedOnDomain(), false)
    XCTAssertEqual(value.wasSkipped(), true)
    XCTAssertEqual(value.isPdf(), false)
    XCTAssertEqual(value.detectedCharset()?.toString(), "detected_charset-optional")
    XCTAssertEqual(value.markdown() == nil, true)
    XCTAssertEqual(value.extractedData()?.toString(), "\"extracted_data-optional\"")
    XCTAssertEqual(value.extractionMeta() == nil, true)
    XCTAssertEqual(value.downloadedDocument() == nil, true)
    XCTAssertEqual(value.browserUsed(), false)
    XCTAssertEqual(value.finalUrl().toString(), "final_url-sentinel")
    XCTAssertEqual(value.redirectCount(), 23)
    XCTAssertEqual(value.noindexDetected(), true)
    XCTAssertEqual(value.nofollowDetected(), false)
    XCTAssertEqual(value.ssrfRefusedUrls().len(), 0)
  }

  func testReleasedNativeCrawlResultValues() throws {
    let value = RustBridge.CrawlResult(
      RustBridge.RustVec<RustBridge.CrawlPageResult>(),
      RustBridge.RustString("final_url-sentinel"),
      3,
      true,
      RustBridge.RustString("error-optional"),
      RustBridge.RustVec<RustBridge.CookieInfo>(),
      false,
      true)
    XCTAssertEqual(value.pages().len(), 0)
    XCTAssertEqual(value.finalUrl().toString(), "final_url-sentinel")
    XCTAssertEqual(value.redirectCount(), 3)
    XCTAssertEqual(value.wasSkipped(), true)
    XCTAssertEqual(value.error()?.toString(), "error-optional")
    XCTAssertEqual(value.cookies().len(), 0)
    XCTAssertEqual(value.stayedOnDomain(), false)
    XCTAssertEqual(value.browserUsed(), true)
  }

  func testReleasedNativeCrawlStreamRequestValues() throws {
    let value = RustBridge.CrawlStreamRequest(
      RustBridge.RustString("url-sentinel"))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
  }

  func testReleasedNativeDownloadedAssetValues() throws {
    let value = RustBridge.DownloadedAsset(
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("content_hash-sentinel"),
      RustBridge.RustString("mime_type-optional"),
      4,
      try RustBridge.assetCategoryFromJson("\"document\""),
      RustBridge.RustString("html_tag-optional"))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.contentHash().toString(), "content_hash-sentinel")
    XCTAssertEqual(value.mimeType()?.toString(), "mime_type-optional")
    XCTAssertEqual(value.size(), 4)
    XCTAssertEqual(value.assetCategory().toString(), "document")
    XCTAssertEqual(value.htmlTag()?.toString(), "html_tag-optional")
  }

  func testReleasedNativeExtractionMetaValues() throws {
    let value = RustBridge.ExtractionMeta(
      1,
      2,
      3,
      RustBridge.RustString("model-optional"),
      5)
    XCTAssertEqual(value.cost(), 1)
    XCTAssertEqual(value.promptTokens(), 2)
    XCTAssertEqual(value.completionTokens(), 3)
    XCTAssertEqual(value.model()?.toString(), "model-optional")
    XCTAssertEqual(value.chunksProcessed(), 5)
  }

  func testReleasedNativeFaviconInfoValues() throws {
    let value = RustBridge.FaviconInfo(
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("rel-sentinel"),
      RustBridge.RustString("sizes-optional"),
      RustBridge.RustString("mime_type-optional"))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.rel().toString(), "rel-sentinel")
    XCTAssertEqual(value.sizes()?.toString(), "sizes-optional")
    XCTAssertEqual(value.mimeType()?.toString(), "mime_type-optional")
  }

  func testReleasedNativeFeedInfoValues() throws {
    let value = RustBridge.FeedInfo(
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("title-optional"),
      try RustBridge.feedTypeFromJson("\"rss\""))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.title()?.toString(), "title-optional")
    XCTAssertEqual(value.feedType().toString(), "rss")
  }

  func testReleasedNativeHeadingInfoValues() throws {
    let value = RustBridge.HeadingInfo(
      1,
      RustBridge.RustString("text-sentinel"))
    XCTAssertEqual(value.level(), 1)
    XCTAssertEqual(value.text().toString(), "text-sentinel")
  }

  func testReleasedNativeHreflangEntryValues() throws {
    let value = RustBridge.HreflangEntry(
      RustBridge.RustString("lang-sentinel"),
      RustBridge.RustString("url-sentinel"))
    XCTAssertEqual(value.lang().toString(), "lang-sentinel")
    XCTAssertEqual(value.url().toString(), "url-sentinel")
  }

  func testReleasedNativeImageInfoValues() throws {
    let value = RustBridge.ImageInfo(
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("alt-optional"),
      3,
      4,
      try RustBridge.imageSourceFromJson("\"img\""))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.alt()?.toString(), "alt-optional")
    XCTAssertEqual(value.width(), 3)
    XCTAssertEqual(value.height(), 4)
    XCTAssertEqual(value.source().toString(), "img")
  }

  func testReleasedNativeInteractionResultValues() throws {
    let value = RustBridge.InteractionResult(
      RustBridge.RustVec<RustBridge.ActionResult>(),
      RustBridge.RustString("final_html-sentinel"),
      RustBridge.RustString("final_url-sentinel"),
      RustBridge.RustString("screenshot_base64-optional"),
      RustBridge.RustVec<RustBridge.RustString>())
    XCTAssertEqual(value.actionResults().len(), 0)
    XCTAssertEqual(value.finalHtml().toString(), "final_html-sentinel")
    XCTAssertEqual(value.finalUrl().toString(), "final_url-sentinel")
    XCTAssertEqual(value.screenshotBase64()?.toString(), "screenshot_base64-optional")
    XCTAssertEqual(value.ssrfRefusedUrls().len(), 0)
  }

  func testReleasedNativeJsonLdEntryValues() throws {
    let value = RustBridge.JsonLdEntry(
      RustBridge.RustString("schema_type-sentinel"),
      RustBridge.RustString("name-optional"),
      RustBridge.RustString("raw-sentinel"))
    XCTAssertEqual(value.schemaType().toString(), "schema_type-sentinel")
    XCTAssertEqual(value.name()?.toString(), "name-optional")
    XCTAssertEqual(value.raw().toString(), "raw-sentinel")
  }

  func testReleasedNativeLinkInfoValues() throws {
    let value = RustBridge.LinkInfo(
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("text-sentinel"),
      try RustBridge.linkTypeFromJson("\"external\""),
      RustBridge.RustString("rel-optional"),
      false)
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.text().toString(), "text-sentinel")
    XCTAssertEqual(value.linkType().toString(), "external")
    XCTAssertEqual(value.rel()?.toString(), "rel-optional")
    XCTAssertEqual(value.nofollow(), false)
  }

  func testReleasedNativeMapResultValues() throws {
    let value = RustBridge.MapResult(
      RustBridge.RustVec<RustBridge.SitemapUrl>())
    XCTAssertEqual(value.urls().len(), 0)
  }

  func testReleasedNativeMarkdownResultValues() throws {
    let value = RustBridge.MarkdownResult(
      RustBridge.RustString("content-sentinel"),
      RustBridge.RustString("document_structure-optional"),
      RustBridge.RustVec<RustBridge.RustString>(),
      RustBridge.RustVec<RustBridge.RustString>(),
      false,
      RustBridge.RustString("fit_content-optional"))
    XCTAssertEqual(value.content().toString(), "content-sentinel")
    XCTAssertEqual(value.documentStructure()?.toString(), "\"document_structure-optional\"")
    XCTAssertEqual(value.tables().len(), 0)
    XCTAssertEqual(value.warnings().len(), 0)
    XCTAssertEqual(value.citations(), false)
    XCTAssertEqual(value.fitContent()?.toString(), "fit_content-optional")
  }

  func testReleasedNativeProxyConfigValues() throws {
    let value = RustBridge.ProxyConfig(
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("username-optional"),
      RustBridge.RustString("password-optional"))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.username()?.toString(), "username-optional")
    XCTAssertEqual(value.password()?.toString(), "password-optional")
  }

  func testReleasedNativeResponseMetaValues() throws {
    let value = RustBridge.ResponseMeta(
      RustBridge.RustString("etag-optional"),
      RustBridge.RustString("last_modified-optional"),
      RustBridge.RustString("cache_control-optional"),
      RustBridge.RustString("server-optional"),
      RustBridge.RustString("x_powered_by-optional"),
      RustBridge.RustString("content_language-optional"),
      RustBridge.RustString("content_encoding-optional"))
    XCTAssertEqual(value.etag()?.toString(), "etag-optional")
    XCTAssertEqual(value.lastModified()?.toString(), "last_modified-optional")
    XCTAssertEqual(value.cacheControl()?.toString(), "cache_control-optional")
    XCTAssertEqual(value.server()?.toString(), "server-optional")
    XCTAssertEqual(value.xPoweredBy()?.toString(), "x_powered_by-optional")
    XCTAssertEqual(value.contentLanguage()?.toString(), "content_language-optional")
    XCTAssertEqual(value.contentEncoding()?.toString(), "content_encoding-optional")
  }

  func testReleasedNativeScrapeResultValues() throws {
    let value = RustBridge.ScrapeResult(
      207,
      RustBridge.RustString("final_url-sentinel"),
      RustBridge.RustString("content_type-sentinel"),
      RustBridge.RustString("html-sentinel"),
      5,
      try RustBridge.pageMetadataFromJson("{}"),
      RustBridge.RustVec<RustBridge.LinkInfo>(),
      RustBridge.RustVec<RustBridge.ImageInfo>(),
      RustBridge.RustVec<RustBridge.FeedInfo>(),
      RustBridge.RustVec<RustBridge.JsonLdEntry>(),
      false,
      12,
      false,
      true,
      RustBridge.RustString("x_robots_tag-optional"),
      true,
      false,
      RustBridge.RustString("detected_charset-optional"),
      false,
      nil,
      RustBridge.RustVec<RustBridge.DownloadedAsset>(),
      true,
      false,
      nil,
      RustBridge.RustString("extracted_data-optional"),
      nil,
      RustBridge.RustString("screenshot_base64-optional"),
      nil,
      nil,
      RustBridge.RustVec<RustBridge.RustString>())
    XCTAssertEqual(value.statusCode(), 207)
    XCTAssertEqual(value.finalUrl().toString(), "final_url-sentinel")
    XCTAssertEqual(value.contentType().toString(), "content_type-sentinel")
    XCTAssertEqual(value.html().toString(), "html-sentinel")
    XCTAssertEqual(value.bodySize(), 5)
    XCTAssertEqual(value.metadata().title()?.toString(), nil)
    XCTAssertEqual(value.links().len(), 0)
    XCTAssertEqual(value.images().len(), 0)
    XCTAssertEqual(value.feeds().len(), 0)
    XCTAssertEqual(value.jsonLd().len(), 0)
    XCTAssertEqual(value.isAllowed(), false)
    XCTAssertEqual(value.crawlDelay(), 12)
    XCTAssertEqual(value.noindexDetected(), false)
    XCTAssertEqual(value.nofollowDetected(), true)
    XCTAssertEqual(value.xRobotsTag()?.toString(), "x_robots_tag-optional")
    XCTAssertEqual(value.isPdf(), true)
    XCTAssertEqual(value.wasSkipped(), false)
    XCTAssertEqual(value.detectedCharset()?.toString(), "detected_charset-optional")
    XCTAssertEqual(value.authHeaderSent(), false)
    XCTAssertEqual(value.responseMeta() == nil, true)
    XCTAssertEqual(value.assets().len(), 0)
    XCTAssertEqual(value.jsRenderHint(), true)
    XCTAssertEqual(value.browserUsed(), false)
    XCTAssertEqual(value.markdown() == nil, true)
    XCTAssertEqual(value.extractedData()?.toString(), "\"extracted_data-optional\"")
    XCTAssertEqual(value.extractionMeta() == nil, true)
    XCTAssertEqual(value.screenshotBase64()?.toString(), "screenshot_base64-optional")
    XCTAssertEqual(value.downloadedDocument() == nil, true)
    XCTAssertEqual(value.browser() == nil, true)
    XCTAssertEqual(value.ssrfRefusedUrls().len(), 0)
  }

  func testReleasedNativeSitemapUrlValues() throws {
    let value = RustBridge.SitemapUrl(
      RustBridge.RustString("url-sentinel"),
      RustBridge.RustString("lastmod-optional"),
      RustBridge.RustString("changefreq-optional"),
      RustBridge.RustString("priority-optional"))
    XCTAssertEqual(value.url().toString(), "url-sentinel")
    XCTAssertEqual(value.lastmod()?.toString(), "lastmod-optional")
    XCTAssertEqual(value.changefreq()?.toString(), "changefreq-optional")
    XCTAssertEqual(value.priority()?.toString(), "priority-optional")
  }

  func testReleasedNativeSsrfPolicyValues() throws {
    let value = RustBridge.SsrfPolicy(
      false,
      RustBridge.RustVec<RustBridge.HostMatcher>(),
      RustBridge.RustVec<RustBridge.HostMatcher>(),
      4,
      RustBridge.RustVec<RustBridge.RustString>())
    XCTAssertEqual(value.denyPrivate(), false)
    XCTAssertEqual(value.allowlist().len(), 0)
    XCTAssertEqual(value.denylist().len(), 0)
    XCTAssertEqual(value.maxRedirects(), 4)
    XCTAssertEqual(value.schemeAllowlist().len(), 0)
  }
}
