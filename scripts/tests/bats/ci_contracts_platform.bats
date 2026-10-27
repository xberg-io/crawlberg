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

@test "changelog mirror accepts matching nonempty unreleased blocks" {
	mkdir -p docs-site/src/content/docs
	printf '## [Unreleased]\n\n- fixed\n## [1.0.0]\nold\n' >CHANGELOG.md
	cp CHANGELOG.md docs-site/src/content/docs/changelog.md
	run_script check-changelog-mirror.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'match (2 lines)'
}

@test "changelog mirror rejects missing headings before comparing empty blocks" {
	mkdir -p docs-site/src/content/docs
	printf 'nothing\n' >CHANGELOG.md
	cp CHANGELOG.md docs-site/src/content/docs/changelog.md
	run_script check-changelog-mirror.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'no '\''## [Unreleased]'\'' heading'
}

header_fixture() {
	local path
	for path in crates/crawlberg-ffi/include/crawlberg.h packages/go/include/crawlberg.h packages/csharp/Crawlberg/runtimes/osx-arm64/native/include/crawlberg.h packages/go/.lib/macos-arm64/include/crawlberg.h packages/java/src/main/resources/natives/macos-arm64/include/crawlberg.h; do
		mkdir -p "$(dirname "$path")"
		for ((i = 0; i < 405; i++)); do printf 'void cberg_test_%s(void);\n' "$i"; done >"$path"
	done
	xberg_stub git 'find crates packages -name crawlberg.h -type f | sort'
}

@test "vendored header gate checks all five tracked headers and four copies" {
	header_fixture
	run_script check-vendored-headers.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'expected 5, found 5'
	xberg_assert_output_contains 'checked 4 vendored copies of 5 tracked crawlberg.h paths; 0 failure(s)'
}

@test "vendored header gate rejects an ABI declaration not provided by canonical header" {
	header_fixture
	printf 'void cberg_removed(void);\n' >>packages/go/.lib/macos-arm64/include/crawlberg.h
	run_script check-vendored-headers.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'declares 1 thing(s)'
}

@test "chrome runtime installs providers before classic Chromium and connects content" {
	xberg_stub_trace sudo
	run_script prepare-chrome-runtime.sh 2 0 gnome-46-2404 mesa-2404
	xberg_assert_status 0
	xberg_assert_trace 'sudo snap install gnome-46-2404' 'sudo snap install mesa-2404' 'sudo snap install chromium --classic' 'sudo snap connect chromium:gnome-46-2404 gnome-46-2404' 'sudo snap connect chromium:gpu-2404 mesa-2404'
}

@test "chrome runtime stops at the retry budget without connecting failed providers" {
	xberg_stub sudo 'printf "%s\n" "$*" >>"$XBERG_TRACE"; exit 7'
	xberg_stub_trace sleep
	run_script prepare-chrome-runtime.sh 2 0 gnome-46-2404
	xberg_assert_status 1
	xberg_assert_output_contains 'failed after 2 attempts'
	xberg_assert_trace 'snap install gnome-46-2404' 'sleep 0' 'snap install gnome-46-2404'
}

@test "sccache wrapper retries cache failures with the wrapper disabled" {
	xberg_stub build 'if [ "${RUSTC_WRAPPER:-}" != "" ]; then echo "sccache error"; exit 8; fi' 'printf "%s:%s\n" "${RUSTC_WRAPPER:-}" "$SCCACHE_GHA_ENABLED" >"$XBERG_TRACE"'
	export RUSTC_WRAPPER=sccache
	run_script actions/setup-rust/build-with-sccache-fallback.sh build
	xberg_assert_status 0
	xberg_assert_file "$XBERG_TRACE" ':false'
	xberg_assert_output_contains 'Build succeeded without sccache'
}

@test "sccache wrapper does not retry compiler errors" {
	xberg_stub build 'echo called >>"$XBERG_TRACE"; echo "type mismatch"; exit 8'
	run_script actions/setup-rust/build-with-sccache-fallback.sh build
	xberg_assert_status 1
	xberg_assert_file "$XBERG_TRACE" called
}

@test "fingerprint cleanup removes only build caches and preserves library artifacts" {
	mkdir -p target/release/.fingerprint target/release/incremental
	touch target/release/.cargo-ok target/release/libcrawlberg_ffi.so
	xberg_stub_trace cargo
	run_script actions/setup-rust/cleanup-fingerprints.sh
	xberg_assert_status 0
	[ ! -e target/release/.fingerprint ]
	[ ! -e target/release/incremental ]
	[ ! -e target/release/.cargo-ok ]
	[ -f target/release/libcrawlberg_ffi.so ]
	xberg_assert_trace 'cargo --version'
}

@test "fingerprint cleanup rejects an unusable Cargo after cleanup" {
	xberg_stub_exit cargo 8
	run_script actions/setup-rust/cleanup-fingerprints.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'Cargo is broken after cleanup'
}

@test "benchmark cleanup delegates to the Docker cleanup script" {
	mkdir -p scripts/ci/docker
	printf 'printf "delegated\\n"\n' >scripts/ci/docker/free-disk-space.sh
	run_script benchmarks/free-disk-space.sh
	xberg_assert_status 0
	xberg_assert_output delegated
}

@test "benchmark cleanup propagates delegated cleanup errors" {
	mkdir -p scripts/ci/docker
	printf 'echo cleanup-failed; exit 8\n' >scripts/ci/docker/free-disk-space.sh
	run_script benchmarks/free-disk-space.sh
	xberg_assert_status 8
	xberg_assert_output cleanup-failed
}
