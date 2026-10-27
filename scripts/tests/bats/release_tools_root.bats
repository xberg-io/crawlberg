#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	export HOME="$XBERG_WORK/home"
	mkdir -p "$HOME" "$XBERG_WORK/scripts"
	local root
	root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	cp "$root"/scripts/{chrome-memwatch.sh,download_pdfium_runtime.sh,install.sh,setup-swift-bridge.sh} \
		"$XBERG_WORK/scripts/"
	cd "$XBERG_WORK"
	xberg_stub uname 'case "$1" in -s) echo "${TEST_OS:-Linux}";; -m) echo "${TEST_ARCH:-x86_64}";; esac'
	export TMPDIR="$XBERG_WORK/tmp"
	mkdir -p "$TMPDIR"
}

@test "chrome memwatch should_log_orphan_helpers_after_one_bounded_sample" {
	xberg_stub ps 'echo unrelated'
	xberg_stub pgrep 'printf "%s\n" "1 chrome" "2 chrome" "3 chrome"'
	export LOG_FILE="$XBERG_WORK/watch.log"
	run bash -c 'sleep() { exit 0; }; export -f sleep; bash "$1"' _ "$XBERG_WORK/scripts/chrome-memwatch.sh"
	xberg_assert_status 0
	xberg_assert_output_contains 'ORPHAN-HELPERS count=3'
	[[ "$(cat "$LOG_FILE")" == *'runner=none helpers=3'* ]]
}

@test "chrome memwatch should_track_active_runner_without_false_orphan_warning" {
	xberg_stub ps 'echo "cargo test --workspace"'
	xberg_stub pgrep 'echo "1 chrome"'
	export LOG_FILE="$XBERG_WORK/watch.log"
	run bash -c 'sleep() { exit 0; }; export -f sleep; bash "$1"' _ "$XBERG_WORK/scripts/chrome-memwatch.sh"
	xberg_assert_status 0
	[[ "$output" != *ORPHAN-HELPERS* ]]
	[[ "$(cat "$LOG_FILE")" == *'SESSION-START runner=rust baseline_helpers=1'* ]]
}

@test "pdfium downloader should_stage_real_archive_in_both_profiles" {
	mkdir -p fixture/lib
	printf pdfium >fixture/lib/libpdfium.so
	tar -czf "$XBERG_WORK/pdfium.tgz" -C fixture lib
	xberg_stub_curl_file "$XBERG_WORK/pdfium.tgz"
	run bash scripts/download_pdfium_runtime.sh 9999
	xberg_assert_status 0
	xberg_assert_output_contains 'Downloading Pdfium 9999 for linux/x64'
	xberg_assert_file target/release/libpdfium.so pdfium
	xberg_assert_file target/debug/libpdfium.so pdfium
}

@test "pdfium downloader should_reject_unsupported_platform_before_network" {
	export TEST_OS=Plan9
	xberg_stub_curl_offline
	run bash scripts/download_pdfium_runtime.sh
	xberg_assert_status 1
	xberg_assert_output 'Unsupported platform: Plan9'
	[ ! -d target ]
}

@test "installer should_install_versioned_archive_and_runtime_libraries_inside_sandbox" {
	local stage=crawlberg-cli-x86_64-unknown-linux-musl
	mkdir -p "fixture/$stage/lib"
	printf '#!/bin/sh\necho crawlberg 1.12.3\n' >"fixture/$stage/crawlberg"
	printf native >"fixture/$stage/crawlberg.bin"
	printf runtime >"fixture/$stage/lib/libfixture.so"
	tar -czf "$XBERG_WORK/cli.tgz" -C fixture "$stage"
	xberg_stub_curl_file "$XBERG_WORK/cli.tgz"
	export CRAWLBERG_INSTALL="$XBERG_WORK/install" CRAWLBERG_VERSION=1.12.3
	run bash scripts/install.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'Verified: crawlberg 1.12.3'
	xberg_assert_file "$CRAWLBERG_INSTALL/lib/libfixture.so" runtime
	xberg_assert_file "$CRAWLBERG_INSTALL/crawlberg.bin" native
	[ -x "$CRAWLBERG_INSTALL/crawlberg" ]
}

@test "installer should_reject_archive_without_binary" {
	mkdir fixture
	printf unrelated >fixture/readme
	tar -czf "$XBERG_WORK/bad.tgz" -C fixture readme
	xberg_stub_curl_file "$XBERG_WORK/bad.tgz"
	export CRAWLBERG_INSTALL="$XBERG_WORK/install" CRAWLBERG_VERSION=1.12.3
	run bash scripts/install.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'binary not found in archive'
	xberg_assert_file_absent "$CRAWLBERG_INSTALL/crawlberg"
}

@test "swift bridge setup should_merge_headers_and_import_modules_from_real_build_output" {
	local out=target/release/build/crawlberg-swift-fixture/out
	mkdir -p "$out/crawlberg-swift"
	printf core-header >"$out/SwiftBridgeCore.h"
	printf bridge-header >"$out/crawlberg-swift/crawlberg-swift.h"
	printf core-swift >"$out/SwiftBridgeCore.swift"
	printf bridge-swift >"$out/crawlberg-swift/crawlberg-swift.swift"
	run bash scripts/setup-swift-bridge.sh
	xberg_assert_status 0
	xberg_assert_file packages/swift/Sources/RustBridgeC/RustBridgeC.h core-headerbridge-header
	xberg_assert_file packages/swift/Sources/RustBridge/crawlberg-swift.swift $'import RustBridgeC\nbridge-swift'
}

@test "swift bridge setup should_fail_when_no_build_output_exists" {
	run bash scripts/setup-swift-bridge.sh debug
	xberg_assert_status 1
	xberg_assert_output_contains 'Could not find swift-bridge build output in target/debug/build/'
	[ ! -d packages ]
}
