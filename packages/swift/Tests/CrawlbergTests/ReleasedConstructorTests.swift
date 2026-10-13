import Crawlberg
import Foundation
import XCTest

// ~keep Hand-written calls from v1.10.3: 23 memberwise initializers and 26 Decoder initializers.
// ~keep Keep these calls unchanged when regenerating bindings; update them only for a new released baseline.
final class ReleasedConstructorTests: XCTestCase {
  func testReleasedMemberwiseCallsStillCompile() {
    let values: [any Sendable] = [
      ArticleMetadata(publishedTime: nil, modifiedTime: nil, author: nil, section: nil, tags: []),
      BatchCrawlStreamRequest(urls: []),
      CitationReference(index: 0, url: "https://example.test", text: "reference"),
      CitationResult(content: "content", references: []),
      ContentConfig(
        outputFormat: "markdown", preprocessingPreset: "standard", removeNavigation: true,
        removeForms: true, stripTags: [], preserveTags: [], excludeSelectors: ["noscript"],
        skipImages: false, maxDepth: nil, wrap: false, wrapWidth: 80,
        includeDocumentStructure: true, extractMetadata: true),
      CookieInfo(name: "session", value: "value", domain: nil, path: nil),
      CrawlStreamRequest(url: "https://example.test"),
      DownloadedAsset(
        url: "https://example.test", contentHash: "hash", mimeType: nil, size: 0,
        assetCategory: .document, htmlTag: nil),
      DownloadedDocument(
        url: "https://example.test", mimeType: "text/plain", size: 0, filename: nil,
        contentHash: "hash", headers: [:], truncated: false, contentPath: nil, contentBase64: nil),
      ExtractionMeta(cost: nil, promptTokens: nil, completionTokens: nil, model: nil, chunksProcessed: 0),
      FaviconInfo(url: "https://example.test", rel: "icon", sizes: nil, mimeType: nil),
      FeedInfo(url: "https://example.test", title: nil, feedType: .rss),
      HeadingInfo(level: 1, text: "heading"),
      HreflangEntry(lang: "en", url: "https://example.test"),
      ImageInfo(url: "https://example.test", alt: nil, width: nil, height: nil, source: .img),
      JsonLdEntry(schemaType: "Article", name: nil, raw: "{}"),
      LinkInfo(url: "https://example.test", text: "link", linkType: .external, rel: nil, nofollow: false),
      MapResult(urls: []),
      PageMetadata(
        title: nil, description: nil, canonicalUrl: nil, keywords: nil, author: nil,
        viewport: nil, themeColor: nil, generator: nil, robots: nil, htmlLang: nil, htmlDir: nil,
        ogTitle: nil, ogType: nil, ogImage: nil, ogDescription: nil, ogUrl: nil, ogSiteName: nil,
        ogLocale: nil, ogVideo: nil, ogAudio: nil, ogLocaleAlternates: nil, twitterCard: nil,
        twitterTitle: nil, twitterDescription: nil, twitterImage: nil, twitterSite: nil, twitterCreator: nil,
        dcTitle: nil, dcCreator: nil, dcSubject: nil, dcDescription: nil, dcPublisher: nil, dcDate: nil,
        dcType: nil, dcFormat: nil, dcIdentifier: nil, dcLanguage: nil, dcRights: nil,
        article: nil, hreflangs: nil, favicons: nil, headings: nil, wordCount: nil),
      ProxyConfig(url: "http://proxy.example.test", username: nil, password: nil),
      ResponseMeta(
        etag: nil, lastModified: nil, cacheControl: nil, server: nil, xPoweredBy: nil,
        contentLanguage: nil, contentEncoding: nil),
      SitemapUrl(url: "https://example.test", lastmod: nil, changefreq: nil, priority: nil),
      SsrfPolicy(denyPrivate: true, allowlist: [], denylist: [], maxRedirects: 5, schemeAllowlist: ["http", "https"]),
    ]
    XCTAssertEqual(values.count, 23)
  }

  func testReleasedDefaultsKeepTheirValues() {
    let content = ContentConfig()
    XCTAssertEqual(content.outputFormat, "markdown")
    XCTAssertEqual(content.wrapWidth, 80)
    XCTAssertEqual(content.includeDocumentStructure, true)
    XCTAssertEqual(content.extractMetadata, true)
    let link = LinkInfo(url: "https://example.test", text: "link", linkType: .external, nofollow: false)
    XCTAssertEqual(link.rel, nil)
    let policy = SsrfPolicy()
    XCTAssertEqual(policy.denyPrivate, true)
    XCTAssertEqual(policy.maxRedirects, 5)
    XCTAssertEqual(policy.schemeAllowlist, ["http", "https"])
  }
}

// ~keep Swift checks this function's body even when no runtime Decoder is supplied. These are
// ~keep the actual public init(from:) calls, including the three payload enums, rather than substitute types.
private func releasedDecoderCalls(_ decoder: any Decoder) throws {
  _ = try ArticleMetadata(from: decoder)
  _ = try BatchCrawlStreamRequest(from: decoder)
  _ = try CitationReference(from: decoder)
  _ = try CitationResult(from: decoder)
  _ = try ContentConfig(from: decoder)
  _ = try CookieInfo(from: decoder)
  _ = try CrawlStreamRequest(from: decoder)
  _ = try DownloadedAsset(from: decoder)
  _ = try DownloadedDocument(from: decoder)
  _ = try ExtractionMeta(from: decoder)
  _ = try FaviconInfo(from: decoder)
  _ = try FeedInfo(from: decoder)
  _ = try HeadingInfo(from: decoder)
  _ = try HreflangEntry(from: decoder)
  _ = try ImageInfo(from: decoder)
  _ = try JsonLdEntry(from: decoder)
  _ = try LinkInfo(from: decoder)
  _ = try MapResult(from: decoder)
  _ = try PageMetadata(from: decoder)
  _ = try ProxyConfig(from: decoder)
  _ = try ResponseMeta(from: decoder)
  _ = try SitemapUrl(from: decoder)
  _ = try SsrfPolicy(from: decoder)
  _ = try AuthConfig(from: decoder)
  _ = try HostMatcher(from: decoder)
  _ = try PageAction(from: decoder)
}
