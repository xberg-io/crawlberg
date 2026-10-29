#!/usr/bin/env bash
#MISE description="Run one published-package test app suite in test_apps/<lang>"
#MISE hide=true
#USAGE arg "<suite>" help="smoke or comprehensive"
#USAGE arg "<lang>" help="test_apps directory name, e.g. python or kotlin_android"
# shellcheck disable=SC2154 # usage_* are set by mise from the #USAGE lines
set -euo pipefail

suite="$usage_suite"
lang="$usage_lang"
case "$suite" in
smoke | comprehensive) ;;
*)
  echo "suite must be smoke or comprehensive, got: $suite" >&2
  exit 2
  ;;
esac
root="$PWD"
[ -d "test_apps/$lang" ] || {
  echo "no test app for $lang: test_apps/$lang is missing" >&2
  exit 2
}

# Picks smoke or comprehensive arguments for the current suite.
pick() {
  if [ "$suite" = smoke ]; then printf '%s' "$1"; else printf '%s' "$2"; fi
}

# AGP 8.13.0 rejects JDK 25+ as the Gradle daemon. jvmToolchain(17) in build.gradle.kts pins the
# compilation JDK but NOT the daemon, so JAVA_HOME is overridden when the host JDK is too new. ~keep
detect_jdk() {
  local dir java_path
  if [ "$(uname)" = "Darwin" ]; then
    for dir in /opt/homebrew/opt/openjdk@17 /opt/homebrew/opt/openjdk@21; do
      if [ -d "$dir" ]; then
        echo "$dir"
        return 0
      fi
    done
    /usr/libexec/java_home -v17 2>/dev/null || /usr/libexec/java_home -v21 2>/dev/null
  elif [ "$(uname)" = "Linux" ]; then
    if command -v update-alternatives >/dev/null 2>&1; then
      java_path=$(update-alternatives --list java 2>/dev/null | grep -E 'java-17|java-21|openjdk-17|openjdk-21' | head -1 || true)
      if [ -n "$java_path" ]; then
        dirname "$(dirname "$java_path")"
        return 0
      fi
    fi
    for dir in /usr/lib/jvm/java-17-openjdk* /usr/lib/jvm/java-21-openjdk* /usr/lib/jvm/temurin-17 /usr/lib/jvm/temurin-21; do
      if [ -d "$dir" ]; then
        echo "$dir"
        return 0
      fi
    done
  fi
  return 1
}

dart_native_dir() {
  case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) printf '%s' "$root/packages/dart/lib/src/native/macos-arm64" ;;
  Darwin-x86_64) printf '%s' "$root/packages/dart/lib/src/native/macos-x86_64" ;;
  Linux-x86_64 | Linux-amd64) printf '%s' "$root/packages/dart/lib/src/native/linux-x86_64" ;;
  Linux-aarch64 | Linux-arm64) printf '%s' "$root/packages/dart/lib/src/native/linux-arm64" ;;
  *) printf '' ;;
  esac
}

if [ "$lang" = dart ] && [ "$suite" = smoke ]; then
  mise run dart:build:release
fi

cd "test_apps/$lang"
case "$lang" in
rust)
  cargo run
  ;;
python)
  uv sync
  uv run pytest "$(pick tests/test_scrape.py tests/)" -v
  ;;
node | wasm)
  pnpm install
  if [ "$suite" = smoke ]; then pnpm exec vitest run tests/scrape.test.ts; else pnpm exec vitest run; fi
  ;;
ruby)
  bundle install
  bundle exec rspec "$(pick spec/scrape_spec.rb spec/)"
  ;;
go)
  export GOWORK=off
  go mod download
  if [ "$suite" = smoke ]; then
    go run github.com/xberg-io/crawlberg/packages/go/cmd/setup
    go test -v -run "Test_Scrape"
  else
    go test -v ./...
  fi
  ;;
java)
  if [ "$suite" = smoke ]; then mvn test -Dtest=ScrapeTest; else mvn test; fi
  ;;
elixir)
  export CRAWLBERG_ALLOW_PRIVATE_NETWORK=true
  mix deps.get
  if [ "$suite" = smoke ]; then mix test test/scrape_test.exs; else mix test; fi
  ;;
php)
  export CRAWLBERG_ALLOW_PRIVATE_NETWORK=true
  bash install.sh
  composer install
  if [ "$suite" = smoke ]; then
    php -dextension=crawlberg vendor/bin/phpunit tests/ScrapeTest.php
  else
    php -dextension=crawlberg vendor/bin/phpunit
  fi
  ;;
csharp)
  export CRAWLBERG_ALLOW_PRIVATE_NETWORK=true
  dotnet restore
  if [ "$suite" = smoke ]; then dotnet test --filter FullyQualifiedName~ScrapeTests; else dotnet test; fi
  ;;
c)
  bash download_ffi.sh
  make "$(pick smoke test)"
  ;;
dart)
  if [ "$suite" = smoke ]; then
    FRB_DART_LOAD_EXTERNAL_LIBRARY_NATIVE_LIB_DIR="$(dart_native_dir)"
    export FRB_DART_LOAD_EXTERNAL_LIBRARY_NATIVE_LIB_DIR
    dart pub get
    dart test test/scrape_test.dart
  else
    dart pub get
    dart test
  fi
  ;;
zig)
  zig fetch --save "$(awk -F'"' '/\.url =/ {print $2; exit}' build.zig.zon)"
  zig build test
  ;;
kotlin_android)
  if [ -z "${ANDROID_HOME:-}${ANDROID_SDK_ROOT:-}" ]; then
    echo "SKIP: kotlin_android $suite requires ANDROID_HOME or ANDROID_SDK_ROOT"
    exit 0
  fi
  if ! JDK_HOME=$(detect_jdk) || [ -z "$JDK_HOME" ]; then
    echo "SKIP: kotlin_android $suite requires JDK 17 or 21. Install with: brew install openjdk@17 (macOS)" \
      "or sdk install java 17.0.13-zulu (sdkman)"
    exit 0
  fi
  export JAVA_HOME="$JDK_HOME" PATH="$JDK_HOME/bin:$PATH"
  ./gradlew test
  ;;
swift_e2e)
  swift test
  ;;
brew)
  ./run_tests.sh
  ;;
*)
  echo "no $suite commands for test app $lang" >&2
  exit 2
  ;;
esac
