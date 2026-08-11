#!/usr/bin/env bash
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <language>"
  echo "Example: $0 swift"
  exit 1
fi

LANGUAGE="$1"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

cd "$REPO_ROOT"
# ~keep No --format flag: alef dropped it, and both 0.60.0 and 0.60.1 reject it outright,
# which silently broke all 12 per-language e2e:generate tasks. Formatting is already a
# separate `<lang>:e2e:format` task that every caller runs immediately after this one.
alef e2e generate --lang "$LANGUAGE"
