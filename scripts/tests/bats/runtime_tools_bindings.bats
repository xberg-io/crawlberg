#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	local repo_root path
	repo_root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	for path in scripts/csharp scripts/e2e/csharp scripts/e2e/go scripts/e2e/swift scripts/go scripts/lib; do
		mkdir -p "$XBERG_WORK/$path"
		cp "$repo_root/$path/"*.sh "$XBERG_WORK/$path/"
	done
	mkdir -p "$XBERG_WORK/packages/"{csharp,go} "$XBERG_WORK/e2e/"{csharp,go,swift_e2e,rust/target/release}
	mkdir -p "$XBERG_WORK/crates/crawlberg-ffi" "$XBERG_WORK/fixtures" "$XBERG_WORK/target/release"
	printf '[workspace.package]\nversion = "1.12.3"\n' >"$XBERG_WORK/Cargo.toml"
	printf '#!/usr/bin/env bash\nprintf "download-pdfium\\n" >>"$XBERG_TRACE"\n' >"$XBERG_WORK/scripts/download_pdfium_runtime.sh"
	chmod +x "$XBERG_WORK/scripts/download_pdfium_runtime.sh"
	export RUNNER_OS=Fixture TESSDATA_PREFIX="$XBERG_WORK/tessdata"
	mkdir -p "$XBERG_WORK/target/tessdata"
	dd if=/dev/zero of="$XBERG_WORK/target/tessdata/eng.traineddata" bs=1000000 count=1 2>/dev/null
	dd if=/dev/zero of="$XBERG_WORK/target/tessdata/osd.traineddata" bs=100000 count=1 2>/dev/null
	xberg_stub_curl_offline
	xberg_stub go 'printf "go %s|cwd=%s|cgo=%s|backtrace=%s\n" "$*" "$PWD" "${CGO_ENABLED:-}" "${RUST_BACKTRACE:-}" >>"$XBERG_TRACE"' 'if [ "$1" = test ]; then exit "${TOOL_STATUS:-0}"; fi' 'if [ "$1" = install ]; then exit "${INSTALL_STATUS:-0}"; fi'
	xberg_stub dotnet 'printf "dotnet %s|cwd=%s|private=%s|ort=%s|mock=%s\n" "$*" "$PWD" "${CRAWLBERG_ALLOW_PRIVATE_NETWORK:-}" "${ORT_DYLIB_PATH:-}" "${MOCK_SERVER_URL:-}" >>"$XBERG_TRACE"' 'exit "${TOOL_STATUS:-0}"'
	xberg_stub gofmt 'printf "gofmt %s|cwd=%s\n" "$*" "$PWD" >>"$XBERG_TRACE"' 'printf "%s" "${GOFMT_OUTPUT:-}"' 'exit "${TOOL_STATUS:-0}"'
	unset CI VERBOSE_MODE RUST_BACKTRACE ORT_DYLIB_PATH MOCK_SERVER_URL CRAWLBERG_ALLOW_PRIVATE_NETWORK CGO_ENABLED
	cd "$XBERG_WORK" || return 1
}

@test "csharp package tests should_use_release_project_and_preserve_explicit_onnx_path" {
	export ORT_DYLIB_PATH="$XBERG_WORK/custom-ort.dylib"
	run bash scripts/csharp/test.sh
	xberg_assert_status 0
	xberg_assert_trace "dotnet test Crawlberg.Tests/Crawlberg.Tests.csproj -c Release|cwd=$XBERG_WORK/packages/csharp|private=|ort=$ORT_DYLIB_PATH|mock="
	export TOOL_STATUS=13
	run bash scripts/csharp/test.sh
	xberg_assert_status 13
}

@test "csharp e2e should_pass_diagnostics_mock_url_and_private_network_policy" {
	printf 'export MOCK_SERVER_URL=http://fixture.invalid:9234\n' >.mock-server.env
	export CRAWLBERG_ALLOW_PRIVATE_NETWORK=false
	run bash scripts/e2e/csharp/test.sh
	xberg_assert_status 0
	local result_dir="$XBERG_WORK/target/test-results/csharp-e2e"
	[ -d "$result_dir" ]
	xberg_assert_trace "dotnet test Crawlberg.E2eTests.csproj -c Release --logger console;verbosity=diagnostic --logger trx;LogFileName=csharp-e2e.trx --results-directory $result_dir --diag $result_dir/dotnet-test-diag.log --blame --blame-crash --blame-hang --blame-hang-timeout 20m --blame-hang-dump-type mini --blame-crash-dump-type mini|cwd=$XBERG_WORK/e2e/csharp|private=false|ort=|mock=http://fixture.invalid:9234"
}

@test "csharp e2e should_default_private_network_and_propagate_failed_tests" {
	export TOOL_STATUS=24
	run bash scripts/e2e/csharp/test.sh
	xberg_assert_status 24
	[[ "$(cat "$XBERG_TRACE")" == *'|private=true|ort=|mock=' ]]
}

@test "go e2e format should_run_in_generated_suite_and_propagate_failure" {
	run bash scripts/e2e/go/format.sh
	xberg_assert_status 0
	xberg_assert_trace "gofmt -w .|cwd=$XBERG_WORK/e2e/go"
	export TOOL_STATUS=11
	run bash scripts/e2e/go/format.sh
	xberg_assert_status 11
}

@test "go format check should_accept_clean_output_and_reject_reported_files" {
	run bash scripts/go/format_check.sh
	xberg_assert_status 0
	xberg_assert_trace "gofmt -l .|cwd=$XBERG_WORK/packages/go"
	export GOFMT_OUTPUT=unformatted.go
	run bash scripts/go/format_check.sh
	xberg_assert_status 1
}

@test "go install should_select_argument_over_environment_and_tidy_package_module" {
	export GOLANGCI_LINT_VERSION=v2.9.0
	run bash scripts/go/install.sh v2.10.0
	xberg_assert_status 0
	xberg_assert_trace \
		"go install github.com/golangci/golangci-lint/v2/cmd/golangci-lint@v2.10.0|cwd=$XBERG_WORK|cgo=|backtrace=" \
		"go mod download|cwd=$XBERG_WORK/packages/go|cgo=|backtrace=" \
		"go mod tidy|cwd=$XBERG_WORK/packages/go|cgo=|backtrace="
}

@test "go install should_select_environment_version_and_stop_after_install_failure" {
	export GOLANGCI_LINT_VERSION=v2.9.0 INSTALL_STATUS=12
	run bash scripts/go/install.sh
	xberg_assert_status 12
	xberg_assert_trace "go install github.com/golangci/golangci-lint/v2/cmd/golangci-lint@v2.9.0|cwd=$XBERG_WORK|cgo=|backtrace="
}

@test "go test should_configure_native_paths_and_enable_explicit_verbose_output" {
	run bash scripts/go/test.sh --verbose
	xberg_assert_status 0
	xberg_assert_trace 'download-pdfium' \
		"go version|cwd=$XBERG_WORK/packages/go|cgo=1|backtrace=" \
		"go test -timeout 10m -v ./...|cwd=$XBERG_WORK/packages/go|cgo=1|backtrace=1"
	[ -f "$XBERG_WORK/crates/crawlberg-ffi/crawlberg-ffi.pc" ]
	xberg_assert_output_contains 'CGO_CFLAGS:'
}

@test "go test should_preserve_failure_status_and_print_native_crash_guidance" {
	export TOOL_STATUS=27
	run bash scripts/go/test.sh
	xberg_assert_status 27
	xberg_assert_trace 'download-pdfium' \
		"go test -timeout 10m ./...|cwd=$XBERG_WORK/packages/go|cgo=1|backtrace=1"
	xberg_assert_output_contains 'Go tests failed with exit code 27'
	xberg_assert_output_contains 'segmentation fault'
}

@test "go debug should_forward_filter_race_coverage_and_full_backtraces" {
	run bash scripts/go/test-debug.sh --test TestHeaders --race --coverage
	xberg_assert_status 0
	xberg_assert_trace 'download-pdfium' \
		"go version|cwd=$XBERG_WORK/packages/go|cgo=1|backtrace=full" \
		"go test -v -timeout 10m -race -coverprofile=coverage.out -covermode=atomic -run TestHeaders ./...|cwd=$XBERG_WORK/packages/go|cgo=1|backtrace=full"
	xberg_assert_output_contains 'All tests passed!'
}

@test "go debug should_reject_unknown_option_without_invoking_go" {
	run bash scripts/go/test-debug.sh --typo
	xberg_assert_status 1
	xberg_assert_output_contains 'Unknown option: --typo'
	xberg_assert_trace 'download-pdfium'
}

@test "go debug should_return_original_test_failure_and_name_selected_pattern" {
	export TOOL_STATUS=26
	run bash scripts/go/test-debug.sh --test TestBroken
	xberg_assert_status 26
	xberg_assert_output_contains "Test 'TestBroken' failed with exit code 26"
	[[ "$output" != *'All tests passed!'* ]]
}

@test "swift e2e should_reject_missing_mock_server_before_running_swift" {
	xberg_stub_trace swift
	run bash scripts/e2e/swift/test.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'mock-server not found'
	xberg_assert_trace_empty
}

@test "swift e2e should_export_fixture_listeners_forward_filter_and_clean_owned_server" {
	cat >"$XBERG_WORK/e2e/rust/target/release/mock-server" <<'SERVER'
#!/usr/bin/env bash
printf 'MOCK_SERVER_URL=http://fixture.invalid:8000\n'
printf 'MOCK_SERVERS={"robots_disallow_path":"http://fixture.invalid:8001"}\n'
cat >/dev/null
SERVER
	chmod +x "$XBERG_WORK/e2e/rust/target/release/mock-server"
	xberg_stub swift 'printf "swift %s|cwd=%s|url=%s|robots=%s\n" "$*" "$PWD" "$MOCK_SERVER_URL" "$MOCK_SERVER_ROBOTS_DISALLOW_PATH" >>"$XBERG_TRACE"' 'exit "${TOOL_STATUS:-0}"'
	export TMPDIR="$XBERG_WORK/tmp"
	mkdir -p "$TMPDIR"
	run bash scripts/e2e/swift/test.sh --filter Headers
	xberg_assert_status 0
	xberg_assert_trace "swift test --filter Headers|cwd=$XBERG_WORK/e2e/swift_e2e|url=http://fixture.invalid:8000|robots=http://fixture.invalid:8001"
	[ "$(find "$TMPDIR" -mindepth 1 | wc -l | tr -d ' ')" -eq 0 ]
	export TOOL_STATUS=28
	run bash scripts/e2e/swift/test.sh
	xberg_assert_status 28
	[ "$(find "$TMPDIR" -mindepth 1 | wc -l | tr -d ' ')" -eq 0 ]
}

@test "go format check should_propagate_formatter_failure_with_empty_stdout" {
	export TOOL_STATUS=17
	xberg_stub gofmt 'printf "gofmt %s|cwd=%s\n" "$*" "$PWD" >>"$XBERG_TRACE"' 'printf "formatter read failure\n" >&2' 'exit 17'
	run bash scripts/go/format_check.sh
	xberg_assert_status 17
	xberg_assert_output 'formatter read failure'
	xberg_assert_trace "gofmt -l .|cwd=$XBERG_WORK/packages/go"
}
