#!/usr/bin/env bash
#MISE description="Check that the bindings keep every constructor of the last release"
set -euo pipefail

if ! python3 -c 'import tree_sitter_swift' 2>/dev/null; then
  echo "The Swift reader of the check is missing. Run: python3 -m pip install -r" \
    "scripts/ci/requirements-released-constructors.txt (in a virtual environment)" >&2
  exit 1
fi
python3 scripts/ci/test_released_constructors.py
python3 scripts/ci/check_released_constructors.py
