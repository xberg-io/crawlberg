#!/usr/bin/env bash
# Ensure the flutter_rust_bridge_codegen binary on PATH matches the flutter_rust_bridge runtime
# version pinned in packages/dart/rust/Cargo.toml.
#
# flutter_rust_bridge's generated bridge embeds FLUTTER_RUST_BRIDGE_CODEGEN_VERSION, and its
# runtime handler asserts that constant equals the linked crate's own version on the first wire
# call. A codegen binary whose version differs from the pinned crate therefore rewrites the
# committed frb_generated.rs into a file that aborts (SIGABRT) the moment Dart calls into Rust.
#
# A presence-only `command -v ... || cargo install` guard does not prevent this: a machine that
# already has the wrong version on PATH skips the install and still corrupts the generated file.
# The version, not mere presence, is what must be checked.
#
# The required version is read from the Cargo manifest rather than hardcoded here so that it
# cannot drift from the pin alef generates ([crates.dart] frb_version in alef.toml).
set -euo pipefail

readonly BINARY="flutter_rust_bridge_codegen"

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
readonly MANIFEST="${repository_root}/packages/dart/rust/Cargo.toml"

if [ ! -f "${MANIFEST}" ]; then
  echo "error: ${MANIFEST} not found; cannot determine the required ${BINARY} version" >&2
  exit 1
fi

required_version="$(
  sed -n 's/^flutter_rust_bridge[[:space:]]*=[[:space:]]*"=\([0-9][^"]*\)".*/\1/p' "${MANIFEST}" |
    head -n 1
)"
readonly required_version

if [ -z "${required_version}" ]; then
  echo "error: could not read the pinned flutter_rust_bridge version from ${MANIFEST}" >&2
  echo "       expected a line of the form: flutter_rust_bridge = \"=<version>\"" >&2
  exit 1
fi

installed_version() {
  command -v "${BINARY}" >/dev/null 2>&1 || return 0
  "${BINARY}" --version 2>/dev/null | head -n 1 | awk '{print $NF}'
}

if [ "$(installed_version)" != "${required_version}" ]; then
  echo "${BINARY}: installed '$(installed_version)', required '${required_version}' - installing"
  cargo install "${BINARY}" --version "${required_version}" --locked --force
fi

# Unconditional assertion: never let a mismatched codegen reach `generate`.
actual_version="$(installed_version)"
readonly actual_version
if [ "${actual_version}" != "${required_version}" ]; then
  echo "error: ${BINARY} reports '${actual_version}' but packages/dart/rust/Cargo.toml pins" >&2
  echo "       flutter_rust_bridge '${required_version}'. Generated bridge output is not" >&2
  echo "       deterministic across codegen versions and the runtime asserts they match." >&2
  echo "       Fix: cargo install ${BINARY} --version ${required_version} --locked --force" >&2
  exit 1
fi
