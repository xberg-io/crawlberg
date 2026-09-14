#!/usr/bin/env bats
#
# Contract tests for scripts/lib/tessdata.sh.
#
# FAN-OUT SHARED: byte-identical in xberg, kreuzberg-lts and crawlberg. The three copies of the
# script differ on exactly two lines, both of them the product token, so the suite derives that
# token in setup_file() and never types one. Copy it with `cp`, never by hand-editing one copy.
#
# setup_tessdata communicates by `export`, so it is called DIRECTLY -- `run` forks, and the
# export would be discarded before the assertion saw it. The download helpers return a status and
# write files, so those use `run`.
#
# LIMITATION, stated rather than faked: ensure_tessdata scans a hardcoded list of absolute
# candidate directories (/opt/homebrew/share/tessdata and friends) and `[ -f ... ]` is a builtin,
# so which branch it takes is a property of the machine, not of the test. The copy loop is
# therefore covered only to the extent that neutering `cp` makes the outcome deterministic; the
# "copied from a candidate" branch has no hermetic test and is not pretended to have one. The
# macOS case in setup_tessdata has the same shape and says so where it sits. ~keep

setup_file() {
	REPO_ROOT="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"

	local ffi_dir
	ffi_dir="$(find "$REPO_ROOT/crates" -maxdepth 1 -type d -name '*-ffi' | head -n 1)"
	[ -n "$ffi_dir" ] || {
		echo "no crates/*-ffi crate under $REPO_ROOT" >&2
		return 1
	}
	PRODUCT="$(basename "$ffi_dir")"
	PRODUCT="${PRODUCT%-ffi}"

	# The derivation is the only thing standing between this suite and a vacuous pass: if it ever
	# stops producing the token the script actually uses, every product-scoped assertion below
	# would compare two strings that are both wrong in the same way. Check it once, loudly. ~keep
	grep -q "${PRODUCT}-tesseract" "$REPO_ROOT/scripts/lib/tessdata.sh" || {
		echo "derived product token '${PRODUCT}' does not appear in scripts/lib/tessdata.sh" >&2
		return 1
	}
	export REPO_ROOT PRODUCT
}

setup() {
	bats_load_library xberg-bats
	xberg_setup

	LIB="$REPO_ROOT/scripts/lib/tessdata.sh"
	DEST="$XBERG_WORK/tessdata"
	ENG_URL="https://github.com/tesseract-ocr/tessdata_fast/raw/main/eng.traineddata"

	unset RUNNER_OS TESSDATA_PREFIX
}

# A curl that writes exactly N zero bytes to its -o/--output target.
#
# The size IS the contract here -- download_traineddata accepts or rejects a download purely on
# its byte count -- so the fixture has to be a real file of a chosen length rather than a string.
# shellcheck disable=SC2016  # the body must expand when the stub runs, not while it is written
stub_curl_sized() {
	export XBERG_CURL_BYTES="$1"
	xberg_stub curl \
		'for ((i = 1; i <= $#; i++)); do' \
		'  case "${!i}" in' \
		'    -o | --output)' \
		'      next=$((i + 1))' \
		'      head -c "$XBERG_CURL_BYTES" /dev/zero >"${!next}"' \
		'      exit 0' \
		'      ;;' \
		'  esac' \
		'done' \
		'exit 1'
}

# --- file_size_bytes ----------------------------------------------------------------------------

@test "file_size_bytes should_print_zero_when_the_path_does_not_exist" {
	source "$LIB"
	run file_size_bytes "$XBERG_WORK/absent"
	xberg_assert_status 0
	xberg_assert_output "0"
}

@test "file_size_bytes should_print_the_byte_count_of_an_existing_file" {
	source "$LIB"
	head -c 4096 /dev/zero >"$XBERG_WORK/blob"
	run file_size_bytes "$XBERG_WORK/blob"
	xberg_assert_output "4096"
}

@test "file_size_bytes should_print_zero_when_the_path_is_a_directory" {
	source "$LIB"
	run file_size_bytes "$XBERG_WORK"
	xberg_assert_output "0"
}

@test "file_size_bytes should_fall_back_to_the_bsd_stat_flag_when_the_gnu_flag_is_unsupported" {
	source "$LIB"
	: >"$XBERG_WORK/blob"
	# The probe is `stat -c%s path >/dev/null 2>&1`, so a stat that rejects -c must send the
	# script down the -f%z path. Without this test the fallback only ever runs on macOS and a
	# change that broke it would go green on every Linux runner. ~keep
	xberg_stub stat \
		'[ "$1" = "-f%z" ] || exit 1' \
		'printf "%s\n" 1234'

	run file_size_bytes "$XBERG_WORK/blob"
	xberg_assert_status 0
	xberg_assert_output "1234"
}

# --- min_traineddata_size_bytes -----------------------------------------------------------------

@test "min_traineddata_size_bytes should_require_a_megabyte_for_eng" {
	source "$LIB"
	run min_traineddata_size_bytes eng
	xberg_assert_output "1000000"
}

@test "min_traineddata_size_bytes should_require_a_megabyte_for_deu" {
	source "$LIB"
	run min_traineddata_size_bytes deu
	xberg_assert_output "1000000"
}

@test "min_traineddata_size_bytes should_require_a_hundred_kilobytes_for_osd" {
	source "$LIB"
	run min_traineddata_size_bytes osd
	xberg_assert_output "100000"
}

@test "min_traineddata_size_bytes should_require_a_hundred_kilobytes_for_an_unlisted_language" {
	source "$LIB"
	run min_traineddata_size_bytes fra
	xberg_assert_output "100000"
}

# --- download_traineddata -----------------------------------------------------------------------

@test "download_traineddata should_move_the_temp_file_into_place_when_it_meets_the_minimum_size" {
	source "$LIB"
	# Comfortably over the threshold, so that the boundary case below is the only test standing
	# on the exact minimum and is not a silent duplicate of this one. ~keep
	stub_curl_sized 200000
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"

	xberg_assert_status 0
	[ "$(wc -c <"$XBERG_WORK/osd.traineddata" | tr -d ' ')" -eq 200000 ]
	xberg_assert_trace_empty
}

@test "download_traineddata should_accept_a_file_that_is_exactly_the_minimum_size" {
	source "$LIB"
	# The comparison is `-ge`, so the boundary is an accept. A test one byte either side of it is
	# the only thing that tells `-ge` from `-gt`. ~keep
	stub_curl_sized 100000
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"
	xberg_assert_status 0
}

@test "download_traineddata should_reject_a_file_one_byte_under_the_minimum_size" {
	source "$LIB"
	stub_curl_sized 99999
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"
	xberg_assert_status 1
}

@test "download_traineddata should_leave_no_temp_file_behind_when_it_succeeds" {
	source "$LIB"
	stub_curl_sized 100000
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"

	xberg_assert_status 0
	xberg_assert_file_absent "$XBERG_WORK/osd.traineddata.tmp"
}

@test "download_traineddata should_leave_no_temp_file_behind_when_it_gives_up" {
	source "$LIB"
	stub_curl_sized 10
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"

	xberg_assert_status 1
	xberg_assert_file_absent "$XBERG_WORK/osd.traineddata.tmp"
}

@test "download_traineddata should_not_create_the_destination_when_every_attempt_is_too_small" {
	source "$LIB"
	stub_curl_sized 10
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"

	xberg_assert_status 1
	xberg_assert_file_absent "$XBERG_WORK/osd.traineddata"
}

@test "download_traineddata should_retry_when_curl_itself_fails" {
	source "$LIB"
	xberg_stub_exit curl 1
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"

	xberg_assert_status 1
	xberg_assert_output_contains "Failed to download osd.traineddata (attempt 1), retrying..."
	xberg_assert_output_contains "Failed to download osd.traineddata (attempt 5), retrying..."
}

@test "download_traineddata should_report_the_measured_size_when_the_download_is_too_small" {
	source "$LIB"
	stub_curl_sized 10
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"

	xberg_assert_output_contains "Downloaded osd.traineddata too small (10 bytes < 100000), retrying..."
	xberg_assert_output_contains "ERROR: Failed to download valid osd.traineddata after retries"
}

@test "download_traineddata should_sleep_for_an_increasing_number_of_seconds_between_attempts" {
	source "$LIB"
	stub_curl_sized 10
	xberg_stub_trace sleep

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"

	# Five sleeps for five attempts: the loop sleeps after the final failure too, so the caller
	# waits 15 seconds it can never benefit from. Asserted rather than tidied away -- the point of
	# the trace is that the schedule is visible instead of merely endured. ~keep
	xberg_assert_trace "sleep 1" "sleep 2" "sleep 3" "sleep 4" "sleep 5"
}

@test "download_traineddata should_succeed_on_a_later_attempt_when_an_early_one_is_truncated" {
	source "$LIB"
	xberg_stub_trace sleep
	# A curl that is truncated once and correct afterwards, which is the failure this retry loop
	# exists for. The counter lives in a file because each invocation is a fresh process.
	export XBERG_ATTEMPTS="$XBERG_WORK/attempts"
	: >"$XBERG_ATTEMPTS"
	# shellcheck disable=SC2016  # the body must expand when the stub runs
	xberg_stub curl \
		'printf x >>"$XBERG_ATTEMPTS"' \
		'count=$(wc -c <"$XBERG_ATTEMPTS" | tr -d " ")' \
		'for ((i = 1; i <= $#; i++)); do' \
		'  case "${!i}" in' \
		'    -o | --output)' \
		'      next=$((i + 1))' \
		'      if [ "$count" -ge 3 ]; then' \
		'        head -c 100000 /dev/zero >"${!next}"' \
		'      else' \
		'        head -c 10 /dev/zero >"${!next}"' \
		'      fi' \
		'      exit 0' \
		'      ;;' \
		'  esac' \
		'done' \
		'exit 1'

	run download_traineddata osd "$XBERG_WORK/osd.traineddata" "$ENG_URL"

	xberg_assert_status 0
	[ "$(wc -c <"$XBERG_ATTEMPTS" | tr -d ' ')" -eq 3 ]
	xberg_assert_trace "sleep 1" "sleep 2"
}

# --- ensure_valid_traineddata -------------------------------------------------------------------

@test "ensure_valid_traineddata should_return_without_downloading_when_the_file_is_large_enough" {
	source "$LIB"
	mkdir -p "$DEST"
	head -c 100000 /dev/zero >"$DEST/osd.traineddata"
	# A curl that cannot run is the only way to tell a genuine early return from a re-download
	# that happens to produce a file of the same size. ~keep
	xberg_stub_curl_offline

	run ensure_valid_traineddata "$DEST" osd "$ENG_URL"
	xberg_assert_status 0
}

@test "ensure_valid_traineddata should_replace_the_file_when_it_is_under_the_minimum_size" {
	source "$LIB"
	mkdir -p "$DEST"
	head -c 10 /dev/zero >"$DEST/osd.traineddata"
	stub_curl_sized 100000
	xberg_stub_trace sleep

	run ensure_valid_traineddata "$DEST" osd "$ENG_URL"

	xberg_assert_status 0
	xberg_assert_output_contains "Invalid osd.traineddata at ${DEST}/osd.traineddata (10 bytes < 100000); re-downloading..."
	[ "$(wc -c <"$DEST/osd.traineddata" | tr -d ' ')" -eq 100000 ]
}

@test "ensure_valid_traineddata should_download_when_the_file_is_absent" {
	source "$LIB"
	mkdir -p "$DEST"
	stub_curl_sized 100000
	xberg_stub_trace sleep

	run ensure_valid_traineddata "$DEST" osd "$ENG_URL"

	xberg_assert_status 0
	# The "invalid, re-downloading" notice is for a file that was there and was wrong. An absent
	# file is the ordinary first run and must not be reported as corruption. ~keep
	[[ "$output" != *"Invalid osd.traineddata"* ]]
}

@test "ensure_valid_traineddata should_fail_when_every_download_attempt_is_too_small" {
	source "$LIB"
	mkdir -p "$DEST"
	stub_curl_sized 10
	xberg_stub_trace sleep

	run ensure_valid_traineddata "$DEST" osd "$ENG_URL"
	xberg_assert_status 1
}

# --- ensure_tessdata ----------------------------------------------------------------------------

@test "ensure_tessdata should_create_the_destination_directory_when_it_does_not_exist" {
	source "$LIB"
	stub_curl_sized 1000000
	xberg_stub_exit cp 0
	xberg_stub_trace sleep

	run ensure_tessdata "$DEST/nested/deeper"

	xberg_assert_status 0
	[ -d "$DEST/nested/deeper" ]
}

@test "ensure_tessdata should_download_both_eng_and_osd_when_the_destination_is_empty" {
	source "$LIB"
	stub_curl_sized 1000000
	# `cp` is neutered so the candidate scan cannot populate the destination from whatever
	# tesseract the host happens to have installed. That is what makes this assertion the same on
	# a developer's Mac and on a bare runner. ~keep
	xberg_stub_exit cp 0
	xberg_stub_trace sleep

	run ensure_tessdata "$DEST"

	xberg_assert_status 0
	[ -f "$DEST/eng.traineddata" ]
	[ -f "$DEST/osd.traineddata" ]
}

@test "ensure_tessdata should_request_both_languages_from_tessdata_fast" {
	source "$LIB"
	xberg_stub_exit cp 0
	xberg_stub_trace sleep
	# shellcheck disable=SC2016  # the body must expand when the stub runs
	xberg_stub curl \
		'printf "%s\n" "$*" >>"$XBERG_TRACE"' \
		'for ((i = 1; i <= $#; i++)); do' \
		'  case "${!i}" in' \
		'    -o | --output)' \
		'      next=$((i + 1))' \
		'      head -c 1000000 /dev/zero >"${!next}"' \
		'      ;;' \
		'  esac' \
		'done' \
		'exit 0'

	run ensure_tessdata "$DEST"

	xberg_assert_status 0
	grep -q "tessdata_fast/raw/main/eng.traineddata" "$XBERG_TRACE"
	grep -q "tessdata_fast/raw/main/osd.traineddata" "$XBERG_TRACE"
}

@test "ensure_tessdata should_fail_and_skip_osd_when_eng_never_reaches_the_minimum_size" {
	source "$LIB"
	stub_curl_sized 10
	xberg_stub_exit cp 0
	xberg_stub_trace sleep

	run ensure_tessdata "$DEST"

	xberg_assert_status 1
	xberg_assert_file_absent "$DEST/osd.traineddata"
}

# --- setup_tessdata -----------------------------------------------------------------------------

# setup_tessdata is called directly, never through `run`: TESSDATA_PREFIX is an export and a
# forked subshell would discard it. ensure_tessdata is redefined after the source so the platform
# mapping can be asserted without reaching the network; the redefinition wins because bash
# resolves the call at run time.
run_setup_tessdata() {
	source "$LIB"
	ensure_tessdata() { :; }
	# The function's own last statement is `[ -f "$prefix/osd.traineddata" ] && echo ...`, so it
	# returns 1 whenever that file is absent even though the prefix was set correctly. Latent in
	# production -- ensure_tessdata either provides both files or fails first -- but it is why the
	# status is captured here instead of letting errexit end the test. Pinned by its own case
	# below rather than papered over with `|| true`. ~keep
	SETUP_STATUS=0
	setup_tessdata >/dev/null || SETUP_STATUS=$?
}

@test "setup_tessdata should_use_the_distribution_tessdata_directory_on_linux" {
	export RUNNER_OS=Linux
	run_setup_tessdata
	[ "$TESSDATA_PREFIX" = "/usr/share/tesseract-ocr/5/tessdata" ]
}

@test "setup_tessdata should_resolve_the_macos_prefix_in_declared_precedence_order" {
	# Same limitation as ensure_tessdata: the two homebrew candidates are absolute paths tested
	# with `-d`, so which one wins is a fact about the machine. The expectation is therefore
	# computed from the host, which still catches a changed literal on whichever branch this
	# machine takes. The product-scoped last resort is covered hermetically by the Windows case
	# below, which reaches the same token without probing the filesystem. ~keep
	local expected
	if [ -d /opt/homebrew/opt/tesseract/share/tessdata ]; then
		expected="/opt/homebrew/opt/tesseract/share/tessdata"
	elif [ -d /usr/local/opt/tesseract/share/tessdata ]; then
		expected="/usr/local/opt/tesseract/share/tessdata"
	else
		expected="$HOME/Library/Application Support/${PRODUCT}-tesseract/tessdata"
	fi

	export RUNNER_OS=macOS
	run_setup_tessdata
	[ "$TESSDATA_PREFIX" = "$expected" ]
}

@test "setup_tessdata should_treat_darwin_from_uname_the_same_as_macos" {
	export RUNNER_OS=Darwin
	run_setup_tessdata
	local darwin_prefix="$TESSDATA_PREFIX"

	export RUNNER_OS=macOS
	run_setup_tessdata
	[ "$TESSDATA_PREFIX" = "$darwin_prefix" ]
}

@test "setup_tessdata should_scope_the_windows_prefix_to_appdata_and_the_product" {
	export RUNNER_OS=Windows APPDATA="$XBERG_WORK/AppData"
	run_setup_tessdata
	[ "$TESSDATA_PREFIX" = "$XBERG_WORK/AppData/${PRODUCT}-tesseract/tessdata" ]
}

@test "setup_tessdata should_fall_back_to_userprofile_when_appdata_is_empty_on_windows" {
	export RUNNER_OS=MINGW64_NT-10.0 APPDATA="" USERPROFILE="$XBERG_WORK/Users/runner"
	run_setup_tessdata
	[ "$TESSDATA_PREFIX" = "$XBERG_WORK/Users/runner/${PRODUCT}-tesseract/tessdata" ]
}

@test "setup_tessdata should_use_a_target_directory_under_the_repo_root_on_an_unknown_platform" {
	export RUNNER_OS=Haiku REPO_ROOT="$XBERG_WORK/repo"
	run_setup_tessdata
	[ "$TESSDATA_PREFIX" = "$XBERG_WORK/repo/target/tessdata" ]
}

@test "setup_tessdata should_fall_back_to_uname_when_runner_os_is_unset" {
	xberg_stub uname 'printf "%s\n" Linux'
	unset RUNNER_OS
	run_setup_tessdata
	[ "$TESSDATA_PREFIX" = "/usr/share/tesseract-ocr/5/tessdata" ]
}

@test "setup_tessdata should_report_failure_when_the_osd_file_is_absent_after_the_prefix_is_set" {
	# Pins the trailing-`&&` wart described in run_setup_tessdata: the prefix is correct and
	# nothing went wrong, yet the function hands its caller a non-zero status. Any caller running
	# under `set -e` would abort here. Documented as behaviour so a fix is a deliberate change
	# rather than an accident. ~keep
	export RUNNER_OS=Haiku REPO_ROOT="$XBERG_WORK/repo"
	run_setup_tessdata
	[ "$TESSDATA_PREFIX" = "$XBERG_WORK/repo/target/tessdata" ]
	[ "$SETUP_STATUS" -ne 0 ]
}

@test "setup_tessdata should_succeed_when_both_traineddata_files_are_present" {
	export RUNNER_OS=Haiku REPO_ROOT="$XBERG_WORK/repo"
	mkdir -p "$XBERG_WORK/repo/target/tessdata"
	: >"$XBERG_WORK/repo/target/tessdata/eng.traineddata"
	: >"$XBERG_WORK/repo/target/tessdata/osd.traineddata"

	run_setup_tessdata
	[ "$SETUP_STATUS" -eq 0 ]
}
