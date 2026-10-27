#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	local root
	root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	mkdir -p "$XBERG_WORK/scripts/task" "$XBERG_WORK/e2e/rust/target/release" \
		"$XBERG_WORK/e2e/dart" "$XBERG_WORK/fixtures" "$XBERG_WORK/tmp"
	cp "$root/scripts/task/dart-e2e-test.sh" "$XBERG_WORK/scripts/task/"
	export TMPDIR="$XBERG_WORK/tmp"
	export REAL_MKTEMP="$(command -v mktemp)"
	xberg_stub mktemp 'exec "$REAL_MKTEMP" "$@" "$TMPDIR/owned.XXXXXX"'
	cat >"$XBERG_WORK/server.py" <<'PY'
import os
import signal
from pathlib import Path

Path(os.environ["XBERG_WORK"], "server.pid").write_text(str(os.getpid()))
print("MOCK_SERVER_URL=http://127.0.0.1:9000", flush=True)
print('MOCK_SERVERS={"robots-disallow":"http://127.0.0.1:9001"}', flush=True)
while True:
    signal.pause()
PY
	xberg_stub mock-server 'exec python3 "$XBERG_WORK/server.py" 3>&-'
	cp "$XBERG_STUB_BIN/mock-server" "$XBERG_WORK/e2e/rust/target/release/mock-server"
	xberg_stub dart \
		'kill -0 "$(cat "$XBERG_WORK/server.pid")" || exit 99' \
		'[ "$(find "$TMPDIR" -mindepth 1 -maxdepth 1 | wc -l | tr -d " ")" -eq 1 ] || exit 98' \
		'printf "%s\n" "$*" "$MOCK_SERVER_URL" "$MOCK_SERVER_ROBOTS_DISALLOW" "server-alive" > "$XBERG_TRACE"' \
		'exit "$DART_EXIT"'
	cd "$XBERG_WORK"
}

teardown() {
	local pid command_line
	if [ -f "$XBERG_WORK/server.pid" ]; then
		pid="$(cat "$XBERG_WORK/server.pid")"
		command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
		if [[ "$command_line" == *"$XBERG_WORK/server.py"* ]]; then
			kill "$pid" 2>/dev/null || true
		fi
	fi
}

assert_dart_cleanup() {
	run bash scripts/task/dart-e2e-test.sh
	xberg_assert_status "$DART_EXIT"
	xberg_assert_trace 'test --compiler=source' 'http://127.0.0.1:9000' \
		'http://127.0.0.1:9001' 'server-alive'
	local pid
	pid="$(cat "$XBERG_WORK/server.pid")"
	if kill -0 "$pid" 2>/dev/null; then
		echo "owned mock server remains alive after Dart exited: $pid" >&2
		return 1
	fi
	[ "$(find "$TMPDIR" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" -eq 0 ]
}

@test "dart e2e should_reap_owned_mock_server_and_remove_output_after_success" {
	export DART_EXIT=0
	assert_dart_cleanup
}

@test "dart e2e should_reap_owned_mock_server_and_preserve_failure_status" {
	export DART_EXIT=38
	assert_dart_cleanup
}
