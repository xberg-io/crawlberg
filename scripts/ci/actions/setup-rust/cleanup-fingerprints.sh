#!/usr/bin/env bash
set -euo pipefail


echo "=== Cleaning Cargo Fingerprints ==="

echo "Removing incremental compilation caches..."
find target -type d -name "incremental" -exec rm -rf {} + 2>/dev/null || true

echo "Cleaning all fingerprint directories..."
find target -type d -name ".fingerprint" -exec rm -rf {} + 2>/dev/null || true

find target -name ".cargo-ok" -delete 2>/dev/null || true

if [[ "$RUNNER_OS" == "Windows" ]] || [[ "${OS:-}" == "Windows_NT" ]]; then
  echo "Detected Windows platform - performing Windows-specific cleanup..."

  if [ -d ~/.cargo/registry/index ]; then
    rm -rf ~/.cargo/registry/index
    echo "  Removed cargo registry index"
  fi

  rm -f ~/.cargo/registry/cache/.cargo-ok 2>/dev/null || true

  cargo metadata --quiet 2>/dev/null || true
fi

echo "Verifying Cargo state..."
if ! cargo --version &>/dev/null; then
  echo "ERROR: Cargo is broken after cleanup!"
  exit 1
fi

echo "Fingerprint cleanup completed successfully"
