#!/usr/bin/env bash

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_LOG="$SCRIPT_DIR/../chrome-memwatch.log"

INTERVAL=${INTERVAL:-5}
LOG_FILE=${LOG_FILE:-$DEFAULT_LOG}
NOISE_THRESHOLD=${NOISE_THRESHOLD:-2}

HELPER_REGEX='(Google Chrome Helper|Chromium Helper|chrome|chromium).*--remote-debugging-port'

declare -a RUNNERS=(
  "rust:cargo test"
  "python:pytest"
  "node:vitest"
  "ruby:rspec"
  "php:phpunit"
  "java:org.apache.maven.surefire"
  "java:surefire-booter"
  "go:go test"
  "elixir:mix test"
  "elixir:beam.smp.*test"
  "dart:dart test"
  "csharp:dotnet test"
  "swift:swift test"
  "swift:xctest"
  "zig:zig build test"
  "zig:zig-out/test"
  "wasm:wasm-pack test"
  "wasm:vitest.*wasm"
  "c:e2e_c"
  "kotlin:gradle.*test"
)

count_helpers() {
  pgrep -fl "$HELPER_REGEX" 2>/dev/null | wc -l | tr -d ' '
}

detect_runner() {
  local procs
  procs="$(ps -axo command= 2>/dev/null)"
  local entry tag pattern
  for entry in "${RUNNERS[@]}"; do
    tag="${entry%%:*}"
    pattern="${entry#*:}"
    if printf '%s\n' "$procs" | grep -Eq -- "$pattern"; then
      echo "$tag"
      return
    fi
  done
  echo ""
}

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

log_sample() {
  local now="$1" runner="$2" count="$3"
  printf '%s runner=%s helpers=%d\n' "$now" "${runner:-none}" "$count" >>"$LOG_FILE"
}

trace() {
  printf '%s TRACE %s\n' "$(ts)" "$*" >>"$LOG_FILE"
}

emit() { printf '%s %s\n' "$(ts)" "$*"; }

emit "MEMWATCH start interval=${INTERVAL}s threshold=${NOISE_THRESHOLD} log=$LOG_FILE"

prev_runner=""
session_start_helpers=0
session_peak_helpers=0
session_started_at=""

while true; do
  cur_runner="$(detect_runner)"
  cur_helpers="$(count_helpers)"
  now="$(ts)"
  log_sample "$now" "$cur_runner" "$cur_helpers"

  if [ -n "$cur_runner" ] && [ "$cur_runner" != "$prev_runner" ]; then
    if [ -n "$prev_runner" ]; then
      end_helpers="$cur_helpers"
      delta=$((end_helpers - session_start_helpers))
      if [ "$delta" -ge "$NOISE_THRESHOLD" ]; then
        emit "LEAK runner=$prev_runner started=$session_started_at delta_helpers=+$delta peak=$session_peak_helpers end=$end_helpers"
      else
        trace "SESSION-END runner=$prev_runner peak=$session_peak_helpers end=$end_helpers (clean)"
      fi
    fi
    session_start_helpers="$cur_helpers"
    session_peak_helpers="$cur_helpers"
    session_started_at="$now"
    trace "SESSION-START runner=$cur_runner baseline_helpers=$cur_helpers"
  fi

  if [ -n "$cur_runner" ] && [ "$cur_helpers" -gt "$session_peak_helpers" ]; then
    session_peak_helpers="$cur_helpers"
  fi

  if [ -z "$cur_runner" ] && [ -n "$prev_runner" ]; then
    end_helpers="$cur_helpers"
    delta=$((end_helpers - session_start_helpers))
    if [ "$delta" -ge "$NOISE_THRESHOLD" ]; then
      emit "LEAK runner=$prev_runner started=$session_started_at delta_helpers=+$delta peak=$session_peak_helpers end=$end_helpers"
    else
      trace "SESSION-END runner=$prev_runner peak=$session_peak_helpers end=$end_helpers (clean)"
    fi
    session_start_helpers=0
    session_peak_helpers=0
    session_started_at=""
  fi

  if [ -z "$cur_runner" ] && [ "$cur_helpers" -ge "$NOISE_THRESHOLD" ]; then
    emit "ORPHAN-HELPERS count=$cur_helpers (no test runner running)"
  fi

  prev_runner="$cur_runner"
  sleep "$INTERVAL"
done
