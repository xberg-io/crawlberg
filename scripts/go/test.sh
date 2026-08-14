#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

source "${REPO_ROOT}/scripts/lib/common.sh"
source "${REPO_ROOT}/scripts/lib/library-paths.sh"

validate_repo_root "$REPO_ROOT" || exit 1

"${REPO_ROOT}/scripts/download_pdfium_runtime.sh"

setup_go_paths "$REPO_ROOT"

cd "${REPO_ROOT}/packages/go"

verbose_mode="${VERBOSE_MODE:-${CI:-false}}"
is_ci="${CI:-}"

while [[ $# -gt 0 ]]; do
  case $1 in
  --verbose | -v)
    verbose_mode=true
    shift
    ;;
  *)
    shift
    ;;
  esac
done

go_test_flags=("-timeout" "10m")

if [ "$verbose_mode" = "true" ] || [ -n "$is_ci" ]; then
  go_test_flags+=("-v")
  echo "Running Go tests with verbose output..."
fi

if [ -n "$is_ci" ] || [ "$verbose_mode" = "true" ]; then
  echo "Environment Information:"
  echo "  Go version: $(go version)"
  echo "  Working directory: $(pwd)"
  echo "  LD_LIBRARY_PATH: ${LD_LIBRARY_PATH:-<not set>}"
  echo "  DYLD_LIBRARY_PATH: ${DYLD_LIBRARY_PATH:-<not set>}"
  echo "  CGO_ENABLED: ${CGO_ENABLED:-<not set>}"
  echo "  CGO_CFLAGS: ${CGO_CFLAGS:-<not set>}"
  echo "  CGO_LDFLAGS: ${CGO_LDFLAGS:-<not set>}"
  echo ""
fi

export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

echo "Starting Go tests..."
go test "${go_test_flags[@]}" ./... || {
  exit_code=$?
  echo ""
  echo "ERROR: Go tests failed with exit code $exit_code"
  echo "This may be a segmentation fault. Check the output above for stack traces."
  exit $exit_code
}
