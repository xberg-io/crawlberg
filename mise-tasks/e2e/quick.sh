#!/usr/bin/env bash
#MISE description="Quick e2e (Rust only — fastest feedback)"
set -euo pipefail

# shellcheck source=mise-tasks/.lib/mock-server.sh
. "$MISE_CONFIG_ROOT/mise-tasks/.lib/mock-server.sh"
mise run e2e:build
start_mock_server
mise run rust:build
mise run rust:e2e:test
