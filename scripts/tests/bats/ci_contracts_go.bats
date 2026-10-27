#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	SOURCE_ROOT="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	export REPO_ROOT="$XBERG_WORK/repo"
	mkdir -p "$REPO_ROOT/scripts/lib" "$REPO_ROOT/packages/ruby" "$REPO_ROOT/crates/crawlberg-ffi" "$REPO_ROOT/crates/crawlberg-node" "$REPO_ROOT/target/release" "$REPO_ROOT/home"
	cp "$SOURCE_ROOT/scripts/lib/common.sh" "$SOURCE_ROOT/scripts/lib/library-paths.sh" "$REPO_ROOT/scripts/lib/"
	printf '[workspace]\nmembers = []\nversion = "1.2.3"\n' >"$REPO_ROOT/Cargo.toml"
	export RUNNER_OS=Linux
	unset OS BUNDLE_PATH BUNDLE_GEMFILE CRAWLBERG_PDFIUM_PREBUILT ORT_LIB_LOCATION
	cd "$REPO_ROOT"
}

run_script() {
	local script="$1"
	shift
	run bash "$SOURCE_ROOT/scripts/ci/$script" "$@"
}

@test "Go diagnostics reports existing manifest and FFI header" {
	mkdir -p crates/crawlberg-ffi/include
	echo header >crates/crawlberg-ffi/include/crawlberg.h
	for cmd in rustc cargo go gcc pkg-config; do xberg_stub "$cmd" 'echo tool-version'; done
	run_script go/debug-build-environment.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'Cargo.toml exists'
	[[ "$output" =~ FFI\ header\ exists\ \([[:space:]]*1\ lines\) ]]
	xberg_assert_output_contains 'End of Diagnostics'
}

@test "Go diagnostics reports missing manifest and header" {
	rm Cargo.toml
	for cmd in rustc cargo go gcc pkg-config; do xberg_stub_exit "$cmd" 1; done
	run_script go/debug-build-environment.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'Cargo.toml not found'
	xberg_assert_output_contains 'FFI header not found'
	xberg_assert_output_contains 'rustc not found'
}

@test "Go path exporter writes CGO and runtime paths to workflow environment" {
	run_script go/export-library-paths-for-tests.sh
	xberg_assert_status 0
	grep -Fx "CGO_CFLAGS=-I$REPO_ROOT/crates/crawlberg-ffi/include" "$GITHUB_ENV"
	grep -Fx 'CGO_ENABLED=1' "$GITHUB_ENV"
	grep -F "LD_LIBRARY_PATH=$REPO_ROOT/target/release:" "$GITHUB_ENV"
	grep -Fx 'Version: 1.2.3' crates/crawlberg-ffi/crawlberg-ffi.pc
}

@test "Go path exporter refuses a nonrepository root" {
	rm Cargo.toml
	run_script go/export-library-paths-for-tests.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'Cargo.toml'
	xberg_assert_github_env ''
}

@test "Go executable preparation adds executable permissions to actual FFI files" {
	touch target/release/libcrawlberg_ffi.so
	chmod -x target/release/libcrawlberg_ffi.so
	run_script go/make-ffi-executable-unix.sh
	xberg_assert_status 0
	[ -x target/release/libcrawlberg_ffi.so ]
}

@test "Go executable preparation tolerates absent optional platform files" {
	run_script go/make-ffi-executable-unix.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'libcrawlberg_ffi.'
	[ ! -e target/release/libcrawlberg_ffi.so ]
}

@test "Go artifact mover copies platform library header and pkgconfig before deleting download" {
	mkdir -p ffi-download/x86_64-pc-windows-gnu
	echo unix >ffi-download/libcrawlberg_ffi.so
	echo windows >ffi-download/x86_64-pc-windows-gnu/libcrawlberg_ffi.dll
	echo header >ffi-download/crawlberg.h
	echo pkg >ffi-download/crawlberg-ffi.pc
	run_script go/move-downloaded-ffi-library.sh
	xberg_assert_status 0
	xberg_assert_file target/release/libcrawlberg_ffi.so unix
	xberg_assert_file target/x86_64-pc-windows-gnu/release/libcrawlberg_ffi.dll windows
	xberg_assert_file packages/go/v4/internal/ffi/crawlberg.h header
	xberg_assert_file crates/crawlberg-ffi/crawlberg-ffi.pc pkg
	[ ! -d ffi-download ]
}

@test "Go artifact mover rejects missing headers and preserves downloads for diagnosis" {
	mkdir -p ffi-download
	echo unix >ffi-download/libcrawlberg_ffi.so
	run_script go/move-downloaded-ffi-library.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'Header file crawlberg.h not found'
	[ -d ffi-download ]
}

@test "Go CGO diagnostic confirms pkgconfig and a real FFI file" {
	xberg_stub go 'echo go-version'
	xberg_stub_exit pkg-config 0
	touch target/release/libcrawlberg_ffi.so
	run_script go/verify-cgo-config.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'pkg-config can find crawlberg-ffi'
	xberg_assert_output_contains 'target/release/libcrawlberg_ffi.so'
}

@test "Go CGO diagnostic reports unavailable pkgconfig and absent FFI" {
	xberg_stub go 'echo go-version'
	xberg_stub_exit pkg-config 1
	run_script go/verify-cgo-config.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'pkg-config cannot find crawlberg-ffi'
	xberg_assert_output_contains 'No FFI library found'
}

@test "Go FFI verifier finds Windows GNU artifacts" {
	export RUNNER_OS=Windows
	mkdir -p target/x86_64-pc-windows-gnu/release
	touch target/x86_64-pc-windows-gnu/release/libcrawlberg_ffi.dll
	run_script go/verify-ffi-library.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'Found FFI library in GNU target'
}

@test "Go FFI verifier rejects absent Unix artifacts" {
	run_script go/verify-ffi-library.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'FFI library not found in target/release'
}
