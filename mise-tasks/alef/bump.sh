#!/usr/bin/env bash
#MISE description="Bump the alef pin in alef.toml and mise.toml, install it, then regenerate"
#USAGE arg "<version>" help="alef release to pin, e.g. 0.115.6"
# shellcheck disable=SC2154 # usage_* are set by mise from the #USAGE lines
set -euo pipefail

version="$usage_version"
# ~keep mise puts its pinned alef first on PATH inside every task, so bumping alef.toml alone
# ~keep would regenerate with the old release. Both pins move together.
sed -i.bak "s/^alef_version = .*/alef_version = \"$version\"/" alef.toml
sed -i.bak "s/^\"github:xberg-io\/alef\" = .*/\"github:xberg-io\/alef\" = \"$version\"/" mise.toml
rm -f alef.toml.bak mise.toml.bak
mise lock github:xberg-io/alef
mise install github:xberg-io/alef
mise run alef:generate
