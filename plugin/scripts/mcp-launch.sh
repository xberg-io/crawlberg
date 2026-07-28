#!/usr/bin/env bash
set -euo pipefail

NPM_PKG="@xberg-io/crawlberg-cli"
PYPI_PKG="crawlberg-cli"

log() { printf 'crawlberg-launch: %s\n' "$*" >&2; }
die() {
	log "error: $*"
	exit 1
}

LAUNCHER="${CRAWLBERG_LAUNCHER:-auto}"
case "$LAUNCHER" in
auto | npx | uvx | brew | download) ;;
*) die "invalid CRAWLBERG_LAUNCHER='$LAUNCHER' (expected auto|npx|uvx|brew|download)" ;;
esac

want() { [ "$LAUNCHER" = "auto" ] || [ "$LAUNCHER" = "$1" ]; }

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

runs_ok() { [ -x "$1" ] && "$1" --version >/dev/null 2>&1; }

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

if want npx && have npx; then
	log "probing npx $NPM_PKG@latest ..."
	scratch="$(mktemp -d)"
	if (cd "$scratch" && npx -y "$NPM_PKG@latest" --version >/dev/null 2>&1); then
		log "launching via npx $NPM_PKG@latest"
		cd "$scratch"
		exec npx -y "$NPM_PKG@latest" "$@"
	fi
	rm -rf "$scratch"
	log "npx $NPM_PKG not available (no CLI bin yet); falling through"
fi

if want uvx && have uvx; then
	log "probing uvx --from $PYPI_PKG $BINARY_NAME ..."
	if uvx --from "$PYPI_PKG" "$BINARY_NAME" --version >/dev/null 2>&1; then
		log "launching via uvx --from $PYPI_PKG $BINARY_NAME"
		exec uvx --from "$PYPI_PKG" "$BINARY_NAME" "$@"
	fi
	log "uvx $PYPI_PKG not available (no CLI entry point yet); falling through"
fi

if want brew && have brew; then
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
	local arch triple ext base_url asset asset_url tmp ex src_bin src_dir
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

	base_url="https://github.com/xberg-io/crawlberg/releases/latest/download"
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

	# SECURITY: crawlberg's release pipeline publishes no checksums manifest for
	log "warning: no published checksum for $asset; integrity relies on TLS only."
	log "downloading $asset from latest release ..."
	if ! fetch "$asset_url" "$tmp/$asset"; then
		log "download: fetch failed (asset missing or network error): $asset_url; falling through"
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
  brew install xberg-io/tap/crawlberg
  cargo install crawlberg-cli --features all
or download a release from https://github.com/xberg-io/crawlberg/releases/latest
then ensure 'crawlberg' is on PATH (or place it at $BIN)."
