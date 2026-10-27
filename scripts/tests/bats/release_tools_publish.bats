#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	local root
	root="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	mkdir -p "$XBERG_WORK/scripts"
	cp -R "$root/scripts/publish" "$XBERG_WORK/scripts/"
	export HOME="$XBERG_WORK/home" TMPDIR="$XBERG_WORK/tmp"
	mkdir -p "$HOME" "$TMPDIR"
	cd "$XBERG_WORK"
}

@test "docker tag check should_publish_exists_and_skip_summary_for_existing_manifest" {
	xberg_stub_trace docker
	export DOCKER_TAG=example/image:1.12.3 SUMMARY_LABEL=runtime
	run bash scripts/publish/check-docker-tag.sh
	xberg_assert_status 0
	xberg_assert_github_output exists=true
	xberg_assert_github_summary 'Docker tag example/image:1.12.3 already exists; runtime publish will be skipped.'
	xberg_assert_trace 'docker manifest inspect example/image:1.12.3'
}

@test "docker tag check should_report_absent_manifest_without_skip_summary" {
	xberg_stub_exit docker 1
	export DOCKER_TAG=example/image:missing
	run bash scripts/publish/check-docker-tag.sh
	xberg_assert_status 0
	xberg_assert_github_output exists=false
	xberg_assert_github_summary ''
}

@test "node platform packer should_pack_only_platforms_with_manifest_and_binary" {
	mkdir -p npm/linux npm/unbuilt npm/no-manifest
	printf '{}' >npm/linux/package.json
	printf '{}' >npm/unbuilt/package.json
	printf binary >npm/linux/addon.node
	xberg_stub pnpm 'printf "%s\n" "$PWD $*" >> "$XBERG_TRACE"' 'printf tarball > fixture.tgz'
	run bash scripts/publish/pack-node-platform-packages.sh "$XBERG_WORK/npm"
	xberg_assert_status 0
	xberg_assert_output_contains 'Successfully packed 1 platform package(s)'
	xberg_assert_output_contains 'File size: 6'
	[[ "$output" != *'stat:'* ]]
	xberg_assert_file npm/fixture.tgz tarball
	xberg_assert_trace "$XBERG_WORK/npm/linux pack"
}

@test "node platform packer should_fail_on_package_manager_error" {
	mkdir -p npm/linux
	printf '{}' >npm/linux/package.json
	printf binary >npm/linux/addon.node
	xberg_stub_exit pnpm 43
	run bash scripts/publish/pack-node-platform-packages.sh "$XBERG_WORK/npm"
	xberg_assert_status 1
	xberg_assert_output_contains 'Failed to pack linux/'
	xberg_assert_file_absent npm/fixture.tgz
}

@test "node artifact preparation should_extract_platforms_and_merge_typescript_definitions" {
	mkdir -p fixture/linux artifacts defs dest/npm/stale
	printf binary >fixture/linux/addon.node
	printf '{}' >fixture/linux/package.json
	printf javascript >defs/index.js
	printf declarations >defs/index.d.ts
	tar -czf artifacts/linux.tar.gz -C fixture linux
	run bash scripts/publish/prepare-node-artifacts.sh artifacts defs dest
	xberg_assert_status 0
	xberg_assert_file dest/npm/linux/addon.node binary
	xberg_assert_file dest/index.d.ts declarations
	[ ! -e dest/npm/stale ]
}

@test "node artifact preparation should_reject_missing_input_without_deleting_destination" {
	mkdir -p dest/npm
	printf preserve >dest/npm/marker
	run bash scripts/publish/prepare-node-artifacts.sh missing defs dest
	xberg_assert_status 1
	xberg_assert_output_contains 'Artifacts directory not found: missing'
	xberg_assert_file dest/npm/marker preserve
}

@test "homebrew updater should_replace_source_checksum_and_remove_stale_bottles" {
	export TAP_DIR="$XBERG_WORK/tap" TAG=v1.12.3 VERSION=1.12.3
	mkdir -p "$TAP_DIR/Formula"
	printf '%s\n' 'class Crawlberg < Formula' '  url "old"' '  sha256 "abc"' \
		'  bottle do' '    root_url "old"' '  end' end >"$TAP_DIR/Formula/crawlberg.rb"
	xberg_stub_curl_body source
	run bash scripts/publish/update-homebrew-formula.sh
	xberg_assert_status 0
	local sha
	sha="$(printf source | shasum -a 256 | awk '{print $1}')"
	local expected
	expected="$(printf '%s\n' 'class Crawlberg < Formula' \
		"  url 'https://github.com/xberg-io/crawlberg/archive/v1.12.3.tar.gz'" "  sha256 '$sha'" end)"
	xberg_assert_file "$TAP_DIR/Formula/crawlberg.rb" "$expected"
}

@test "homebrew updater should_reject_formula_without_source_checksum_without_writing" {
	export TAP_DIR="$XBERG_WORK/tap" TAG=v1.12.3 VERSION=1.12.3
	mkdir -p "$TAP_DIR/Formula"
	printf 'url "old"\n' >"$TAP_DIR/Formula/crawlberg.rb"
	xberg_stub_curl_body source
	run bash scripts/publish/update-homebrew-formula.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'no source sha256 line matched'
	xberg_assert_file "$TAP_DIR/Formula/crawlberg.rb" 'url "old"'
}

@test "docker dry run summary should_render_image_version_and_suffix" {
	export IMAGE=example/image VERSION=1.12.3 TAG_SUFFIX=-minimal
	run bash scripts/publish/docker/dry-run-summary.sh
	xberg_assert_status 0
	xberg_assert_github_summary 'Dry run requested; Docker image example/image:1.12.3-minimal tested but not pushed.'
}

@test "docker dry run summary should_require_image_and_version" {
	unset IMAGE VERSION
	run bash scripts/publish/docker/dry-run-summary.sh
	xberg_assert_status 2
	xberg_assert_output_contains 'Usage: set IMAGE and VERSION'
	xberg_assert_github_summary ''
}

@test "disk cleanup should_invoke_only_stubbed_privileged_and_docker_boundaries" {
	xberg_stub_trace sudo
	xberg_stub_trace docker
	xberg_stub df 'echo sandbox-disk'
	run bash scripts/publish/ubuntu/free-disk-space.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'Disk usage after cleanup'
	local trace
	trace="$(cat "$XBERG_TRACE")"
	[[ "$trace" == *'docker system prune -af --volumes'* ]]
	[[ "$trace" == *'sudo apt-get autoremove --yes'* ]]
	[ "$(wc -l <"$XBERG_TRACE" | tr -d ' ')" -eq 12 ]
}

@test "disk cleanup should_continue_after_external_cleanup_failures" {
	xberg_stub sudo 'printf "sudo %s\n" "$*" >> "$XBERG_TRACE"' 'exit 35'
	xberg_stub docker 'printf "docker %s\n" "$*" >> "$XBERG_TRACE"' 'exit 36'
	xberg_stub df 'echo sandbox-disk'
	run bash scripts/publish/ubuntu/free-disk-space.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'Disk usage after cleanup'
	[ "$(wc -l <"$XBERG_TRACE" | tr -d ' ')" -eq 12 ]
}
