#!/usr/bin/env bash
# Verify every tracked copy of the generated C FFI header against the canonical one.
set -euo pipefail

CANONICAL="crates/crawlberg-ffi/include/crawlberg.h"

# ~keep Two tiers, because the tracked copies are not produced the same way and a single
# ~keep byte-diff over all of them is not satisfiable. STRICT_COPIES are the paths
# ~keep crates/crawlberg-ffi/build.rs publishes on `task c:headers`, so they must be
# ~keep byte-identical. ABI_COPIES are vendored next to a prebuilt platform dylib; they were
# ~keep emitted by a different cbindgen and differ from the canonical header only in comment
# ~keep wrapping (936 diff lines, 520/520 cberg_* symbols identical). Byte-diffing them would
# ~keep fail for formatting reasons and get them excluded again — the exact failure this gate
# ~keep exists to stop — and `poly fmt` reformats a byte-identical copy at those paths, so
# ~keep byte equality is not even a stable state there. They are compared as a normalised
# ~keep declaration stream, which still fails loudly when a declaration is added, removed or
# ~keep changed, i.e. whenever a caller could actually break.
STRICT_COPIES=(
  "packages/go/include/crawlberg.h"
)
ABI_COPIES=(
  "packages/csharp/Crawlberg/runtimes/osx-arm64/native/include/crawlberg.h"
  "packages/go/.lib/macos-arm64/include/crawlberg.h"
  "packages/java/src/main/resources/natives/macos-arm64/include/crawlberg.h"
)

# ~keep A normalised stream shorter than this cannot be a real crawlberg.h, so a normaliser
# ~keep that silently produced nothing fails instead of reporting every copy as matching.
MIN_DECLARATIONS=400

workdir="$(mktemp -d)"
[ -n "$workdir" ] && [ -d "$workdir" ] || exit 90
trap 'rm -rf "$workdir"' EXIT

# ~keep cc -fpreprocessed is not portable (macOS clang rejects it), so comments are stripped
# ~keep with an explicit state machine instead.
strip_comments() {
  awk '
    {
      out = ""; i = 1; n = length($0)
      while (i <= n) {
        c = substr($0, i, 1); d = substr($0, i, 2)
        if (in_comment) {
          if (d == "*/") { in_comment = 0; i += 2 } else { i++ }
        } else if (d == "/*") {
          in_comment = 1; i += 2
        } else if (d == "//") {
          break
        } else {
          out = out c; i++
        }
      }
      print out
    }
  ' "$1"
}

normalise_header() {
  strip_comments "$1" |
    tr -s '[:space:]' ' ' |
    tr ';' '\n' |
    sed -e 's/^ *//' -e 's/ *$//' -e 's/( /(/g' -e 's/ )/)/g' -e 's/ ,/,/g' |
    { grep -v '^$' || true; }
}

count_symbols() {
  { grep -oE 'cberg_[A-Za-z0-9_]+' "$1" || true; } | sort -u | wc -l | tr -d ' '
}

if [ ! -f "$CANONICAL" ]; then
  echo "::error::canonical header $CANONICAL is missing"
  exit 1
fi

normalise_header "$CANONICAL" >"$workdir/canonical.decls"
canonical_declarations="$(wc -l <"$workdir/canonical.decls" | tr -d ' ')"
canonical_symbols="$(count_symbols "$CANONICAL")"

if [ "$canonical_declarations" -lt "$MIN_DECLARATIONS" ]; then
  echo "::error::normalising $CANONICAL yielded only $canonical_declarations declarations" \
    "(expected at least $MIN_DECLARATIONS) — the normaliser examined nothing"
  exit 1
fi

echo "canonical: $CANONICAL — $canonical_declarations declarations, $canonical_symbols cberg_* symbols"

# ~keep The gate's own failure mode is checking a list that has quietly stopped covering every
# ~keep copy: it shipped for months validating only packages/go/include/crawlberg.h, the one
# ~keep path that cannot differ. Discover the copies from git instead and fail on any that is
# ~keep not classified, so adding a new vendored header forces a decision here.
tracked=()
while IFS= read -r tracked_path; do
  tracked+=("$tracked_path")
done < <(git ls-files | grep -E '(^|/)crawlberg\.h$' | sort)
expected_copies=$((${#STRICT_COPIES[@]} + ${#ABI_COPIES[@]} + 1))
echo "tracked crawlberg.h copies: expected $expected_copies, found ${#tracked[@]}"

failures=0
checked=0

for path in "${tracked[@]}"; do
  classified=0
  for known in "$CANONICAL" "${STRICT_COPIES[@]}" "${ABI_COPIES[@]}"; do
    if [ "$path" = "$known" ]; then
      classified=1
      break
    fi
  done
  if [ "$classified" -eq 0 ]; then
    echo "::error::$path is a tracked copy of crawlberg.h that this check does not classify —" \
      "add it to STRICT_COPIES or ABI_COPIES in $0"
    failures=$((failures + 1))
  fi
done

for path in "${STRICT_COPIES[@]}"; do
  checked=$((checked + 1))
  if [ ! -f "$path" ]; then
    echo "::error::vendored header $path is missing — restore it or update STRICT_COPIES in $0"
    failures=$((failures + 1))
  elif ! diff -q "$CANONICAL" "$path" >/dev/null 2>&1; then
    echo "::error::$path is not byte-identical to $CANONICAL — run 'task c:headers'"
    diff "$CANONICAL" "$path" | head -20
    failures=$((failures + 1))
  else
    echo "ok (byte-identical): $path"
  fi
done

for path in "${ABI_COPIES[@]}"; do
  checked=$((checked + 1))
  if [ ! -f "$path" ]; then
    echo "::error::vendored header $path is missing — restore it or update ABI_COPIES in $0"
    failures=$((failures + 1))
    continue
  fi
  normalise_header "$path" >"$workdir/copy.decls"
  declarations="$(wc -l <"$workdir/copy.decls" | tr -d ' ')"
  symbols="$(count_symbols "$path")"
  if ! diff -q "$workdir/canonical.decls" "$workdir/copy.decls" >/dev/null 2>&1; then
    echo "::error::$path declares a different C API than $CANONICAL" \
      "($declarations declarations / $symbols symbols vs $canonical_declarations / $canonical_symbols)." \
      "Refresh the vendored native bundle for that platform."
    diff "$workdir/canonical.decls" "$workdir/copy.decls" | head -20
    failures=$((failures + 1))
  else
    echo "ok (same C API): $path — $declarations declarations, $symbols cberg_* symbols"
  fi
done

echo "checked $checked vendored copies of $expected_copies tracked crawlberg.h paths; $failures failure(s)"

if [ "$checked" -ne $((expected_copies - 1)) ]; then
  echo "::error::expected to check $((expected_copies - 1)) vendored copies but checked $checked"
  exit 1
fi

[ "$failures" -eq 0 ] || exit 1
