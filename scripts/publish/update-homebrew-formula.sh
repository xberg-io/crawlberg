#!/usr/bin/env bash
set -euo pipefail

#   VERSION — semver without v prefix, e.g. 0.3.0-rc.25

tag="${TAG:?TAG is required (e.g. v0.3.0-rc.25)}"
version="${VERSION:?VERSION is required (e.g. 0.3.0-rc.25)}"
tap_dir="${TAP_DIR:?TAP_DIR is required (path to homebrew-tap checkout)}"

formula="${tap_dir}/Formula/crawlberg.rb"

[[ -f "$formula" ]] || {
  echo "Missing $formula" >&2
  exit 1
}

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

source_url="https://github.com/xberg-io/crawlberg/archive/${tag}.tar.gz"
echo "Downloading source archive from $source_url..." >&2
curl -fsSL "$source_url" -o "$work_dir/source.tar.gz"
source_sha="$(shasum -a 256 "$work_dir/source.tar.gz" | awk '{print $1}')"

if [[ ! "$source_sha" =~ ^[a-f0-9]{64}$ ]]; then
  echo "Computed invalid sha256: $source_sha" >&2
  exit 1
fi

echo "Source tarball sha256: $source_sha" >&2

python3 - "$formula" "$source_url" "$source_sha" "$version" <<'PY'
import re
import sys

path, url, sha, version = sys.argv[1:5]
content = open(path).read()

# `url '...'` or `url "..."`
content = re.sub(r'''url\s+['"][^'"]*['"]''', f'url \'{url}\'', content, count=1)
# First `sha256 '...'` — formula source SHA appears before the bottle block,
# so the first match is the source SHA; bottle SHAs (cellar: …, tag: "...")
# have a different shape and don't match the bare `sha256 'hex'` regex.
content = re.sub(r'''sha256\s+['"][0-9a-f]+['"]''', f'sha256 \'{sha}\'', content, count=1)

# Strip the existing bottle block. Bumping the url without touching the bottle
# block leaves root_url pinned to the PREVIOUS release while the version moved
# forward, so Homebrew composes `<old-tag>/crawlberg-<new-version>...bottle.tar.gz`
# → 404 for every user until the bottle-DSL merge job lands (and if that job fails
# or is skipped, the formula stays broken). Removing the block here makes the
# committed intermediate formula always installable — it just builds from source
# until the merge re-adds a fresh block matching this release.
bottle_re = re.compile(r"^[ \t]*bottle do\b.*?^[ \t]*end(?:\n|\Z)", re.MULTILINE | re.DOTALL)
content = bottle_re.sub("", content)
content = re.sub(r"\n{3,}", "\n\n", content)

open(path, 'w').write(content)
print(f"Updated source url + sha256 (stripped stale bottle block) in {path}", file=sys.stderr)
PY

echo "Updated formula: $formula" >&2
