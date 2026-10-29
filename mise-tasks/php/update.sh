#!/usr/bin/env bash
#MISE description="Update PHP dependencies to latest versions (including major versions)"
set -euo pipefail

# Move each direct dependency of one composer project to its latest release.
require_latest() {
  composer outdated --direct --format=json | jq -r '.installed[]? | .name' | grep -v '^$' |
    xargs -r -I {} composer require {} --dev --no-update --no-interaction || true
}

echo "Updating root composer.json dependencies..."
require_latest
composer update --with-all-dependencies

echo "Updating packages/php dependencies to latest versions..."
(cd packages/php && require_latest)
(cd packages/php && composer update --with-all-dependencies)
(cd packages/php && echo "Checking remaining outdated dependencies..." && composer outdated --direct) || true

# e2e/php is regenerated from fixtures, so a failure there does not fail the update. ~keep
echo "Updating e2e/php dependencies to latest versions..."
(cd e2e/php && require_latest) || true
(cd e2e/php && composer update --with-all-dependencies) || true
(cd e2e/php && echo "Checking remaining outdated dependencies..." && composer outdated --direct) || true
