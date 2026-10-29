#!/usr/bin/env bash
#MISE description="Run kotlin-android instrumented e2e tests on a running emulator (needs ANDROID_HOME and adb)"
#MISE dir="{{config_root}}/packages/kotlin-android"
set -euo pipefail

# ~keep Start the emulator first, e.g. `emulator -avd xberg-test-34 -no-window` or the GHA
# ~keep `reactivecircus/android-emulator-runner` action. MOCK_SERVER_URL defaults to the host
# ~keep loopback as the emulator sees it.
if [ -z "${ANDROID_HOME:-}" ]; then
  # shellcheck disable=SC2016 # the backticks are literal command examples
  echo 'ANDROID_HOME is unset — install the Android SDK (e.g. `brew install android-commandlinetools` then' \
    '`sdkmanager "platform-tools" "emulator" "platforms;android-34"' \
    '"system-images;android-34;google_apis;arm64-v8a"`) and export ANDROID_HOME before invoking this task.' >&2
  exit 1
fi
if ! command -v adb >/dev/null; then
  echo "adb not on PATH — add \$ANDROID_HOME/platform-tools to PATH" >&2
  exit 1
fi
adb wait-for-device
gradle connectedDebugAndroidTest --no-daemon -Pmock_server_url="${MOCK_SERVER_URL:-http://10.0.2.2:8765}"
