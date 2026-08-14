#!/usr/bin/env bash
set -euo pipefail

mode="${1:-check}"

root="$(git rev-parse --show-toplevel)"

export PATH="$HOME/go/bin:/usr/lib/golang/bin:${PATH:-}"
export PKG_CONFIG_PATH="$root/crates/crawlberg-ffi:${PKG_CONFIG_PATH:-}"
export DYLD_LIBRARY_PATH="$root/target/release:$root/target/debug:${DYLD_LIBRARY_PATH:-}"
export LD_LIBRARY_PATH="$root/target/release:$root/target/debug:${LD_LIBRARY_PATH:-}"

if [ ! -f "$root/target/release/libcrawlberg_ffi.dylib" ] && [ ! -f "$root/target/release/libcrawlberg_ffi.so" ] && [ ! -f "$root/target/debug/libcrawlberg_ffi.dylib" ] && [ ! -f "$root/target/debug/libcrawlberg_ffi.so" ]; then
  echo "==> Building crawlberg-ffi (required by Go bindings)..."
  cargo build -p crawlberg-ffi 2>/dev/null
fi

workspace_dirs=(
  packages/go
  e2e/go
  tools/benchmark-harness/scripts
)

standalone_dirs=()

failed=0

lint_dir() {
  local dir="$1"
  local full="$root/$dir"

  if [ ! -f "$full/go.mod" ]; then
    return
  fi

  echo "==> Linting $dir"
  cd "$full"

  case "$mode" in
  fix)
    go fmt ./...
    golangci-lint run --config "$root/.golangci.yml" --fix ./... || failed=1
    ;;
  check)
    if gofmt -l . | read -r; then
      echo "  gofmt issues in $dir:"
      gofmt -l .
      failed=1
    fi
    golangci-lint run --config "$root/.golangci.yml" ./... || failed=1
    ;;
  *)
    echo "Usage: $0 [fix|check]" >&2
    exit 2
    ;;
  esac
}

for dir in "${workspace_dirs[@]}"; do
  lint_dir "$dir"
done

if [ ${#standalone_dirs[@]} -gt 0 ]; then
  for dir in "${standalone_dirs[@]}"; do
    GOWORK=off lint_dir "$dir"
  done
fi

exit $failed
