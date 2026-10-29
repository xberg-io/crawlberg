#!/usr/bin/env bash
#MISE description="Start the e2e mock HTTP server (background)"
set -euo pipefail

if [ ! -x e2e/rust/target/release/mock-server ]; then
  echo "alef mock-server binary not built. Run: mise run e2e:build" >&2
  exit 1
fi
MOCK_SERVER_NO_STDIN_WATCH=1 ./e2e/rust/target/release/mock-server ./fixtures \
  </dev/null >.mock-server.out 2>&1 &
server_pid=$!
# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
  kill "$server_pid" 2>/dev/null || true
  rm -f .mock-server.pid .mock-server.env
}
trap cleanup EXIT
printf "%s\n" "$server_pid" >.mock-server.pid
for _ in $(seq 1 100); do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat .mock-server.out >&2
    exit 1
  fi
  if grep -q "^MOCK_SERVERS=" .mock-server.out &&
    grep -q "^MOCK_SERVER_URL=http://127\.0\.0\.1:" .mock-server.out; then
    server_url="$(grep "^MOCK_SERVER_URL=" .mock-server.out)"
    server_map="$(grep "^MOCK_SERVERS=" .mock-server.out)"
    printf "MOCK_SERVER_URL=%q\nMOCK_SERVERS=%q\n" \
      "${server_url#MOCK_SERVER_URL=}" "${server_map#MOCK_SERVERS=}" >.mock-server.env
    trap - EXIT
    cat .mock-server.env
    exit 0
  fi
  sleep 0.1
done
cat .mock-server.out >&2
echo "mock-server did not become ready within 10 seconds" >&2
exit 1
