// swift-tools-version: 6.0
import PackageDescription
import Foundation

// NOTE: Run `cargo build -p crawlberg-swift` and then rerun `alef generate`

let rustTargetDir = (#filePath as NSString).deletingLastPathComponent.appending("/../../target")

let package = Package(
  name: "Crawlberg",
  platforms: [
    .macOS(.v13),
    .iOS(.v16),
  ],
  products: [
    .library(name: "Crawlberg", targets: ["Crawlberg"])
  ],
  targets: [
    .target(
      name: "RustBridgeC",
      path: "Sources/RustBridgeC",
      publicHeadersPath: "."
    ),
    .target(
      name: "RustBridge",
      dependencies: ["RustBridgeC"],
      path: "Sources/RustBridge",
      linkerSettings: [
        .unsafeFlags([
          "-L\(rustTargetDir)/release",
          "-L\(rustTargetDir)/debug",
          "-Xlinker", "-rpath", "-Xlinker", "\(rustTargetDir)/release",
          "-Xlinker", "-rpath", "-Xlinker", "\(rustTargetDir)/debug",
        ]),
        .linkedLibrary("crawlberg_swift"),
        .linkedLibrary("crawlberg_ffi"),
        .linkedFramework("Security", .when(platforms: [.macOS, .iOS])),
        .linkedFramework("CoreFoundation", .when(platforms: [.macOS, .iOS])),
        .linkedFramework("SystemConfiguration", .when(platforms: [.macOS])),
      ]
    ),
    .target(
      name: "Crawlberg", dependencies: ["RustBridge"],
      path: "Sources/Crawlberg",
      exclude: ["LICENSE"]),
    .testTarget(
      name: "CrawlbergTests", dependencies: ["Crawlberg"],
      path: "Tests/CrawlbergTests"),
  ]
)
