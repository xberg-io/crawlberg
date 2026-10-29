#!/usr/bin/env bash
#MISE description="Refresh every ecosystem, carrying on past failures and listing them at the end"
#MISE hide=true
#USAGE arg "<mode>" help="update stays within major versions; upgrade takes the latest, breaking ones included"
# shellcheck disable=SC2154 # usage_* are set by mise from the #USAGE lines
set -euo pipefail

mode="$usage_mode"
failed=()
step() {
  "$@" || failed+=("$*")
}

case "$mode" in
update)
  step alef update
  ;;
upgrade)
  step alef update --latest
  ;;
*)
  echo "mode must be update or upgrade, got: $mode" >&2
  exit 2
  ;;
esac
step gau --update .
for lang in rust python node go ruby php elixir dart swift; do
  step mise run deps "$mode" "$lang"
done
if [ "$mode" = upgrade ]; then
  step mise run deps upgrade zig
  step mise run java:update
  step mise run csharp:update
fi

# A refresh is best effort: an ecosystem whose toolchain is missing must not stop the others. ~keep
if [ "${#failed[@]}" -gt 0 ]; then
  echo "warning: these refresh steps failed:" >&2
  printf '  %s\n' "${failed[@]}" >&2
fi
