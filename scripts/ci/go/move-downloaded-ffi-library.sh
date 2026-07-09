#!/usr/bin/env bash
set -euo pipefail

if [ ! -d "ffi-download" ]; then
  echo "✗ Error: ffi-download directory not found"
  exit 1
fi

mkdir -p target/release
mkdir -p target/x86_64-pc-windows-gnu/release
mkdir -p packages/go/v4/internal/ffi
mkdir -p crates/crawlberg-ffi

echo "Moving FFI artifacts from ffi-download..."
echo ""

LIBRARY_COUNT=0
while IFS= read -r file; do
  filename="$(basename "$file")"
  if [[ "$file" == *"x86_64-pc-windows-gnu"* ]]; then
    cp "$file" target/x86_64-pc-windows-gnu/release/
    echo "✓ Copied $filename to target/x86_64-pc-windows-gnu/release/"
  else
    cp "$file" target/release/
    echo "✓ Copied $filename to target/release/"
  fi
  ((LIBRARY_COUNT++)) || true
done < <(find ffi-download -type f \( -name "libcrawlberg_ffi.*" -o -name "crawlberg_ffi.*" \))

if [ "$LIBRARY_COUNT" -eq 0 ]; then
  echo "⚠ Warning: No FFI library files found in ffi-download (may be a cross-platform build artifact)"
fi

HEADER_FOUND=false
if [ -f "ffi-download/crawlberg.h" ]; then
  cp ffi-download/crawlberg.h packages/go/v4/internal/ffi/
  echo "✓ Copied crawlberg.h to packages/go/v4/internal/ffi/"
  HEADER_FOUND=true
elif [ -f "ffi-download/crates/crawlberg-ffi/include/crawlberg.h" ]; then
  cp ffi-download/crates/crawlberg-ffi/include/crawlberg.h packages/go/v4/internal/ffi/
  echo "✓ Copied crawlberg.h to packages/go/v4/internal/ffi/"
  HEADER_FOUND=true
fi

if [ "$HEADER_FOUND" = false ]; then
  echo "✗ Error: Header file crawlberg.h not found in ffi-download"
  echo "   Contents of ffi-download:"
  ls -la ffi-download/ || echo "   (unable to list directory)"
  echo "   Contents of ffi-download/crates (if exists):"
  ls -la ffi-download/crates/ 2>/dev/null || echo "   (directory does not exist)"
  exit 1
fi

if [ ! -f "packages/go/v4/internal/ffi/crawlberg.h" ]; then
  echo "✗ Error: Failed to copy crawlberg.h to packages/go/v4/internal/ffi/"
  exit 1
fi

if [ -f "ffi-download/crawlberg-ffi.pc" ]; then
  cp ffi-download/crawlberg-ffi.pc crates/crawlberg-ffi/
  echo "✓ Copied crawlberg-ffi.pc to crates/crawlberg-ffi/"
elif [ -f "ffi-download/crates/crawlberg-ffi/crawlberg-ffi.pc" ]; then
  cp ffi-download/crates/crawlberg-ffi/crawlberg-ffi.pc crates/crawlberg-ffi/
  echo "✓ Copied crawlberg-ffi.pc to crates/crawlberg-ffi/"
else
  echo "⚠ Warning: pkg-config file crawlberg-ffi.pc not found in ffi-download"
fi

echo ""
echo "Cleaning up ffi-download directory..."
rm -rf ffi-download
echo "✓ Done"
