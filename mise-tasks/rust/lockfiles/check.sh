#!/usr/bin/env bash
#MISE description="Check secondary Cargo lockfiles match their manifests"
set -euo pipefail

# ~keep These five manifests own independent lockfiles and are not covered by commands
# ~keep against the root workspace. Assert both counts so a lost path or an empty loop
# ~keep cannot turn this drift gate into a successful zero-work check (#296).
expected=5
checked=0
manifests=(
  fuzz/Cargo.toml
  e2e/rust/Cargo.toml
  packages/elixir/native/crawlberg_nif/Cargo.toml
  packages/ruby/ext/crawlberg_rb/native/Cargo.toml
  vendor/crawlberg-chromiumoxide/Cargo.toml
)

echo "secondary Cargo manifests: expected $expected, configured ${#manifests[@]}"
if [ "${#manifests[@]}" -ne "$expected" ]; then
  echo "expected $expected secondary Cargo manifests, configured ${#manifests[@]}" >&2
  exit 1
fi

for manifest in "${manifests[@]}"; do
  if [ -z "$manifest" ] || [ ! -f "$manifest" ]; then
    echo "secondary Cargo manifest is missing: ${manifest:-<empty>}" >&2
    exit 1
  fi
  lockfile="${manifest%/*}/Cargo.lock"
  if [ ! -f "$lockfile" ]; then
    echo "secondary Cargo lockfile is missing: $lockfile" >&2
    exit 1
  fi
  if ! cargo metadata --format-version 1 --locked --manifest-path "$manifest" >/dev/null; then
    echo "secondary Cargo lockfile is stale: $lockfile" >&2
    exit 1
  fi
  checked=$((checked + 1))
done

echo "checked $checked of $expected secondary Cargo lockfiles"
if [ "$checked" -ne "$expected" ]; then
  echo "expected to check $expected secondary Cargo lockfiles, checked $checked" >&2
  exit 1
fi
