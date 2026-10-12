import XCTest
import RustBridge
@testable import Crawlberg

final class ContentConstructorCompatibilityTests: XCTestCase {
  func testLegacyBridgeConstructorKeepsItsThirteenArguments() {
    let native = RustBridge.ContentConfig(
      RustString("markdown"), RustString("minimal"), false, false,
      RustVec<RustString>(), RustVec<RustString>(), RustVec<RustString>(),
      true, 5, false, 80, true, false
    )
    XCTAssertTrue(native.skipImages())
    XCTAssertEqual(native.maxDepth(), 5)
    XCTAssertFalse(native.extractMetadata())
    XCTAssertFalse(native.keepHidden())
  }

  func testAddedFieldUsesTheSeparateBridgeConstructor() throws {
    let config = Crawlberg.ContentConfig(skipImages: true, maxDepth: 5, keepHidden: true)
    let native = try config.intoRust()
    XCTAssertTrue(native.skipImages())
    XCTAssertEqual(native.maxDepth(), 5)
    XCTAssertTrue(native.keepHidden())
  }
}
