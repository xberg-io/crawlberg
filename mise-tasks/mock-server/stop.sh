#!/usr/bin/env bash
#MISE description="Stop the e2e mock HTTP server"
set -euo pipefail

if [ -f .mock-server.pid ]; then
  server_pid="$(cat .mock-server.pid)"
  if ! [[ "$server_pid" =~ ^[0-9]+$ ]] || [ "$server_pid" -le 1 ]; then
    echo "mock-server PID must be an integer greater than 1" >&2
    exit 1
  fi
  kill "$server_pid" 2>/dev/null || true
  rm -f .mock-server.pid .mock-server.env
fi
