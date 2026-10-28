#!/usr/bin/env bash
set -euo pipefail

mode="${1:-check}"
root="$(git rev-parse --show-toplevel)"

java_dirs=(
  packages/java
  e2e/java
)

failed=0

has_plugin() {
  local pom="$1"
  local plugin="$2"
  grep -q "$plugin" "$pom" 2>/dev/null
}

for dir in "${java_dirs[@]}"; do
  full="$root/$dir"
  if [ ! -f "$full/pom.xml" ]; then
    continue
  fi

  echo "==> Linting $dir"
  cd "$full"

  case "$mode" in
  fix)
    if has_plugin "$full/pom.xml" "spotless-maven-plugin"; then
      mvn -q spotless:apply || failed=1
    fi
    ;;
  check)
    if has_plugin "$full/pom.xml" "spotless-maven-plugin"; then
      mvn -q spotless:check || failed=1
    fi
    if has_plugin "$full/pom.xml" "maven-checkstyle-plugin"; then
      mvn -q checkstyle:check || failed=1
    fi
    if has_plugin "$full/pom.xml" "maven-pmd-plugin"; then
      mvn -q pmd:check pmd:cpd-check || failed=1
    fi
    ;;
  *)
    echo "Usage: $0 [fix|check]" >&2
    exit 2
    ;;
  esac
done

exit $failed
