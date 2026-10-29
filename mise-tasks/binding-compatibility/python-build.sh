#!/usr/bin/env bash
#MISE description="Build and install the current Python extension for released-call tests"
#MISE dir="{{config_root}}/packages/python"
set -euo pipefail

uv sync --locked --all-extras --no-install-project --no-install-workspace
project_python="$(uv run --no-sync python -c 'import sys; print(sys.executable)')"
[ -n "$project_python" ] && [ -x "$project_python" ]
uv pip install --python "$project_python" 'maturin>=1,<2'
project_venv="$("$project_python" -c 'import sys; print(sys.prefix)')"
[ -n "$project_venv" ] && [ -d "$project_venv" ]
project_maturin="$("$project_python" -c '
import pathlib, sys, sysconfig
executable = "maturin.exe" if sys.platform == "win32" else "maturin"
print(pathlib.Path(sysconfig.get_path("scripts")) / executable)
')"
[ -n "$project_maturin" ] && [ -x "$project_maturin" ]
VIRTUAL_ENV="$project_venv" "$project_maturin" develop --locked --profile dev
