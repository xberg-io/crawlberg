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

teardown() {
	/bin/rm -f "$XBERG_STUB_BIN/rm"
}

run_script() {
	local script="$1"
	shift
	run bash "$SOURCE_ROOT/scripts/ci/$script" "$@"
}

@test "Node unpack installs the discovered tarball into the workspace root" {
	touch crates/crawlberg-node/crawlberg-node-1.2.3.tgz
	xberg_stub_trace pnpm
	xberg_stub cygpath 'printf "%s\n" "$2"'
	run_script node/unpack-bindings.sh
	xberg_assert_status 0
	xberg_assert_trace "pnpm add --workspace-root file:$REPO_ROOT/crates/crawlberg-node/crawlberg-node-1.2.3.tgz"
}

@test "Node unpack rejects a missing tarball without installing anything" {
	xberg_stub_trace pnpm
	run_script node/unpack-bindings.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'No crawlberg-node tarball found'
	xberg_assert_trace_empty
}

@test "TypeScript artifact verifier counts packaged dist entries" {
	mkdir -p "$XBERG_WORK/package/dist" crates/crawlberg-node/dist
	echo exported >"$XBERG_WORK/package/dist/index.js"
	tar czf crates/crawlberg-node/binding.tgz -C "$XBERG_WORK" package
	run_script node/verify-ts-artifacts.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'dist files in tarball: 2'
}

@test "TypeScript artifact verifier rejects an absent tarball" {
	run_script node/verify-ts-artifacts.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'ERROR: No tarball found'
}

php_script() {
	mkdir -p scripts/ci/php
	cp "$SOURCE_ROOT/scripts/ci/php/create-ci-php-ini.sh" scripts/ci/php/
}

@test "PHP ini creator points to the actual local extension and active extension directory" {
	php_script
	touch target/release/libcrawlberg_php.so
	xberg_stub php 'echo /php/extensions'
	run env OSTYPE=linux-gnu bash scripts/ci/php/create-ci-php-ini.sh
	xberg_assert_status 0
	grep -Fx "extension=\"$REPO_ROOT/target/release/libcrawlberg_php.so\"" php-crawlberg.ini
	grep -Fx 'extension_dir = /php/extensions' php-crawlberg.ini
}

@test "PHP ini creator refuses missing extension artifacts" {
	php_script
	run env OSTYPE=linux-gnu bash scripts/ci/php/create-ci-php-ini.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'Built extension libcrawlberg_php.so not found'
	[ ! -e php-crawlberg.ini ]
}

@test "Python cleanup removes stale wheel and binding artifacts and preserves source" {
	mkdir -p target/wheels target/maturin packages/python/crawlberg
	touch target/wheels/stale.whl target/maturin/stale packages/python/crawlberg/_internal_bindings.so packages/python/crawlberg/__init__.py
	run_script python/clean-artifacts.sh
	xberg_assert_status 0
	[ ! -d target/wheels ]
	[ ! -d target/maturin ]
	[ ! -e packages/python/crawlberg/_internal_bindings.so ]
	[ -f packages/python/crawlberg/__init__.py ]
}

@test "Python cleanup propagates removal failures" {
	xberg_stub_exit rm 8
	run_script python/clean-artifacts.sh
	xberg_assert_status 8
}

@test "Python disk cleanup requests package cache and Docker cleanup through external tools" {
	for cmd in sudo docker rm df; do xberg_stub_trace "$cmd"; done
	run_script python/free-disk-space.sh
	xberg_assert_status 0
	grep -Fx 'sudo apt-get autoremove --yes' "$XBERG_TRACE"
	grep -Fx 'docker system prune -af --volumes' "$XBERG_TRACE"
	grep -Fx 'docker builder prune -af' "$XBERG_TRACE"
	grep -F "rm -rf $HOME/.cargo/registry/cache $HOME/.cargo/git/db" "$XBERG_TRACE"
}

@test "Python disk cleanup tolerates external cleanup errors and still reports final disk space" {
	for cmd in sudo docker rm; do xberg_stub_exit "$cmd" 8; done
	xberg_stub df 'echo disk-fixture'
	run_script python/free-disk-space.sh
	xberg_assert_status 0
	xberg_assert_output_contains 'Final disk space after cleanup'
	xberg_assert_output_contains 'disk-fixture'
}

@test "spacy installer requests the model wheel via uv" {
	xberg_stub_trace uv
	run_script python/install-spacy-model.sh
	xberg_assert_status 0
	xberg_assert_trace 'uv pip install https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl'
}

@test "spacy installer propagates uv installation failure" {
	xberg_stub_exit uv 8
	run_script python/install-spacy-model.sh
	xberg_assert_status 8
}

@test "Python wheel installer installs the discovered dist wheel" {
	mkdir dist
	touch dist/crawlberg-1.2.3.whl
	xberg_stub_trace python
	run_script python/install-wheel.sh
	xberg_assert_status 0
	xberg_assert_trace 'python -m pip install dist/crawlberg-1.2.3.whl'
}

@test "Python wheel installer rejects absent wheels before pip" {
	xberg_stub_trace python
	run_script python/install-wheel.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'No wheel found in dist/'
	xberg_assert_trace_empty
}

@test "Python wheel smoke installs offline before printing its version" {
	xberg_stub_trace pip
	xberg_stub_trace python
	run_script python/smoke-test-wheel.sh
	xberg_assert_status 0
	xberg_assert_trace "pip install --no-index --find-links $REPO_ROOT/target/wheels crawlberg" "python $REPO_ROOT/scripts/python/print_crawlberg_version.py"
}

@test "Python wheel smoke stops when installation fails" {
	xberg_stub_exit pip 8
	xberg_stub_trace python
	run_script python/smoke-test-wheel.sh
	xberg_assert_status 8
	xberg_assert_trace_empty
}

ruby_tools() {
	for cmd in ruby rustc cargo gem python3; do xberg_stub_trace "$cmd"; done
}

@test "Ruby compilation invokes vendoring then the verbose native build with bounded jobs" {
	ruby_tools
	xberg_stub bundle 'printf "%s:%s:%s:%s\n" "$*" "$CARGO_BUILD_JOBS" "$RUST_BACKTRACE" "$RB_SYS_VERBOSE" >>"$XBERG_TRACE"'
	run_script ruby/compile-extension.sh
	xberg_assert_status 0
	grep -Fx "python3 $REPO_ROOT/scripts/ci/ruby/vendor-crawlberg-core.py" "$XBERG_TRACE"
	grep -Fx 'exec rake compile --verbose --trace:1:1:1' "$XBERG_TRACE"
	xberg_assert_output_contains 'Compilation complete'
}

@test "Ruby compilation emits diagnostics and fails when rake fails" {
	ruby_tools
	xberg_stub_exit bundle 8
	run_script ruby/compile-extension.sh
	xberg_assert_status 1
	xberg_assert_output_contains 'ERROR: rake compile failed'
	xberg_assert_output_contains 'Checking gem installation'
}

@test "Ruby gem installer selects the local packaged gem without documentation" {
	mkdir -p packages/ruby/pkg
	touch packages/ruby/pkg/crawlberg-1.2.3.gem
	xberg_stub_trace gem
	run_script ruby/install-gem.sh
	xberg_assert_status 0
	xberg_assert_trace 'gem install pkg/crawlberg-1.2.3.gem --no-document'
}

@test "Ruby gem installer propagates installation failures" {
	xberg_stub_exit gem 8
	run_script ruby/install-gem.sh
	xberg_assert_status 8
}

@test "Ruby dependency installer sets local bundle path and exports workflow environment" {
	xberg_stub_trace bundle
	run_script ruby/install-ruby-deps.sh
	xberg_assert_status 0
	xberg_assert_trace 'bundle config set deployment false' "bundle config set path $REPO_ROOT/packages/ruby/.bundle/bundle" 'bundle install --jobs 4'
	xberg_assert_github_env "BUNDLE_GEMFILE=$REPO_ROOT/packages/ruby/Gemfile
BUNDLE_PATH=$REPO_ROOT/packages/ruby/.bundle/bundle"
}

@test "Ruby dependency installer stops after failed configuration" {
	xberg_stub_exit bundle 8
	run_script ruby/install-ruby-deps.sh
	xberg_assert_status 8
	xberg_assert_output_contains 'Installing Ruby dependencies'
}

@test "wasm installer uses an existing wasm-pack without downloading" {
	xberg_stub wasm-pack 'echo wasm-pack-fixture'
	xberg_stub_curl_offline
	run_script wasm/install-wasm-pack.sh
	xberg_assert_status 0
	xberg_assert_output wasm-pack-fixture
	xberg_assert_github_path ''
}

@test "wasm installer propagates broken existing wasm-pack instead of hiding failure" {
	xberg_stub_exit wasm-pack 8
	xberg_stub_curl_offline
	run_script wasm/install-wasm-pack.sh
	xberg_assert_status 8
	xberg_assert_github_path ''
}
