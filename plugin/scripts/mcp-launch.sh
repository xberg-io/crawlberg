#!/usr/bin/env bash
set -euo pipefail

# ~keep Rewritten by scripts/sync_plugin_version.py on every release. The plugin only accepts a
# ~keep binary at or above this version, and fetches exactly this release (npm package or GitHub
# ~keep archive), so a plugin update cannot keep running an older crawlberg that happens to be installed.
PINNED_VERSION="1.13.6"
NPM_PACKAGE="@xberg-io/crawlberg-cli"

log() { printf 'crawlberg-launch: %s\n' "$*" >&2; }
die() {
  log "error: $*"
  exit 1
}

LAUNCHER="${CRAWLBERG_LAUNCHER:-auto}"
case "$LAUNCHER" in
auto | npx | brew | download) ;;
*) die "invalid CRAWLBERG_LAUNCHER='$LAUNCHER' (expected auto|npx|brew|download)" ;;
esac

want() { [ "$LAUNCHER" = "auto" ] || [ "$LAUNCHER" = "$1" ]; }
# brew mutates the host package manager, so it runs only when explicitly requested.
want_explicit() { [ "$LAUNCHER" = "$1" ]; }

PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-}"
if [ -z "$PLUGIN_ROOT" ]; then
  PLUGIN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

BINARY_NAME="crawlberg"
case "$(uname -s)" in
MINGW* | MSYS* | CYGWIN* | Windows_NT) BINARY_NAME="crawlberg.exe" ;;
esac
BIN_DIR="$PLUGIN_ROOT/bin"
BIN="$BIN_DIR/$BINARY_NAME"

have() { command -v "$1" >/dev/null 2>&1; }

# True when $1 runs and reports a version at or above PINNED_VERSION. A prerelease of a version
# sorts below that version (SemVer), so 1.13.1-rc.1 does not satisfy a 1.13.1 pin. ~keep
runs_ok() {
  local out found base
  [ -x "$1" ] || return 1
  out="$("$1" --version 2>/dev/null)" || return 1
  found="$(printf '%s\n' "$out" | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?' | head -n 1)"
  [ -n "$found" ] || return 1
  base="${found%%-*}"
  if [ "$(printf '%s\n%s\n' "$PINNED_VERSION" "$base" | sort -V | head -n 1)" != "$PINNED_VERSION" ] ||
    { [ "$base" = "$PINNED_VERSION" ] && [ "$found" != "$base" ]; }; then
    log "ignoring $1: version $found is older than the pinned $PINNED_VERSION"
    return 1
  fi
}

if runs_ok "$BIN"; then
  log "using cached crawlberg at $BIN"
  exec "$BIN" "$@"
fi
if have "$BINARY_NAME"; then
  PATH_BIN="$(command -v "$BINARY_NAME")"
  if runs_ok "$PATH_BIN"; then
    log "using crawlberg on PATH ($PATH_BIN)"
    exec "$PATH_BIN" "$@"
  fi
fi

# The npm package carries the native binary in a per-platform optional dependency, so this tier
# works wherever an npm registry (or mirror) is reachable and needs no curl, wget or brew.
# npm writes install progress to stderr only; the update notifier, fund and audit notices are off
# so nothing but the server's own protocol reaches stdout. ~keep
if want npx && have node && have npx; then
  log "running ${NPM_PACKAGE}@${PINNED_VERSION} via npx ..."
  export npm_config_update_notifier=false npm_config_fund=false npm_config_audit=false
  exec npx --yes "${NPM_PACKAGE}@${PINNED_VERSION}" "$@"
fi

if want_explicit brew && have brew; then
  log "installing via 'brew install xberg-io/tap/crawlberg' ..."
  if brew install xberg-io/tap/crawlberg >&2; then
    if have "$BINARY_NAME"; then
      BREW_BIN="$(command -v "$BINARY_NAME")"
      runs_ok "$BREW_BIN" && exec "$BREW_BIN" "$@"
    fi
    log "brew install reported success but crawlberg is not on PATH; falling through"
  else
    log "brew install failed; falling through"
  fi
fi

download_install() {
  local arch triple ext base_url asset asset_url tmp ex src_bin src_dir expected
  arch="$(uname -m)"
  case "$(uname -s)" in
  Darwin)
    case "$arch" in
    arm64 | aarch64) triple="aarch64-apple-darwin" ;;
    x86_64) triple="x86_64-apple-darwin" ;;
    *)
      log "download: unsupported macOS architecture: $arch; falling through"
      return 1
      ;;
    esac
    ;;
  Linux)
    case "$arch" in
    aarch64 | arm64) triple="aarch64-unknown-linux-gnu" ;;
    x86_64) triple="x86_64-unknown-linux-gnu" ;;
    *)
      log "download: unsupported Linux architecture: $arch; falling through"
      return 1
      ;;
    esac
    ;;
  MINGW* | MSYS* | CYGWIN* | Windows_NT) triple="x86_64-pc-windows-msvc" ;;
  *)
    log "download: unsupported platform: $(uname -s) $arch; falling through"
    return 1
    ;;
  esac
  case "$triple" in
  *windows*) ext="zip" ;;
  *) ext="tar.gz" ;;
  esac

  base_url="https://github.com/xberg-io/crawlberg/releases/download/v${PINNED_VERSION}"
  asset="crawlberg-cli-${triple}.${ext}"
  asset_url="${base_url}/${asset}"

  if have curl; then
    fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
  elif have wget; then
    fetch() { wget -q -O "$2" "$1"; }
  else
    log "download: need curl or wget; falling through"
    return 1
  fi

  tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN

  if have sha256sum; then
    sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
  elif have shasum; then
    sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
  else
    log "download: need sha256sum or shasum to verify the archive; falling through"
    return 1
  fi

  log "downloading $asset from release v$PINNED_VERSION ..."
  if ! fetch "$asset_url" "$tmp/$asset"; then
    log "download: fetch failed (asset missing or network error): $asset_url; falling through"
    return 1
  fi
  if ! fetch "${base_url}/crawlberg-cli-SHA256SUMS" "$tmp/SHA256SUMS"; then
    log "download: could not fetch crawlberg-cli-SHA256SUMS; refusing an unverified archive; falling through"
    return 1
  fi
  expected="$(awk -v a="$asset" '{ f = $2; sub(/^\*/, "", f); if (f == a) { print $1; exit } }' "$tmp/SHA256SUMS")"
  if [ -z "$expected" ]; then
    log "download: no checksum entry for $asset; refusing an unverified archive; falling through"
    return 1
  fi
  if [ "$(sha256_of "$tmp/$asset")" != "$expected" ]; then
    log "download: checksum mismatch for $asset; falling through"
    return 1
  fi

  log "extracting ..."
  ex="$tmp/extracted"
  mkdir -p "$ex"
  case "$ext" in
  tar.gz) tar -xzf "$tmp/$asset" -C "$ex" || return 1 ;;
  zip)
    if have unzip; then
      unzip -qo "$tmp/$asset" -d "$ex" || return 1
    else
      log "download: need unzip to extract $asset; falling through"
      return 1
    fi
    ;;
  esac

  src_bin="$(find "$ex" -type f -name "$BINARY_NAME" -print -quit)"
  if [ -z "$src_bin" ]; then
    log "download: binary $BINARY_NAME not found in $asset; falling through"
    return 1
  fi
  src_dir="$(dirname "$src_bin")"

  rm -rf "$BIN_DIR"
  mkdir -p "$BIN_DIR"
  mv "$src_dir"/* "$BIN_DIR"/
  chmod +x "$BIN"
  log "installed crawlberg to $BIN"
  return 0
}

if want download; then
  if download_install; then
    runs_ok "$BIN" && exec "$BIN" "$@"
    log "downloaded binary at $BIN did not run; falling through"
  fi
fi

die "could not locate or install a crawlberg binary. Install one manually:
  npm install -g ${NPM_PACKAGE}@${PINNED_VERSION}  (or run it with: npx --yes ${NPM_PACKAGE}@${PINNED_VERSION} mcp)
  brew install xberg-io/tap/crawlberg
  cargo install crawlberg-cli --features all
or download a release from https://github.com/xberg-io/crawlberg/releases/tag/v${PINNED_VERSION}
then ensure 'crawlberg' is on PATH (or place it at $BIN)."
