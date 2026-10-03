#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	REPO_ROOT="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	SCRIPT="$REPO_ROOT/scripts/ci/rust/run-unit-tests.sh"
	xberg_stub_trace cargo
}

@test "run-unit-tests should_pass_no_fail_fast_to_all_three_test_runs" {
	local workspace_command="cargo test --workspace"
	workspace_command+=" --exclude crawlberg --exclude crawlberg-py --exclude crawlberg-node"
	workspace_command+=" --exclude crawlberg-php --exclude crawlberg-wasm --exclude crawlberg-cli"
	workspace_command+=" --all-features --no-fail-fast --verbose"

	run env REPO_ROOT="$REPO_ROOT" "$SCRIPT"

	xberg_assert_status 0
	xberg_assert_trace \
		"cargo test -p crawlberg --all-features --no-fail-fast --verbose" \
		"$workspace_command" \
		"cargo test -p crawlberg-cli --features all --no-fail-fast --verbose"
}
