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
alef e2e generate --lang "$LANGUAGE" --format=false
