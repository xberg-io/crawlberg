#!/usr/bin/env bash
set -euo pipefail

chrome_path="${CHROME:?CHROME must name the installed browser executable}"
[ -x "$chrome_path" ] || {
  echo "Chrome executable is missing: $chrome_path" >&2
  exit 1
}

if [ "$(uname -s)" = Darwin ]; then
  case "$chrome_path" in
  */Contents/MacOS/*)
    bundle="${chrome_path%/Contents/MacOS/*}"
    case "$bundle" in
    *.app) ;;
    *)
      [ -f "$bundle/Contents/Info.plist" ] || {
        echo "Chrome bundle has no Info.plist: $bundle" >&2
        exit 1
      }
      # ~keep setup-chrome caches a macOS bundle as arm64 without .app; Chrome 155 then
      # ~keep cancels page creation. ditto preserves the framework's links and permissions.
      prepared_dir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/crawlberg-chrome.XXXXXX")"
      prepared_bundle="$prepared_dir/$(basename "$chrome_path").app"
      ditto "$bundle" "$prepared_bundle"
      chrome_path="$prepared_bundle/${chrome_path#"$bundle/"}"
      [ -x "$chrome_path" ] || {
        echo "Prepared Chrome executable is missing: $chrome_path" >&2
        exit 1
      }
      ;;
    esac
    ;;
  esac
fi

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  printf 'chrome-path=%s\n' "$chrome_path" >>"$GITHUB_OUTPUT"
fi
printf '%s\n' "$chrome_path"
