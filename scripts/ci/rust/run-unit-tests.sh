#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"

source "$REPO_ROOT/scripts/lib/common.sh"

validate_repo_root "$REPO_ROOT" || exit 1

cd "$REPO_ROOT"

echo "=== Running Rust unit tests ==="
echo "  Repository: $REPO_ROOT"
echo "  RUST_BACKTRACE: ${RUST_BACKTRACE:-not set}"
echo "  CARGO_TERM_COLOR: ${CARGO_TERM_COLOR:-not set}"

TEST_LOG="/tmp/cargo-test-$$.log"
: > "$TEST_LOG"

# ~keep A single `if ! { cmd1; cmd2; } | tee log; then` masks cmd1's failure: `set -e`
# is suppressed inside an `if` condition, and the compound command's exit status
# collapses to cmd2's. Run each cargo invocation through its own `if ! ... | tee`
# and read PIPESTATUS right after, so neither run's failure goes unnoticed.

core_status=0
echo "=== cargo test -p crawlberg --all-features ==="
if ! RUST_BACKTRACE=full cargo test -p crawlberg --all-features --verbose 2>&1 | tee -a "$TEST_LOG"; then
  core_status="${PIPESTATUS[0]}"
fi

workspace_status=0
echo "=== cargo test --workspace (excluding bindings) ==="
if ! RUST_BACKTRACE=full cargo test \
  --workspace \
  --exclude crawlberg \
  --exclude crawlberg-py \
  --exclude crawlberg-node \
  --exclude crawlberg-php \
  --exclude crawlberg-wasm \
  --all-features \
  --verbose 2>&1 | tee -a "$TEST_LOG"; then
  workspace_status="${PIPESTATUS[0]}"
fi

if [ "$core_status" -ne 0 ] || [ "$workspace_status" -ne 0 ]; then
  echo "=== Test execution failed ==="
  echo "Last 50 lines of test output:"
  tail -n 50 "$TEST_LOG"
  rm -f "$TEST_LOG"
  exit 1
fi

rm -f "$TEST_LOG"

echo "=== Tests complete ==="
