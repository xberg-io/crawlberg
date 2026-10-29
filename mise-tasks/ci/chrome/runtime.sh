#!/usr/bin/env bash
#MISE description="Prepare Chromium snap runtime providers on Linux ARM runners"
set -euo pipefail

# shellcheck source=mise-tasks/.lib/platform.sh
. "$MISE_CONFIG_ROOT/mise-tasks/.lib/platform.sh"
require_platform linux arm64
# Arguments: max attempts, retry delay in seconds, then the snap providers. ~keep
bash scripts/ci/prepare-chrome-runtime.sh 5 5 gnome-46-2404 mesa-2404
