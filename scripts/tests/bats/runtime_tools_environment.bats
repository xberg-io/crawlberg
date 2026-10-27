#!/usr/bin/env bats

setup() {
	RUNTIME_ORIGINAL_PATH="$PATH"
	bats_load_library xberg-bats
	xberg_setup
	local repo_root path
	repo_root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	for path in scripts/ci/rust scripts/ci/validate; do
		mkdir -p "$XBERG_WORK/$path"
	done
	cp "$repo_root/scripts/ci/rust/"{pre-build-diagnostics,prepare-chrome}.sh "$XBERG_WORK/scripts/ci/rust/"
	cp "$repo_root/scripts/ci/validate/"{cleanup-after-lint,show-disk-space}.sh "$XBERG_WORK/scripts/ci/validate/"
	xberg_stub rustc 'echo rustc-fixture; exit "${RUSTC_STATUS:-0}"'
	xberg_stub cargo 'echo cargo-fixture'
	xberg_stub rustup 'printf "%s\n" "${TARGETS:-fixture-target (installed)}"'
	xberg_stub df 'printf "df %s\n" "$*" >>"$XBERG_TRACE"' 'echo disk-fixture; exit "${DF_STATUS:-0}"'
	cd "$XBERG_WORK" || return 1
}

teardown() {
	export PATH="$RUNTIME_ORIGINAL_PATH"
}

@test "build diagnostics should_report_toolchains_targets_and_runner_environment" {
	run env RUNNER_OS=Linux RUNNER_ARCH=ARM64 RUSTFLAGS=-Dwarnings bash scripts/ci/rust/pre-build-diagnostics.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'OS: Linux'
	xberg_assert_output_contains 'Arch: ARM64'
	xberg_assert_output_contains 'rustc-fixture'
	xberg_assert_output_contains 'cargo-fixture'
	xberg_assert_output_contains 'fixture-target (installed)'
	xberg_assert_output_contains 'RUSTFLAGS=-Dwarnings'
	xberg_assert_trace 'df -h /'
}

@test "build diagnostics should_report_no_installed_targets_and_use_disk_fallback" {
	export TARGETS=plain-target DF_STATUS=12
	xberg_stub du 'printf "fallback-disk\n"'
	run bash scripts/ci/rust/pre-build-diagnostics.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'No targets installed'
	xberg_assert_output_contains 'fallback-disk'
}

@test "build diagnostics should_stop_on_broken_rust_toolchain" {
	export RUSTC_STATUS=18
	run bash scripts/ci/rust/pre-build-diagnostics.sh
	xberg_assert_status 18
	[[ "$output" != *'cargo-fixture'* ]]
	xberg_assert_trace_empty
}

@test "prepare chrome should_publish_valid_linux_executable_without_copying" {
	export CHROME="$XBERG_WORK/chrome"
	printf '#!/bin/sh\nexit 0\n' >"$CHROME"
	chmod +x "$CHROME"
	xberg_stub uname 'echo Linux'
	xberg_stub_exit ditto 99
	run bash scripts/ci/rust/prepare-chrome.sh
	xberg_assert_status 0
	xberg_assert_output "$CHROME"
	xberg_assert_github_output "chrome-path=$CHROME"
}

@test "prepare chrome should_reject_missing_browser_and_missing_configuration" {
	run env CHROME="$XBERG_WORK/missing" bash scripts/ci/rust/prepare-chrome.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'Chrome executable is missing'
	xberg_assert_github_output ''
	run env -u CHROME bash scripts/ci/rust/prepare-chrome.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'CHROME must name the installed browser executable'
}

@test "prepare chrome should_repackage_extensionless_macos_bundle_and_publish_new_path" {
	export CHROME="$XBERG_WORK/arm64/Contents/MacOS/Chrome" RUNNER_TEMP="$XBERG_WORK/prepared"
	mkdir -p "$(dirname "$CHROME")" "$RUNNER_TEMP"
	touch "$XBERG_WORK/arm64/Contents/Info.plist"
	printf '#!/bin/sh\nexit 0\n' >"$CHROME"
	chmod +x "$CHROME"
	xberg_stub uname 'echo Darwin'
	xberg_stub ditto 'cp -R "$1" "$2"'
	run bash scripts/ci/rust/prepare-chrome.sh
	xberg_assert_status 0
	[[ "$output" == "$RUNNER_TEMP"/crawlberg-chrome.*/Chrome.app/Contents/MacOS/Chrome ]]
	[ -x "$output" ]
	xberg_assert_github_output "chrome-path=$output"
}

@test "prepare chrome should_reject_extensionless_bundle_without_plist" {
	export CHROME="$XBERG_WORK/arm64/Contents/MacOS/Chrome"
	mkdir -p "$(dirname "$CHROME")"
	touch "$CHROME"
	chmod +x "$CHROME"
	xberg_stub uname 'echo Darwin'
	run bash scripts/ci/rust/prepare-chrome.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'Chrome bundle has no Info.plist'
	xberg_assert_github_output ''
}

@test "show disk should_render_custom_label_and_both_disk_queries" {
	run bash scripts/ci/validate/show-disk-space.sh 'After checks'
	xberg_assert_status 0
	xberg_assert_output_contains '=== After checks ==='
	xberg_assert_output_contains 'Disk info:'
	xberg_assert_output_contains 'disk-fixture'
	xberg_assert_trace 'df -h /' 'df -B1 /'
}

@test "show disk should_tolerate_unsupported_byte_flag_but_fail_initial_query" {
	xberg_stub df 'if [ "$1" = -B1 ]; then exit 14; fi; echo disk-fixture'
	run bash scripts/ci/validate/show-disk-space.sh
	xberg_assert_status 0
	xberg_assert_output_contains '=== Disk space ==='
	xberg_stub_exit df 16
	run bash scripts/ci/validate/show-disk-space.sh
	xberg_assert_status 16
}

@test "lint cleanup should_remove_only_incremental_and_named_cache_directories" {
	mkdir -p target/{debug,release}/incremental target/debug/build/crawlberg-ffi-123/out/pdfium
	mkdir -p target/release/build/crawlberg-ffi-456/out/pdfium target/debug/keep lib/__pycache__ lib/.pytest_cache
	printf keep >target/debug/keep/artifact
	run bash scripts/ci/validate/cleanup-after-lint.sh
	xberg_assert_status 0
	[ ! -e target/debug/incremental ]
	[ ! -e target/release/incremental ]
	[ ! -e target/debug/build/crawlberg-ffi-123/out/pdfium ]
	[ ! -e target/release/build/crawlberg-ffi-456/out/pdfium ]
	[ ! -e lib/__pycache__ ]
	[ ! -e lib/.pytest_cache ]
	xberg_assert_file target/debug/keep/artifact keep
	xberg_assert_output_contains 'Disk space after cleanup'
}

@test "lint cleanup should_warn_on_pdfium_cleanup_failure_and_finish_diagnostics" {
	mkdir -p target/debug/build
	xberg_stub find 'printf "find %s\n" "$*" >>"$XBERG_TRACE"' 'exit 9'
	xberg_stub rm 'printf "rm %s\n" "$*" >>"$XBERG_TRACE"' 'exit 8'
	run bash scripts/ci/validate/cleanup-after-lint.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'Warning: Failed to clean some debug pdfium directories'
	xberg_assert_output_contains 'Disk space after cleanup'
	xberg_assert_trace 'rm -rf target/debug/incremental' 'rm -rf target/release/incremental' \
		'find target/debug/build -type d -name pdfium -path */crawlberg-ffi-*/out/pdfium -delete' \
		'find . -type d -name __pycache__ -delete' 'find . -type d -name .pytest_cache -delete' \
		'df -h /' 'df -B1 /'
}
