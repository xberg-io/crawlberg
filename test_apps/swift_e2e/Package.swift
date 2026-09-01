// swift-tools-version: 6.0
// The first-party dependency pin below is managed by alef (sync.text_replacements); do not edit it by hand.
// alef:hash:1d4f95f7991b96f285ef3f78c3814e3b3fe6648dc4839584809773d7025570b7
import PackageDescription

let package = Package(
  name: "E2eSwift",
  platforms: [
    .macOS(.v13),
    .iOS(.v16),
  ],
  dependencies: [
    .package(url: "https://github.com/xberg-io/crawlberg", branch: "release/swift/1.5.0"),
  ],
  targets: [
    .testTarget(
      name: "CrawlbergE2ETests",
      dependencies: [.product(name: "Crawlberg", package: "crawlberg")]
    ),
  ]
)
