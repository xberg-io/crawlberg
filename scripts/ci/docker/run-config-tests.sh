#!/usr/bin/env bash

set -euo pipefail

variant="${1:?missing variant}"

echo "=== Running Docker configuration tests (${variant}) ==="

exec ./scripts/test/test-docker-config-local.sh --image "crawlberg:${variant}" --variant "${variant}"
