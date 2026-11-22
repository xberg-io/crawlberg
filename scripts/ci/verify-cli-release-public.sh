#!/usr/bin/env bash
# Fail unless the release is public and carries every CLI archive and the checksum file.
#
# The CLI proxy packages resolve the release by tag through the public API at install time, and
# a draft is invisible there. Publishing a package that cannot download its binary would hand
# users a version that fails until the release is public, and a registry version cannot be
# replaced, so the publish jobs call this first.
#
# Usage: REPO=owner/name TAG=vX.Y.Z GH_TOKEN=... scripts/ci/verify-cli-release-public.sh
set -euo pipefail

: "${REPO:?REPO is required}"
: "${TAG:?TAG is required}"

required=(
  crawlberg-cli-aarch64-apple-darwin.tar.gz
  crawlberg-cli-aarch64-unknown-linux-gnu.tar.gz
  crawlberg-cli-aarch64-unknown-linux-musl.tar.gz
  crawlberg-cli-x86_64-unknown-linux-gnu.tar.gz
  crawlberg-cli-x86_64-unknown-linux-musl.tar.gz
  crawlberg-cli-x86_64-pc-windows-msvc.zip
  crawlberg-cli-SHA256SUMS
)

attempts=6
for attempt in $(seq 1 "$attempts"); do
  if release="$(gh api "repos/${REPO}/releases/tags/${TAG}" 2>/dev/null)" &&
    [[ "$(jq -r '.draft' <<<"$release")" == "false" ]]; then
    names="$(jq -r '.assets[].name' <<<"$release")"
    missing=()
    for asset in "${required[@]}"; do
      grep -qxF "$asset" <<<"$names" || missing+=("$asset")
    done
    if [[ "${#missing[@]}" -eq 0 ]]; then
      echo "Release ${TAG} is public with all ${#required[@]} CLI assets."
      exit 0
    fi
    echo "Release ${TAG} is public but lacks: ${missing[*]}" >&2
  else
    echo "Release ${TAG} is not public yet (attempt ${attempt}/${attempts})." >&2
  fi
  [[ "$attempt" -lt "$attempts" ]] && sleep 10
done

echo "::error::Release ${TAG} is not public with every CLI asset, so a package that downloads its binary from it would be broken on arrival." >&2
exit 1
