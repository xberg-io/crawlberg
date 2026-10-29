#!/usr/bin/env bash
#MISE description="Run e2e tests across all bindings (mock server auto-managed)"
set -euo pipefail

# shellcheck source=mise-tasks/.lib/mock-server.sh
. "$MISE_CONFIG_ROOT/mise-tasks/.lib/mock-server.sh"
mise run e2e:build
start_mock_server
for task in rust:build python:build:dev node:build:dev rust:ffi:build go:build java:build csharp:build \
  ruby:build php:build elixir:build; do
  mise run "$task"
done

# Each fixture server is also exported as MOCK_SERVER_<FIXTURE_ID>. ~keep
fixture_exports="$(python3 -c '
import json, os, re, shlex
servers = json.loads(os.environ["MOCK_SERVERS"])
if not isinstance(servers, dict):
    raise ValueError("MOCK_SERVERS must be an object")
for fixture_id, url in servers.items():
    name = "MOCK_SERVER_" + fixture_id.upper()
    if not fixture_id or re.fullmatch(r"[A-Z_][A-Z0-9_]*", name) is None or not isinstance(url, str):
        raise ValueError("MOCK_SERVERS requires valid environment names and string URLs")
    print("export " + name + "=" + shlex.quote(url))
')"
eval "$fixture_exports"

# Every suite runs even when an earlier one fails; the last failing status is the result. ~keep
overall=0
for lang in rust python node go java csharp ruby php elixir; do
  mise run "$lang:e2e:test" || overall=$?
done
if command -v pnpm >/dev/null 2>&1; then
  mise run wasm:e2e:test || overall=$?
else
  echo "skip: wasm e2e (pnpm not found)"
fi
if command -v cc >/dev/null 2>&1 && command -v make >/dev/null 2>&1; then
  mise run c:e2e:test || overall=$?
else
  echo "skip: c e2e (cc/make not found)"
fi
mise run brew:e2e:test || overall=$?
exit "$overall"
