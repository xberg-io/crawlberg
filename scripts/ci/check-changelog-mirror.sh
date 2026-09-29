#!/usr/bin/env bash
# Verify the docs-site changelog mirror carries the same [Unreleased] block as CHANGELOG.md.
set -euo pipefail

ROOT="CHANGELOG.md"
MIRROR="docs-site/src/content/docs/changelog.md"

# ~keep Nothing syncs these two files (alef.toml says so and only excludes both from a lint rule),
# so they diverged unnoticed: the mirror was missing two whole [Unreleased] entries on main. Only
# the [Unreleased] block is compared — released sections are appended at release time and the
# mirror legitimately trails there.
extract_unreleased() {
  awk '/^## \[Unreleased\]/ { found = 1; next } /^## \[/ { found = 0 } found' "$1"
}

workdir="$(mktemp -d)"
[ -n "$workdir" ] && [ -d "$workdir" ] || exit 90
trap 'rm -rf "$workdir"' EXIT

failures=0
for path in "$ROOT" "$MIRROR"; do
  if [ ! -f "$path" ]; then
    echo "::error::$path is missing"
    failures=$((failures + 1))
  elif ! grep -q '^## \[Unreleased\]' "$path"; then
    echo "::error::$path has no '## [Unreleased]' heading — this check would compare nothing"
    failures=$((failures + 1))
  fi
done
[ "$failures" -eq 0 ] || exit 1

extract_unreleased "$ROOT" >"$workdir/root.txt"
extract_unreleased "$MIRROR" >"$workdir/mirror.txt"
root_lines="$(wc -l <"$workdir/root.txt" | tr -d ' ')"
mirror_lines="$(wc -l <"$workdir/mirror.txt" | tr -d ' ')"
echo "[Unreleased] block: $ROOT has $root_lines lines, $MIRROR has $mirror_lines lines"

if [ "$root_lines" -eq 0 ]; then
  echo "::error::the [Unreleased] block in $ROOT is empty — nothing was compared"
  exit 1
fi

if ! diff -q "$workdir/root.txt" "$workdir/mirror.txt" >/dev/null 2>&1; then
  diff -u "$workdir/root.txt" "$workdir/mirror.txt" | head -60
  echo "::error::$MIRROR is a hand-maintained mirror of $ROOT and its [Unreleased] block has drifted. Copy the block from $ROOT into $MIRROR."
  exit 1
fi

echo "ok: the [Unreleased] blocks match ($root_lines lines)"
