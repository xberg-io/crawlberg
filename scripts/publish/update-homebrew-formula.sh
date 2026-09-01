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


def substitute_once(text, pattern, replacement, what):
    # A `re.sub` that matches nothing returns the input unchanged, which is
    # indistinguishable from a successful rewrite once the file is written back: the
    # formula would keep the PREVIOUS release's url or sha256 and this script would still
    # exit 0, leaving the tap installing the old version (or failing every user's checksum
    # check) with nothing in the release logs. Assert the pattern matched instead. The
    # mirror-image check — grepping afterwards for the value we just wrote — is not a
    # substitute: it passes both when the rewrite worked and when the formula already
    # happened to contain it.
    updated, count = re.subn(pattern, replacement, text, count=1)
    if count != 1:
        sys.exit(f"{path}: no {what} line matched; refusing to publish a formula that still points at the previous release")
    return updated


# `url '...'` or `url "..."`
content = substitute_once(content, r'''url\s+['"][^'"]*['"]''', f'url \'{url}\'', 'url')
# First `sha256 '...'` — formula source SHA appears before the bottle block,
# so the first match is the source SHA; bottle SHAs (cellar: …, tag: "...")
# have a different shape and don't match the bare `sha256 'hex'` regex.
content = substitute_once(content, r'''sha256\s+['"][0-9a-f]+['"]''', f'sha256 \'{sha}\'', 'source sha256')

# Strip the existing bottle block. Bumping the url without touching the bottle
# block leaves root_url pinned to the PREVIOUS release while the version moved
# forward, so Homebrew composes `<old-tag>/crawlberg-<new-version>...bottle.tar.gz`
# → 404 for every user until the bottle-DSL merge job lands (and if that job fails
# or is skipped, the formula stays broken). Removing the block here makes the
# committed intermediate formula always installable — it just builds from source
# until the merge re-adds a fresh block matching this release.
# Unlike the two rewrites above this one legitimately matches nothing: a formula that has
# never been bottled, or a re-run after the block was already stripped, has no block to
# remove. Report the count rather than asserting it.
bottle_re = re.compile(r"^[ \t]*bottle do\b.*?^[ \t]*end(?:\n|\Z)", re.MULTILINE | re.DOTALL)
content, bottles_stripped = bottle_re.subn("", content)
print(f"Stripped {bottles_stripped} bottle block(s)", file=sys.stderr)
content = re.sub(r"\n{3,}", "\n\n", content)

open(path, 'w').write(content)
print(f"Updated source url + sha256 (stripped stale bottle block) in {path}", file=sys.stderr)
PY

echo "Updated formula: $formula" >&2
