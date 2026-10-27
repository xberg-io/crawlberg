#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	local repo_root
	repo_root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	mkdir -p "$XBERG_WORK/scripts/e2e/go" "$XBERG_WORK/scripts/lib" "$XBERG_WORK/e2e/go"
	mkdir -p "$XBERG_WORK/target/release" "$XBERG_WORK/crates/crawlberg-ffi"
	cp "$repo_root/scripts/e2e/go/test.sh" "$XBERG_WORK/scripts/e2e/go/test.sh"
	cp "$repo_root/scripts/lib/"{common,library-paths}.sh "$XBERG_WORK/scripts/lib/"
	printf '[workspace.package]\nversion = "1.0.0"\n' >"$XBERG_WORK/Cargo.toml"
	printf 'module nativecache\n\ngo 1.18\n' >"$XBERG_WORK/e2e/go/go.mod"
	cat >"$XBERG_WORK/e2e/go/native.go" <<'GO'
package nativecache

// int native_value(void);
import "C"

func nativeValue() int { return int(C.native_value()) }
GO

	export GOWORK=off GOFLAGS='' GOTOOLCHAIN=local GOCACHE="$BATS_TEST_TMPDIR/go-cache"
	unset RUNNER_OS CGO_LDFLAGS CGO_CFLAGS
	source "$XBERG_WORK/scripts/lib/library-paths.sh"
	setup_go_paths "$XBERG_WORK" >/dev/null
	cd "$XBERG_WORK/e2e/go"
}

build_native_archive() {
	printf 'int native_value(void) { return %s; }\n' "$1" >"$XBERG_WORK/native.c"
	cc -c "$XBERG_WORK/native.c" -o "$XBERG_WORK/native.o"
	ar rcs "$XBERG_WORK/target/release/libcrawlberg_ffi.a" "$XBERG_WORK/native.o"
}

write_native_test() {
	cat >"$XBERG_WORK/e2e/go/native_test.go" <<GO
package nativecache

import "testing"

func TestNativeValue(t *testing.T) {
	if got := nativeValue(); got != $1 {
		t.Fatalf("native value: got %d, want $1", got)
	}
}
GO
}

@test "go e2e runner should_relink_and_execute_tests_when_only_the_native_archive_changes" {
	build_native_archive 1
	write_native_test 1
	run go test ./...
	xberg_assert_status 0
	[[ "$output" != *"(cached)"* ]]
	run go test ./...
	xberg_assert_status 0
	xberg_assert_output_contains '(cached)'

	build_native_archive 2
	run bash "$XBERG_WORK/scripts/e2e/go/test.sh"
	xberg_assert_status 1
	xberg_assert_output_contains 'native value: got 2, want 1'
	xberg_assert_output_contains '--- FAIL: TestNativeValue'

	write_native_test 2
	local attempt
	for attempt in 1 2; do
		run bash "$XBERG_WORK/scripts/e2e/go/test.sh"
		xberg_assert_status 0
		xberg_assert_output_contains '--- PASS: TestNativeValue'
		[ "$(printf '%s\n' "$output" | grep -c '^=== RUN')" -eq 1 ]
		[[ "$output" != *"(cached)"* ]]
	done
}
