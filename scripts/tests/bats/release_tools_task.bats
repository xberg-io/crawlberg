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
	for script in brew-e2e-test csharp-lint dart-e2e-test e2e-generate elixir-lint frb-codegen-ensure \
		go-lint java-lint ruby-lint shell-lint sync-bats-lib; do
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

@test "e2e generator should_forward_selected_language_without_obsolete_format_flag" {
	xberg_stub_trace alef
	run bash scripts/task/e2e-generate.sh swift
	xberg_assert_status 0
	xberg_assert_trace 'alef e2e generate --lang swift'
}

@test "e2e generator should_reject_missing_language_without_invoking_generator" {
	xberg_stub_trace alef
	run bash scripts/task/e2e-generate.sh
	xberg_assert_status 1
	xberg_assert_output_contains '<language>'
	xberg_assert_trace_empty
}

prepare_csharp() {
	mkdir -p packages/csharp e2e/csharp
	: >packages/csharp/Crawlberg.slnx
	: >e2e/csharp/Crawlberg.E2E.csproj
	xberg_stub dotnet 'printf "%s\n" "$*" >> "$XBERG_TRACE"' 'if [ "$1" = format ]; then exit "${LINT_EXIT:-0}"; fi'
}

@test "csharp lint should_check_both_real_projects_without_modifying_them" {
	prepare_csharp
	run bash scripts/task/csharp-lint.sh check
	xberg_assert_status 0
	xberg_assert_trace 'restore Crawlberg.slnx --verbosity quiet' 'format Crawlberg.slnx --verify-no-changes' \
		'restore Crawlberg.E2E.csproj --verbosity quiet' 'format Crawlberg.E2E.csproj --verify-no-changes'
}

@test "csharp lint should_aggregate_formatter_failures_and_still_visit_both_projects" {
	prepare_csharp
	export LINT_EXIT=42
	run bash scripts/task/csharp-lint.sh fix
	xberg_assert_status 1
	xberg_assert_trace 'restore Crawlberg.slnx --verbosity quiet' 'format Crawlberg.slnx' \
		'restore Crawlberg.E2E.csproj --verbosity quiet' 'format Crawlberg.E2E.csproj'
}

prepare_elixir() {
	mkdir -p packages/elixir e2e/elixir
	: >packages/elixir/mix.exs
	: >e2e/elixir/mix.exs
	xberg_stub mix 'printf "%s\n" "$*" >> "$XBERG_TRACE"' 'if [ "$1" = credo ]; then exit "${LINT_EXIT:-0}"; fi'
}

@test "elixir lint should_check_format_and_credo_for_both_projects" {
	prepare_elixir
	run bash scripts/task/elixir-lint.sh check
	xberg_assert_status 0
	xberg_assert_trace 'deps.get --quiet' 'format --check-formatted' 'credo --strict' \
		'deps.get --quiet' 'format --check-formatted' 'credo --strict'
}

@test "elixir lint should_preserve_failure_while_completing_fix_passes" {
	prepare_elixir
	export LINT_EXIT=42
	run bash scripts/task/elixir-lint.sh fix
	xberg_assert_status 1
	xberg_assert_trace 'deps.get --quiet' 'format' 'credo suggest --all --strict' \
		'deps.get --quiet' 'format' 'credo suggest --all --strict'
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

prepare_go() {
	mkdir -p packages/go e2e/go tools/benchmark-harness/scripts target/release
	: >target/release/libcrawlberg_ffi.so
	: >packages/go/go.mod
	: >e2e/go/go.mod
	: >tools/benchmark-harness/scripts/go.mod
	xberg_stub gofmt 'if [ "${GOFMT_DIRTY:-0}" = 1 ]; then echo unformatted.go; fi'
	xberg_stub_trace go
	xberg_stub golangci-lint 'printf "%s\n" "$*" >> "$XBERG_TRACE"' 'exit "${LINT_EXIT:-0}"'
	xberg_stub cargo 'echo unexpected-cargo >&2; exit 99'
}

@test "go lint should_check_all_three_modules_with_shared_config_and_existing_ffi" {
	prepare_go
	run bash scripts/task/go-lint.sh check
	xberg_assert_status 0
	local expected="run --config $XBERG_WORK/.golangci.yml ./..."
	xberg_assert_trace "$expected" "$expected" "$expected"
}

@test "go lint should_fail_when_gofmt_reports_real_output" {
	prepare_go
	export GOFMT_DIRTY=1
	run bash scripts/task/go-lint.sh check
	xberg_assert_status 1
	xberg_assert_output_contains 'unformatted.go'
	[ "$(wc -l <"$XBERG_TRACE" | tr -d ' ')" -eq 3 ]
}

prepare_java() {
	mkdir -p packages/java e2e/java
	printf '%s\n' spotless-maven-plugin maven-checkstyle-plugin maven-pmd-plugin >packages/java/pom.xml
	printf '%s\n' spotless-maven-plugin >e2e/java/pom.xml
	xberg_stub mvn 'printf "%s\n" "$*" >> "$XBERG_TRACE"' 'exit "${LINT_EXIT:-0}"'
}

@test "java lint should_run_only_plugins_declared_by_each_manifest" {
	prepare_java
	run bash scripts/task/java-lint.sh check
	xberg_assert_status 0
	xberg_assert_trace '-q spotless:check' '-q checkstyle:check' '-q pmd:check pmd:cpd-check' '-q spotless:check'
}

@test "java lint should_aggregate_plugin_failures_in_both_modules" {
	prepare_java
	export LINT_EXIT=42
	run bash scripts/task/java-lint.sh fix
	xberg_assert_status 1
	xberg_assert_trace '-q spotless:apply' '-q spotless:apply'
}

prepare_ruby() {
	mkdir -p packages/ruby e2e/ruby
	: >packages/ruby/library.rb
	: >packages/ruby/Steepfile
	: >e2e/ruby/test.rb
	xberg_stub bundle 'printf "%s\n" "$*" >> "$XBERG_TRACE"' 'exit "${LINT_EXIT:-0}"'
}

@test "ruby lint should_check_package_types_and_e2e_with_distinct_configs" {
	prepare_ruby
	run bash scripts/task/ruby-lint.sh check
	xberg_assert_status 0
	xberg_assert_output_contains 'Ruby lint: OK'
	xberg_assert_trace 'exec rubocop --config .rubocop.yml .' 'exec steep check' \
		"exec rubocop --config $XBERG_WORK/e2e/ruby/.rubocop.yaml ."
}

@test "ruby lint should_report_aggregated_failure_without_skipping_e2e" {
	prepare_ruby
	export LINT_EXIT=42
	run bash scripts/task/ruby-lint.sh fix
	xberg_assert_status 1
	xberg_assert_output_contains 'Ruby lint: FAILED'
	xberg_assert_trace 'exec rubocop --config .rubocop.yml --autocorrect-all .' 'exec steep check' \
		"exec rubocop --config $XBERG_WORK/e2e/ruby/.rubocop.yaml --autocorrect-all ."
}

@test "shell lint should_check_each_tracked_script_before_shellcheck" {
	xberg_stub git 'printf "%s\n" first.sh second.sh'
	xberg_stub_trace shfmt
	xberg_stub_trace shellcheck
	run bash scripts/task/shell-lint.sh check
	xberg_assert_status 0
	xberg_assert_trace 'shfmt -d first.sh second.sh' 'shellcheck first.sh second.sh'
}

@test "shell lint should_stop_after_formatter_failure" {
	xberg_stub git 'echo first.sh'
	xberg_stub_exit shfmt 42
	xberg_stub_trace shellcheck
	run bash scripts/task/shell-lint.sh check
	xberg_assert_status 42
	xberg_assert_trace_empty
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
