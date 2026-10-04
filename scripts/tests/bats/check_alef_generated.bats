#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	REPO_ROOT="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	SCRIPT="$REPO_ROOT/scripts/ci/check-alef-generated.sh"
	xberg_stub poly 'printf "%s\n" "poly 0.28.2"'
}

stub_alef_verify() {
	local verify_output="$1" verify_status="${2:-0}"
	export VERIFY_OUTPUT="$verify_output" VERIFY_STATUS="$verify_status"
	xberg_stub alef \
		'if [ "$1" = "--version" ]; then' \
		'  printf "%s\n" "alef 0.103.12"' \
		'  exit 0' \
		'fi' \
		'[ "$*" = "verify --exit-code" ] || exit 99' \
		'printf "%s\n" "$VERIFY_OUTPUT"' \
		'exit "$VERIFY_STATUS"'
}

@test "check-alef-generated should_require_a_nonzero_real_poly_comparison" {
	stub_alef_verify 'Formatted-output drift check: 7 file(s) compared via a real `poly fmt` pass, 12 matched the render with no formatter involved.'

	run "$SCRIPT"

	xberg_assert_status 0
	xberg_assert_output_contains "Verified Poly-owned formatted-output comparison covered 7 generated file(s)."
}

@test "check-alef-generated should_fail_when_the_poly_comparison_examines_zero_files" {
	stub_alef_verify 'Formatted-output drift check: 0 file(s) compared via a real `poly fmt` pass, 19 matched the render with no formatter involved.'

	run "$SCRIPT"

	xberg_assert_status 1
	xberg_assert_output_contains "ERROR: alef verify compared zero Poly-owned generated files"
}

@test "check-alef-generated should_fail_when_alef_omits_the_poly_comparison" {
	stub_alef_verify 'All bindings and versions are up to date.'

	run "$SCRIPT"

	xberg_assert_status 1
	xberg_assert_output_contains "ERROR: alef verify did not report Poly-owned formatted-output coverage"
}

@test "check-alef-generated should_fail_when_poly_reports_an_unchecked_owned_file" {
	stub_alef_verify $'Formatted-output drift check: 7 file(s) compared via a real `poly fmt` pass.\n3 generated file(s) could not be checked for formatted-output drift (install poly to close this gap)'

	run "$SCRIPT"

	xberg_assert_status 1
	xberg_assert_output_contains "ERROR: alef verify skipped Poly-owned generated files because its Poly comparison could not run"
}

@test "check-alef-generated should_reject_a_nonfatal_poly_temp_copy_failure" {
	stub_alef_verify $'poly fmt over the drift-check temp copies failed (non-fatal): exit status 1\nFormatted-output drift check: 7 file(s) compared via a real `poly fmt` pass.'

	run "$SCRIPT"

	xberg_assert_status 1
	xberg_assert_output_contains "ERROR: alef verify could not complete its Poly-owned formatted-output comparison"
}

@test "check-alef-generated should_preserve_an_early_alef_failure_without_a_coverage_summary" {
	stub_alef_verify 'ERROR failed to load alef.toml' 42

	run "$SCRIPT"

	xberg_assert_status 42
	xberg_assert_output_contains "ERROR failed to load alef.toml"
}

@test "check-alef-generated should_preserve_a_real_alef_drift_failure" {
	stub_alef_verify $'Formatted-output drift check: 7 file(s) compared via a real `poly fmt` pass.\nERROR generated bindings are out of date' 23

	run "$SCRIPT"

	xberg_assert_status 23
	xberg_assert_output_contains "ERROR generated bindings are out of date"
}
