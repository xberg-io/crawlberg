#!/usr/bin/env bash
#MISE description="Run Windows driver job tests and real Chrome lifecycle regressions"
set -euo pipefail

# shellcheck source=mise-tasks/.lib/platform.sh
. "$MISE_CONFIG_ROOT/mise-tasks/.lib/platform.sh"
require_os windows
# shellcheck disable=SC2016 # $env: is PowerShell syntax, expanded by PowerShell
if ! powershell -NoProfile -Command \
  'if (-not (Test-Path -LiteralPath $env:CRAWLBERG_TEST_CHROME_PATH -PathType Leaf)) { exit 1 }'; then
  echo "CRAWLBERG_TEST_CHROME_PATH must point to the installed Chrome executable" >&2
  exit 1
fi
cargo test --locked --manifest-path vendor/crawlberg-chromiumoxide/Cargo.toml --lib -- --test-threads=1
# ~keep The two library tests of the crate that compile only on Windows. They get their own
# ~keep command: a name filter beside `--test` would filter the two test files below too.
cargo test -p crawlberg --features browser,interact --lib -- --exact \
  browser_pool::tests::a_process_started_at_once_by_the_launched_one_is_in_its_process_tree \
  browser_pool::tests::a_launched_profile_holds_the_process_tree_of_its_chrome --test-threads=1
cargo test -p crawlberg --features browser,interact \
  --test test_browser_profile_cleanup --test test_browser_pool_lifecycle -- --test-threads=1
