#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	local repo_root
	repo_root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	mkdir -p "$XBERG_WORK/scripts/e2e/zig" "$XBERG_WORK/e2e/zig" "$XBERG_WORK/fixtures"
	mkdir -p "$XBERG_WORK/e2e/rust/target/release"
	cp "$repo_root/scripts/e2e/zig/test.sh" "$XBERG_WORK/scripts/e2e/zig/test.sh"
	xberg_stub mock-server \
		'printf "%s\n" "MOCK_SERVER_URL=http://127.0.0.1:8765"' \
		'printf "%s\n" '\''MOCK_SERVERS={"robots_disallow_path":"http://127.0.0.1:8766"}'\''' \
		'cat >/dev/null'
	cp "$XBERG_STUB_BIN/mock-server" "$XBERG_WORK/e2e/rust/target/release/mock-server"
	xberg_stub sleep \
		'if [ "$1" != 9999 ]; then /bin/sleep "$@"; fi'
	xberg_stub zig \
		'printf "%s\n" "$@" >"$XBERG_TRACE"' \
		'printf "%s\n" "$PWD" "$MOCK_SERVER_URL" "$MOCK_SERVER_ROBOTS_DISALLOW_PATH" >"$XBERG_TRACE.env"' \
		'printf "%s\n" "zig emitted a warning" >&2' \
		'exit "${ZIG_EXIT_STATUS:-0}"'
}

@test "zig e2e runner should_report_all_results_and_preserve_forwarded_arguments" {
	run bash "$XBERG_WORK/scripts/e2e/zig/test.sh" -Doptimize=ReleaseSafe --test-filter "robots allow"
	xberg_assert_status 0
	xberg_assert_output_contains 'zig emitted a warning'
	xberg_assert_trace build test --summary all \
		"-Dffi_path=$XBERG_WORK/target/release" \
		"-Dffi_include_path=$XBERG_WORK/crates/crawlberg-ffi/include" \
		-Doptimize=ReleaseSafe --test-filter "robots allow"
	xberg_assert_file "$XBERG_TRACE.env" "$(printf '%s\n' \
		"$XBERG_WORK/e2e/zig" 'http://127.0.0.1:8765' 'http://127.0.0.1:8766')"
}

@test "zig e2e runner should_propagate_the_compiler_failure_status" {
	export ZIG_EXIT_STATUS=37
	run bash "$XBERG_WORK/scripts/e2e/zig/test.sh"
	xberg_assert_status 37
	xberg_assert_output_contains 'zig emitted a warning'
	xberg_assert_trace build test --summary all \
		"-Dffi_path=$XBERG_WORK/target/release" \
		"-Dffi_include_path=$XBERG_WORK/crates/crawlberg-ffi/include"
}
