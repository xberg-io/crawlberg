#!/bin/bash

set -e

PROFILE="${1:-release}"
BUILD_DIR="target/${PROFILE}/build"

if stat -f '%m %N' "$BUILD_DIR" >/dev/null 2>&1; then
  STAT_FMT=(-f '%m %N')
else
  STAT_FMT=(-c '%Y %n')
fi
# ~keep Cargo's per-package build-dir layout nests every compile unit of the package under
# build/crawlberg-swift/<hash>/out (the build-script compile, the lib rmeta, the final objects), not
# just the build-script run, so the newest `out` is not necessarily the one holding the bridge. Keep
# only directories that actually contain SwiftBridgeCore.h; the flat build/crawlberg-swift-<hash>/out
# layout is accepted too.
OUT=$(find "$BUILD_DIR" -maxdepth 4 -type f -name SwiftBridgeCore.h \
  \( -path '*/crawlberg-swift-*/out/*' -o -path '*/crawlberg-swift/*/out/*' \) \
  -exec stat "${STAT_FMT[@]}" {} + 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2- | xargs -I{} dirname {})
if [ -z "$OUT" ]; then
  echo "ERROR: Could not find swift-bridge build output in ${BUILD_DIR}/"
  exit 1
fi

echo "Using swift-bridge output from: $OUT"

mkdir -p packages/swift/Sources/RustBridgeC
mkdir -p packages/swift/Sources/RustBridge

cat "$OUT/SwiftBridgeCore.h" "$OUT/crawlberg-swift/crawlberg-swift.h" \
  >packages/swift/Sources/RustBridgeC/RustBridgeC.h

{
  printf 'import RustBridgeC\n'
  cat "$OUT/SwiftBridgeCore.swift"
} >packages/swift/Sources/RustBridge/SwiftBridgeCore.swift
{
  printf 'import RustBridgeC\n'
  cat "$OUT/crawlberg-swift/crawlberg-swift.swift"
} >packages/swift/Sources/RustBridge/crawlberg-swift.swift

echo "Swift-bridge files setup complete"
