#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	local repo_root
	repo_root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	mkdir -p "$XBERG_WORK/scripts/ci/docker" "$XBERG_WORK/scripts/test" "$XBERG_WORK/docker"
	cp "$repo_root/scripts/ci/docker/"*.sh "$XBERG_WORK/scripts/ci/docker/"
	cp "$repo_root/scripts/test/test-docker-config-local.sh" "$XBERG_WORK/scripts/test/"
	touch "$XBERG_WORK/docker/Dockerfile.alpine"
	xberg_stub docker 'printf "docker %s\n" "$*" >>"$XBERG_TRACE"' 'exit "${DOCKER_STATUS:-0}"'
	xberg_stub python3 'printf "python3 %s\n" "$*" >>"$XBERG_TRACE"' 'exit "${PYTHON_STATUS:-0}"'
	xberg_stub_trace sudo
	xberg_stub_trace df
	cd "$XBERG_WORK" || return 1
}

run_docker_script() {
	run bash "$XBERG_WORK/scripts/ci/docker/$1" "${@:2}"
}

@test "docker build should_build_verify_and_forward_verbose_suite_arguments" {
	run_docker_script build_and_test.sh --verbose
	xberg_assert_status 0
	xberg_assert_trace \
		"docker build -f $XBERG_WORK/docker/Dockerfile.alpine -t crawlberg-test:latest $XBERG_WORK" \
		'docker inspect crawlberg-test:latest' \
		"python3 $XBERG_WORK/scripts/ci/docker/test_docker.py --image crawlberg-test:latest --variant cli --verbose"
	xberg_assert_output_contains 'All tests passed'
}

@test "docker build should_reject_unknown_options_before_external_commands" {
	run_docker_script build_and_test.sh --typo
	xberg_assert_status 2
	xberg_assert_output_contains 'unknown option: --typo'
	xberg_assert_trace_empty
}

@test "docker build should_fail_when_selected_image_is_missing" {
	export DOCKER_STATUS=17
	run_docker_script build_and_test.sh --image custom:tag
	xberg_assert_status 1
	xberg_assert_output_contains 'Image not found: custom:tag'
	xberg_assert_trace 'docker inspect custom:tag'
}

@test "docker build should_report_suite_failure_without_building_explicit_image" {
	export PYTHON_STATUS=9
	run_docker_script build_and_test.sh --image custom:tag
	xberg_assert_status 1
	xberg_assert_output_contains 'Tests failed'
	xberg_assert_trace 'docker inspect custom:tag' \
		"python3 $XBERG_WORK/scripts/ci/docker/test_docker.py --image custom:tag --variant cli"
}

@test "docker size should_warn_only_when_core_threshold_is_exceeded" {
	xberg_stub docker 'case "$1" in images) echo 2GB ;; inspect) echo 1611661312 ;; esac'
	run_docker_script check-image-size.sh core
	xberg_assert_status 0
	xberg_assert_output_contains 'Image size in MB: 1537'
	xberg_assert_output_contains '::warning::Core image is larger than 1.5GB'
}

@test "docker size should_accept_exact_full_threshold_without_warning" {
	xberg_stub docker 'case "$1" in images) echo 2560MB ;; inspect) echo 2684354560 ;; esac'
	run_docker_script check-image-size.sh full
	xberg_assert_status 0
	xberg_assert_output_contains 'Image size in MB: 2560'
	[[ "$output" != *'::warning::'* ]]
}

@test "docker size should_reject_missing_variant" {
	run_docker_script check-image-size.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'Usage: check-image-size.sh'
	xberg_assert_trace_empty
}

@test "docker cleanup should_continue_after_docker_failures_and_show_disk_space" {
	export DOCKER_STATUS=8
	run_docker_script cleanup.sh core
	xberg_assert_status 0
	xberg_assert_trace 'docker ps -aq --filter name=crawlberg-test' \
		'docker rmi crawlberg:core' 'docker system prune -af --volumes' 'df -h /'
	xberg_assert_output_contains 'Final disk space'
}

@test "docker cleanup should_reject_missing_variant_before_cleanup" {
	run_docker_script cleanup.sh
	xberg_assert_status 1
	xberg_assert_trace_empty
}

@test "docker collect should_capture_each_container_and_diagnostic_artifact" {
	xberg_stub docker 'case "$1" in ps) printf "crawlberg-test-a\ncrawlberg-test-b\n" ;; logs) echo "log:$2" ;; info) echo info ;; version) echo version ;; esac'
	run_docker_script collect-logs.sh "$XBERG_WORK/logs"
	xberg_assert_status 0
	xberg_assert_file "$XBERG_WORK/logs/crawlberg-test-a.log" 'log:crawlberg-test-a'
	xberg_assert_file "$XBERG_WORK/logs/crawlberg-test-b.log" 'log:crawlberg-test-b'
	xberg_assert_file "$XBERG_WORK/logs/docker-info.txt" info
	xberg_assert_file "$XBERG_WORK/logs/docker-version.txt" version
}

@test "docker collect should_preserve_failure_output_and_continue_diagnostics" {
	xberg_stub docker 'case "$1" in ps) echo crawlberg-test-failed ;; *) echo unavailable; exit 3 ;; esac'
	run_docker_script collect-logs.sh "$XBERG_WORK/logs"
	xberg_assert_status 0
	xberg_assert_file "$XBERG_WORK/logs/crawlberg-test-failed.log" unavailable
	xberg_assert_file "$XBERG_WORK/logs/docker-info.txt" unavailable
	xberg_assert_file "$XBERG_WORK/logs/docker-version.txt" unavailable
}

@test "docker free disk should_request_only_documented_cleanup_and_survive_prune_failure" {
	export DOCKER_STATUS=8
	local apt_cleanup
	apt_cleanup="sudo rm -rf $(printf '%s ' /var/lib/apt/lists/*)"
	apt_cleanup="${apt_cleanup% }"
	run_docker_script free-disk-space.sh
	xberg_assert_status 0
	xberg_assert_trace 'df -h /' 'sudo rm -rf /usr/share/dotnet' \
		'sudo rm -rf /usr/local/lib/android' 'sudo rm -rf /opt/ghc' \
		'sudo rm -rf /opt/hostedtoolcache/CodeQL' 'sudo rm -rf /usr/local/share/boost' \
		'sudo rm -rf /usr/local/lib/node_modules' 'sudo rm -rf /opt/microsoft' \
		'sudo apt-get clean' "$apt_cleanup" \
		'docker system prune -af --volumes' 'df -h /'
}

@test "docker free disk should_stop_when_package_cleanup_fails" {
	xberg_stub sudo 'exit 19'
	run_docker_script free-disk-space.sh
	xberg_assert_status 19
	xberg_assert_trace 'df -h /'
	[[ "$output" != *'Cleaning Docker'* ]]
}

@test "docker cli runner should_forward_cli_image_and_propagate_suite_failure" {
	run_docker_script run-cli-tests.sh
	xberg_assert_status 0
	xberg_assert_trace 'python3 scripts/ci/docker/test_docker.py --image crawlberg:cli --variant cli --verbose'
	export PYTHON_STATUS=14
	run_docker_script run-cli-tests.sh
	xberg_assert_status 14
}

@test "docker feature runner should_forward_variant_and_propagate_suite_failure" {
	run_docker_script run-feature-tests.sh full
	xberg_assert_status 0
	xberg_assert_trace 'python3 scripts/ci/docker/test_docker.py --image crawlberg:full --variant full --verbose'
	export PYTHON_STATUS=14
	run_docker_script run-feature-tests.sh core
	xberg_assert_status 14
}

@test "docker feature runner should_reject_missing_variant" {
	run_docker_script run-feature-tests.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'missing variant'
	xberg_assert_trace_empty
}

@test "docker config runner should_execute_selected_configuration_suite" {
	run_docker_script run-config-tests.sh full
	xberg_assert_status 0
	xberg_assert_trace "python3 $XBERG_WORK/scripts/test/../ci/docker/test_config.py --image crawlberg:full"
	export PYTHON_STATUS=16
	run_docker_script run-config-tests.sh full
	xberg_assert_status 16
}

@test "docker config runner should_reject_missing_variant" {
	run_docker_script run-config-tests.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'missing variant'
	xberg_assert_trace_empty
}

@test "docker save should_write_actual_compressed_image_bytes" {
	xberg_stub docker 'printf "image:%s" "$2"'
	run_docker_script save-image.sh core "$XBERG_WORK/export"
	xberg_assert_status 0
	run gzip -dc "$XBERG_WORK/export/crawlberg-core.tar.gz"
	xberg_assert_status 0
	xberg_assert_output 'image:crawlberg:core'
}

@test "docker save should_propagate_pipeline_failure" {
	export DOCKER_STATUS=23
	run_docker_script save-image.sh full "$XBERG_WORK/export"
	xberg_assert_status 23
}

@test "docker save should_reject_missing_variant" {
	run_docker_script save-image.sh
	xberg_assert_status 1
	xberg_assert_trace_empty
}

@test "docker summary should_render_valid_json_and_fallback_to_invalid_json_text" {
	printf '{"passed":6}\n' >"$XBERG_WORK/results.json"
	run_docker_script summary.sh core "$XBERG_WORK/results.json"
	xberg_assert_status 0
	xberg_assert_output_contains 'Variant: core'
	xberg_assert_output_contains '"passed": 6'
	printf 'invalid-json\n' >"$XBERG_WORK/results.json"
	run_docker_script summary.sh full "$XBERG_WORK/results.json"
	xberg_assert_status 0
	xberg_assert_output_contains 'invalid-json'
}

@test "docker summary should_reject_missing_variant" {
	run_docker_script summary.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'Usage: summary.sh'
}

@test "docker local config should_default_to_both_variants_and_honor_image_override" {
	run bash scripts/test/test-docker-config-local.sh
	xberg_assert_status 0
	xberg_assert_trace "python3 $XBERG_WORK/scripts/test/../ci/docker/test_config.py --image crawlberg:core" \
		"python3 $XBERG_WORK/scripts/test/../ci/docker/test_config.py --image crawlberg:full"
	: >"$XBERG_TRACE"
	run bash scripts/test/test-docker-config-local.sh --variant core --image custom:tag --verbose
	xberg_assert_status 0
	xberg_assert_trace "python3 $XBERG_WORK/scripts/test/../ci/docker/test_config.py --image custom:tag"
}

@test "docker local config should_reject_unknown_variant_and_option_before_testing" {
	run bash scripts/test/test-docker-config-local.sh --variant typo
	xberg_assert_status 1
	xberg_assert_output_contains 'Invalid variant: typo'
	xberg_assert_trace_empty
	run bash scripts/test/test-docker-config-local.sh --typo
	xberg_assert_status 1
	xberg_assert_output_contains 'Unknown option: --typo'
	xberg_assert_trace_empty
}

@test "docker local config should_stop_before_second_variant_on_suite_failure" {
	export PYTHON_STATUS=31
	run bash scripts/test/test-docker-config-local.sh
	xberg_assert_status 31
	xberg_assert_trace "python3 $XBERG_WORK/scripts/test/../ci/docker/test_config.py --image crawlberg:core"
}
