import XCTest

@testable import Crawlberg

final class CrawlbergTests: XCTestCase {
  /// Round-trips the generated `HreflangEntry` DTO through `JSONEncoder`/`JSONDecoder`,
  /// so a broken `Codable` conformance or a field that silently stops encoding fails
  /// `swift test` immediately instead of shipping green with a suite that asserts
  /// nothing about the generated API. Create-only scaffold seed. ~keep
  func testHreflangEntryRoundTripsThroughJSON() throws {
    let original = HreflangEntry(lang: "alef-scaffold", url: "alef-scaffold")
    let data = try JSONEncoder().encode(original)
    let decoded = try JSONDecoder().decode(HreflangEntry.self, from: data)
    XCTAssertEqual(decoded, original)
  }
}
