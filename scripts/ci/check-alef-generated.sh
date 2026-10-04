#!/usr/bin/env bash
set -euo pipefail

command -v alef >/dev/null
command -v poly >/dev/null

alef --version
poly --version

report="$(mktemp)"
trap 'rm -f "$report"' EXIT

set +e
alef verify --exit-code 2>&1 | tee "$report"
pipeline_status=("${PIPESTATUS[@]}")
alef_status="${pipeline_status[0]}"
tee_status="${pipeline_status[1]}"
set -e

if [ "$tee_status" -ne 0 ]; then
  echo "ERROR: failed to capture alef verify output" >&2
  exit "$tee_status"
fi

if grep -q 'poly fmt over the drift-check temp copies failed (non-fatal)' "$report"; then
  echo "ERROR: alef verify could not complete its Poly-owned formatted-output comparison" >&2
  if [ "$alef_status" -ne 0 ]; then
    exit "$alef_status"
  fi
  exit 1
fi

summary="$(grep -E "Formatted-output drift check: [0-9]+ file\(s\) compared via a real \`poly fmt\` pass" "$report" | tail -1 || true)"
if [ -z "$summary" ]; then
  if [ "$alef_status" -ne 0 ]; then
    exit "$alef_status"
  fi
  echo "ERROR: alef verify did not report Poly-owned formatted-output coverage" >&2
  exit 1
fi

if grep -q 'could not be checked for formatted-output drift' "$report"; then
  echo "ERROR: alef verify skipped Poly-owned generated files because its Poly comparison could not run" >&2
  exit 1
fi

compared="$(printf '%s\n' "$summary" | sed -E 's/.*Formatted-output drift check: ([0-9]+) file\(s\).*/\1/')"
if [ "$compared" -le 0 ]; then
  echo "ERROR: alef verify compared zero Poly-owned generated files" >&2
  exit 1
fi

echo "Verified Poly-owned formatted-output comparison covered ${compared} generated file(s)."
exit "$alef_status"
