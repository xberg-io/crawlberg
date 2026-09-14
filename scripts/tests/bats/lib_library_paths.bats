#!/usr/bin/env bats
#
# Contract tests for scripts/lib/library-paths.sh.
#
# FAN-OUT SHARED: this file is meant to be byte-identical in xberg, kreuzberg-lts and crawlberg.
# The three copies of the script under test differ only by a product token, so the suite must
# never type one -- FFI_CRATE and LIB_NAME are derived from the repo in setup_file(). Copy it with
# `cp`, never by hand-editing one copy.
#
# These tests call the functions DIRECTLY rather than through `run`. Every function here
# communicates by `export`, and `run` forks a subshell, so the export is discarded before the
# assertion sees it -- a `run`-based suite passes against a function that exports nothing. `run`
# appears only where the status or the output IS the contract. ~keep

setup_file() {
	REPO_ROOT="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	export REPO_ROOT

	local ffi_dir
	ffi_dir="$(find "$REPO_ROOT/crates" -maxdepth 1 -type d -name '*-ffi' | head -n 1)"
	[ -n "$ffi_dir" ] || {
		echo "no crates/*-ffi crate under $REPO_ROOT" >&2
		return 1
	}
	FFI_CRATE="$(basename "$ffi_dir")"
	LIB_NAME="${FFI_CRATE//-/_}"
	export FFI_CRATE LIB_NAME
}

setup() {
	bats_load_library xberg-bats
	xberg_setup

	LIB="$REPO_ROOT/scripts/lib/library-paths.sh"

	# Start every appended-to variable from a known value. Without this the assertions inherit
	# whatever the developer's login shell exported, and the suite is neither independent nor
	# reproducible. ~keep
	export LD_LIBRARY_PATH='' DYLD_LIBRARY_PATH='' DYLD_FALLBACK_LIBRARY_PATH=''
	export PKG_CONFIG_PATH='' CGO_CFLAGS='' CGO_LDFLAGS='' CARGO_TARGET_DIR=''
	unset ORT_LIB_LOCATION RUNNER_OS CGO_ENABLED
}

# --- setup_onnx_paths ---------------------------------------------------------------------------

@test "setup_onnx_paths should_prepend_ort_lib_to_ld_library_path_when_platform_is_linux" {
	source "$LIB"
	export RUNNER_OS=Linux ORT_LIB_LOCATION=/opt/ort
	setup_onnx_paths >/dev/null
	[ "$LD_LIBRARY_PATH" = "/opt/ort:" ]
	[ -z "$DYLD_LIBRARY_PATH" ]
}

@test "setup_onnx_paths should_set_both_dyld_variables_when_platform_is_macos" {
	source "$LIB"
	export RUNNER_OS=macOS ORT_LIB_LOCATION=/opt/ort
	setup_onnx_paths >/dev/null
	[ "$DYLD_LIBRARY_PATH" = "/opt/ort:" ]
	[ "$DYLD_FALLBACK_LIBRARY_PATH" = "/opt/ort:" ]
	[ -z "$LD_LIBRARY_PATH" ]
}

@test "setup_onnx_paths should_leave_every_library_path_unset_when_ort_lib_location_is_empty" {
	source "$LIB"
	export RUNNER_OS=Linux
	setup_onnx_paths >/dev/null
	[ -z "$LD_LIBRARY_PATH" ]
	[ -z "$DYLD_LIBRARY_PATH" ]
}

@test "setup_onnx_paths should_fall_back_to_uname_when_runner_os_is_unset" {
	source "$LIB"
	xberg_stub uname 'printf "%s\n" Linux'
	export ORT_LIB_LOCATION=/opt/ort
	setup_onnx_paths >/dev/null
	[ "$LD_LIBRARY_PATH" = "/opt/ort:" ]
}

# --- setup_rust_ffi_paths -----------------------------------------------------------------------

@test "setup_rust_ffi_paths should_not_touch_library_paths_when_target_release_is_absent" {
	source "$LIB"
	export RUNNER_OS=Linux
	setup_rust_ffi_paths "$XBERG_WORK" >/dev/null
	[ -z "$LD_LIBRARY_PATH" ]
}

@test "setup_rust_ffi_paths should_prepend_target_release_when_the_directory_exists_on_linux" {
	source "$LIB"
	export RUNNER_OS=Linux
	mkdir -p "$XBERG_WORK/target/release"
	setup_rust_ffi_paths "$XBERG_WORK" >/dev/null
	[ "$LD_LIBRARY_PATH" = "$XBERG_WORK/target/release:" ]
}

@test "setup_rust_ffi_paths should_set_both_dyld_variables_when_the_directory_exists_on_macos" {
	source "$LIB"
	export RUNNER_OS=macOS
	mkdir -p "$XBERG_WORK/target/release"
	setup_rust_ffi_paths "$XBERG_WORK" >/dev/null
	[ "$DYLD_LIBRARY_PATH" = "$XBERG_WORK/target/release:" ]
	[ "$DYLD_FALLBACK_LIBRARY_PATH" = "$XBERG_WORK/target/release:" ]
}

@test "setup_rust_ffi_paths should_fall_back_to_repo_root_when_the_argument_is_empty" {
	# `${1:-${REPO_ROOT:-}}` means an empty argument is NOT "no repo root" -- it falls through to
	# the environment. Asserting the fallback is the contract; asserting "does nothing" would pass
	# only on a machine where $REPO_ROOT/target/release happens not to exist. ~keep
	source "$LIB"
	export RUNNER_OS=Linux REPO_ROOT="$XBERG_WORK"
	mkdir -p "$XBERG_WORK/target/release"
	setup_rust_ffi_paths "" >/dev/null
	[ "$LD_LIBRARY_PATH" = "$XBERG_WORK/target/release:" ]
}

@test "setup_rust_ffi_paths should_write_nothing_when_neither_argument_nor_repo_root_is_set" {
	source "$LIB"
	export RUNNER_OS=Linux
	unset REPO_ROOT
	setup_rust_ffi_paths "" >/dev/null
	[ -z "$LD_LIBRARY_PATH" ]
}

# --- setup_go_paths -----------------------------------------------------------------------------

@test "setup_go_paths should_write_the_cargo_toml_version_into_the_generated_pc_file" {
	source "$LIB"
	export RUNNER_OS=Linux
	mkdir -p "$XBERG_WORK/crates/$FFI_CRATE"
	printf '[workspace.package]\nversion = "9.9.9"\n' >"$XBERG_WORK/Cargo.toml"

	setup_go_paths "$XBERG_WORK" >/dev/null

	local version_line
	version_line="$(grep '^Version:' "$XBERG_WORK/crates/$FFI_CRATE/$FFI_CRATE.pc")"
	[ "$version_line" = "Version: 9.9.9" ] || {
		echo "expected 'Version: 9.9.9', got '$version_line'" >&2
		return 1
	}
}

@test "setup_go_paths should_not_overwrite_an_existing_pc_file_when_one_is_present" {
	source "$LIB"
	export RUNNER_OS=Linux
	mkdir -p "$XBERG_WORK/crates/$FFI_CRATE"
	printf '[workspace.package]\nversion = "9.9.9"\n' >"$XBERG_WORK/Cargo.toml"
	printf 'HAND-WRITTEN\n' >"$XBERG_WORK/crates/$FFI_CRATE/$FFI_CRATE.pc"

	setup_go_paths "$XBERG_WORK" >/dev/null

	xberg_assert_file "$XBERG_WORK/crates/$FFI_CRATE/$FFI_CRATE.pc" "HAND-WRITTEN"
}

@test "setup_go_paths should_emit_the_repo_ffi_library_name_in_cgo_ldflags_on_linux" {
	source "$LIB"
	export RUNNER_OS=Linux
	mkdir -p "$XBERG_WORK/crates/$FFI_CRATE"
	: >"$XBERG_WORK/Cargo.toml"

	setup_go_paths "$XBERG_WORK" >/dev/null

	[ "$CGO_LDFLAGS" = "-L$XBERG_WORK/target/release -l$LIB_NAME -Wl,-rpath,$XBERG_WORK/target/release" ]
	[ "$CGO_ENABLED" = "1" ]
}

@test "setup_go_paths should_point_pkg_config_path_at_the_ffi_crate_directory" {
	source "$LIB"
	export RUNNER_OS=Linux
	mkdir -p "$XBERG_WORK/crates/$FFI_CRATE"
	: >"$XBERG_WORK/Cargo.toml"

	setup_go_paths "$XBERG_WORK" >/dev/null

	case "$PKG_CONFIG_PATH" in
	"$XBERG_WORK/crates/$FFI_CRATE"*) ;;
	*) return 1 ;;
	esac
}

@test "setup_go_paths should_write_nothing_when_neither_argument_nor_repo_root_is_set" {
	source "$LIB"
	unset REPO_ROOT
	setup_go_paths "" >/dev/null
	[ -z "$PKG_CONFIG_PATH" ]
}

# --- verify_pkg_config --------------------------------------------------------------------------

@test "verify_pkg_config should_return_zero_when_pkg_config_resolves_the_ffi_package" {
	source "$LIB"
	xberg_stub pkg-config 'exit 0'
	run verify_pkg_config
	xberg_assert_status 0
}

@test "verify_pkg_config should_name_the_ffi_package_on_stderr_when_it_cannot_be_found" {
	source "$LIB"
	xberg_stub pkg-config 'exit 1'
	run verify_pkg_config
	xberg_assert_status 1
	xberg_assert_output_contains "Error: pkg-config cannot find $FFI_CRATE"
	xberg_assert_output_contains "PKG_CONFIG_PATH="
}

# --- _get_path_separator ------------------------------------------------------------------------

@test "_get_path_separator should_return_a_semicolon_when_the_platform_is_a_windows_variant" {
	source "$LIB"
	[ "$(_get_path_separator MINGW64_NT-10.0)" = ";" ]
	[ "$(_get_path_separator Windows)" = ";" ]
}

@test "_get_path_separator should_return_a_colon_when_the_platform_is_not_windows" {
	source "$LIB"
	[ "$(_get_path_separator Linux)" = ":" ]
	[ "$(_get_path_separator Darwin)" = ":" ]
}
