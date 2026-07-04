// swift-tools-version: 6.0
import PackageDescription

let package = Package(
  name: "E2eSwift",
  platforms: [
    .macOS(.v13),
    .iOS(.v16),
  ],
  dependencies: [
    .package(url: "https://github.com/xberg-io/crawlberg", branch: "release/swift/1.0.3"),
  ],
  targets: [
    .testTarget(
      name: "CrawlbergE2ETests",
      dependencies: [.product(name: "Crawlberg", package: "crawlberg")]
    ),
  ]
)
