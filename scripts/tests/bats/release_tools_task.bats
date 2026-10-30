#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	# ~keep Finite mock servers need no signal; keep cleanup inside the sandbox.
	kill() { :; }
	export -f kill
	local root
	root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	mkdir -p "$XBERG_WORK/scripts/task"
	local script
	for script in brew-e2e-test dart-e2e-test frb-codegen-ensure sync-bats-lib; do
		cp "$root/scripts/task/$script.sh" "$XBERG_WORK/scripts/task/"
	done
	export HOME="$XBERG_WORK/home" TMPDIR="$XBERG_WORK/tmp"
	mkdir -p "$HOME" "$TMPDIR"
	xberg_stub git 'printf "%s\n" "$XBERG_WORK"'
	cd "$XBERG_WORK"
}

prepare_mock_server() {
	mkdir -p e2e/rust/target/release e2e/brew e2e/dart fixtures
	xberg_stub mock-server \
		'printf "%s\n" "MOCK_SERVER_URL=http://127.0.0.1:9000"' \
		'printf "%s\n" '\''MOCK_SERVERS={"robots-disallow":"http://127.0.0.1:9001"}'\'''
	cp "$XBERG_STUB_BIN/mock-server" e2e/rust/target/release/mock-server
}

@test "brew e2e task should_forward_mock_server_environment_and_return_consumer_status" {
	prepare_mock_server
	printf 'printf "%%s\\n" "$MOCK_SERVER_URL" "$MOCK_SERVER_ROBOTS_DISALLOW" > "$XBERG_TRACE"\nexit 31\n' \
		>e2e/brew/run_tests.sh
	run bash scripts/task/brew-e2e-test.sh
	xberg_assert_status 31
	xberg_assert_trace 'http://127.0.0.1:9000' 'http://127.0.0.1:9001'
}

@test "brew e2e task should_reject_missing_mock_server_before_running_tests" {
	run bash scripts/task/brew-e2e-test.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'mock-server binary not found'
	xberg_assert_trace_empty
}

@test "dart e2e task should_use_source_compiler_and_forward_fixture_environment" {
	prepare_mock_server
	xberg_stub dart 'printf "%s\n" "$*" "$MOCK_SERVER_URL" "$MOCK_SERVER_ROBOTS_DISALLOW" > "$XBERG_TRACE"'
	run bash scripts/task/dart-e2e-test.sh
	xberg_assert_status 0
	xberg_assert_trace 'test --compiler=source' 'http://127.0.0.1:9000' 'http://127.0.0.1:9001'
}

@test "dart e2e task should_propagate_dart_failure" {
	prepare_mock_server
	xberg_stub_exit dart 38
	run bash scripts/task/dart-e2e-test.sh
	xberg_assert_status 38
}

prepare_frb() {
	mkdir -p packages/dart/rust
	printf 'flutter_rust_bridge = "=2.13.0"\n' >packages/dart/rust/Cargo.toml
	xberg_stub flutter_rust_bridge_codegen 'echo "flutter_rust_bridge_codegen ${FRB_INSTALLED:-2.13.0}"'
	xberg_stub_trace cargo
}

@test "frb ensure should_accept_exact_runtime_version_without_installing" {
	prepare_frb
	run bash scripts/task/frb-codegen-ensure.sh
	xberg_assert_status 0
	xberg_assert_no_output
	xberg_assert_trace_empty
}

@test "frb ensure should_reject_still_mismatched_codegen_after_install_attempt" {
	prepare_frb
	export FRB_INSTALLED=2.12.0
	run bash scripts/task/frb-codegen-ensure.sh
	xberg_assert_status 1
	xberg_assert_output_contains "reports '2.12.0'"
	xberg_assert_trace 'cargo install flutter_rust_bridge_codegen --version 2.13.0 --locked --force'
}

@test "bats library sync should_vendor_and_verify_downloaded_immutable_bytes" {
	export BATS_LIB_REF=v1.2.3 BATS_LIB_DEST="$XBERG_WORK/vendor"
	xberg_stub_curl_body fixture-library
	run bash scripts/task/sync-bats-lib.sh sync
	xberg_assert_status 0
	xberg_assert_file "$BATS_LIB_DEST/load.bash" fixture-library
	run bash scripts/task/sync-bats-lib.sh --check
	xberg_assert_status 0
	xberg_assert_output_contains 'matches xberg-io/actions@v1.2.3'
}

@test "bats library sync should_reject_drift_from_immutable_source" {
	export BATS_LIB_REF=v1.2.3 BATS_LIB_DEST="$XBERG_WORK/vendor"
	mkdir -p "$BATS_LIB_DEST"
	printf stale >"$BATS_LIB_DEST/load.bash"
	xberg_stub_curl_body fixture-library
	run bash scripts/task/sync-bats-lib.sh --check
	xberg_assert_status 1
	xberg_assert_output_contains 'has drifted from xberg-io/actions@v1.2.3'
	xberg_assert_file "$BATS_LIB_DEST/load.bash" stale
}
