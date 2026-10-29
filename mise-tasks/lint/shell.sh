#!/usr/bin/env bash
#MISE description="Format-check the shell sources under scripts/tests"
set -euo pipefail

# ~keep poly runs shellcheck on shell but does NOT run shfmt: a tracked .sh mangled on purpose
# still passes `poly fmt --check` while `shfmt -l` flags it. So formatting needs an
# explicit invocation.
#
# scripts/tests/lib is excluded because it holds the VENDORED helper library, which must
# stay byte-identical to its source or `mise run test:scripts:lib:check` fails.
#
# The list is built with find and asserted non-empty: shfmt with zero arguments reads
# stdin and exits 0, so a stale scope would silently check nothing.
files=()
while IFS= read -r file; do
  files+=("$file")
done < <(find scripts/tests -type f \( -name '*.bats' -o -name '*.sh' \) -not -path 'scripts/tests/lib/*')
if [ "${#files[@]}" -eq 0 ]; then
  echo "lint:shell matched no files under scripts/tests; the scope is stale" >&2
  exit 1
fi
shfmt -d "${files[@]}"
