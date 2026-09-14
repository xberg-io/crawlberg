#!/usr/bin/env bats
#
# Contract tests for scripts/lib/common.sh.
#
# FAN-OUT SHARED: byte-identical in xberg, kreuzberg-lts and crawlberg, where the script under
# test is md5-identical. It must never name a product. Copy it with `cp`, never by hand.
#
# The functions here return values on stdout and statuses to the caller, so `run` is the right
# tool for most of them -- unlike library-paths.sh, nothing communicates by `export`.

setup() {
	bats_load_library xberg-bats
	xberg_setup
	LIB="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)/scripts/lib/common.sh"
}

# --- get_repo_root ------------------------------------------------------------------------------

@test "get_repo_root should_print_the_nearest_ancestor_holding_a_cargo_toml" {
	mkdir -p "$XBERG_WORK/repo/crates/deep/nested"
	: >"$XBERG_WORK/repo/Cargo.toml"

	run bash -c 'source "$1"; cd "$2" && get_repo_root' _ "$LIB" "$XBERG_WORK/repo/crates/deep/nested"

	xberg_assert_status 0
	xberg_assert_output "$XBERG_WORK/repo"
}

@test "get_repo_root should_prefer_the_closest_cargo_toml_when_several_are_nested" {
	mkdir -p "$XBERG_WORK/outer/inner/sub"
	: >"$XBERG_WORK/outer/Cargo.toml"
	: >"$XBERG_WORK/outer/inner/Cargo.toml"

	run bash -c 'source "$1"; cd "$2" && get_repo_root' _ "$LIB" "$XBERG_WORK/outer/inner/sub"

	xberg_assert_status 0
	xberg_assert_output "$XBERG_WORK/outer/inner"
}

@test "get_repo_root should_report_the_starting_directory_on_stderr_when_no_cargo_toml_exists" {
	# Runs from a directory with no Cargo.toml above it inside the sandbox. The real filesystem
	# root has none either, so the walk terminates at "/" and the error path is reached.
	mkdir -p "$XBERG_WORK/orphan"

	run bash -c 'source "$1"; cd "$2" && get_repo_root' _ "$LIB" "$XBERG_WORK/orphan"

	xberg_assert_status 1
	xberg_assert_output_contains "Could not find repository root"
	xberg_assert_output_contains "$XBERG_WORK/orphan"
}

# --- validate_repo_root -------------------------------------------------------------------------

@test "validate_repo_root should_accept_the_argument_when_it_holds_a_cargo_toml" {
	mkdir -p "$XBERG_WORK/repo"
	: >"$XBERG_WORK/repo/Cargo.toml"

	run bash -c 'source "$1"; validate_repo_root "$2"' _ "$LIB" "$XBERG_WORK/repo"

	xberg_assert_status 0
	xberg_assert_no_output
}

@test "validate_repo_root should_fall_back_to_the_repo_root_variable_when_no_argument_is_given" {
	mkdir -p "$XBERG_WORK/repo"
	: >"$XBERG_WORK/repo/Cargo.toml"

	run bash -c 'source "$1"; REPO_ROOT="$2" validate_repo_root' _ "$LIB" "$XBERG_WORK/repo"

	xberg_assert_status 0
}

@test "validate_repo_root should_fail_when_neither_argument_nor_variable_is_set" {
	run bash -c 'source "$1"; unset REPO_ROOT; validate_repo_root' _ "$LIB"

	xberg_assert_status 1
	xberg_assert_output_contains "REPO_ROOT not provided"
}

@test "validate_repo_root should_name_the_missing_manifest_when_the_directory_has_no_cargo_toml" {
	mkdir -p "$XBERG_WORK/empty"

	run bash -c 'source "$1"; validate_repo_root "$2"' _ "$LIB" "$XBERG_WORK/empty"

	xberg_assert_status 1
	xberg_assert_output_contains "$XBERG_WORK/empty/Cargo.toml"
}

# --- error_exit ---------------------------------------------------------------------------------

@test "error_exit should_exit_with_status_one_and_the_default_message_when_called_bare" {
	run bash -c 'source "$1"; error_exit' _ "$LIB"

	xberg_assert_status 1
	xberg_assert_output "Error: Unknown error"
}

@test "error_exit should_use_the_supplied_message_and_exit_code_when_both_are_given" {
	run bash -c 'source "$1"; error_exit "disk is full" 42' _ "$LIB"

	xberg_assert_status 42
	xberg_assert_output "Error: disk is full"
}

# --- get_platform -------------------------------------------------------------------------------

@test "get_platform should_return_the_runner_os_verbatim_when_it_is_set" {
	# Returned verbatim rather than normalised: CI already speaks GitHub's spelling, and
	# rewriting it here would disagree with the case labels in library-paths.sh.
	run bash -c 'source "$1"; RUNNER_OS=Windows get_platform' _ "$LIB"
	xberg_assert_output "Windows"
}

@test "get_platform should_map_uname_to_the_runner_spelling_when_runner_os_is_unset" {
	xberg_stub uname 'printf "%s\n" Darwin'
	run bash -c 'source "$1"; unset RUNNER_OS; get_platform' _ "$LIB"
	xberg_assert_output "macOS"

	xberg_stub uname 'printf "%s\n" Linux'
	run bash -c 'source "$1"; unset RUNNER_OS; get_platform' _ "$LIB"
	xberg_assert_output "Linux"

	xberg_stub uname 'printf "%s\n" MINGW64_NT-10.0'
	run bash -c 'source "$1"; unset RUNNER_OS; get_platform' _ "$LIB"
	xberg_assert_output "Windows"
}

@test "get_platform should_return_unknown_when_uname_reports_an_unrecognised_system" {
	xberg_stub uname 'printf "%s\n" FreeBSD'
	run bash -c 'source "$1"; unset RUNNER_OS; get_platform' _ "$LIB"
	xberg_assert_output "unknown"
}
