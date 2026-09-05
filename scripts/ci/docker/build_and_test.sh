#!/usr/bin/env bash
# =============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../" && pwd)"
DOCKERFILE="$REPO_ROOT/docker/Dockerfile.alpine"
IMAGE_NAME="${IMAGE_NAME:-crawlberg-test:latest}"
BUILD_IMAGE=true
VERBOSE=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build)
      BUILD_IMAGE=false
      shift
      ;;
    --image)
      IMAGE_NAME="$2"
      BUILD_IMAGE=false
      shift 2
      ;;
    --verbose)
      VERBOSE=true
      shift
      ;;
    --help|-h)
      grep "^#" "$0" | tail -n +2
      exit 0
      ;;
    *)
      echo "Error: unknown option: $1" >&2
      exit 2
      ;;
  esac
done

log_info() {
  echo -e "${BLUE}[INFO]${NC} $*" >&2
}

log_ok() {
  echo -e "${GREEN}[OK]${NC} $*" >&2
}

log_error() {
  echo -e "${RED}[ERROR]${NC} $*" >&2
}


if [[ "$BUILD_IMAGE" == "true" ]]; then
  log_info "Building Docker image: $IMAGE_NAME"
  log_info "Dockerfile: $DOCKERFILE"
  log_info "Context: $REPO_ROOT"

  if [[ ! -f "$DOCKERFILE" ]]; then
    log_error "Dockerfile not found: $DOCKERFILE"
    exit 1
  fi

  docker build \
    -f "$DOCKERFILE" \
    -t "$IMAGE_NAME" \
    "$REPO_ROOT"

  log_ok "Docker image built successfully"
fi


log_info "Verifying image: $IMAGE_NAME"
if ! docker inspect "$IMAGE_NAME" > /dev/null 2>&1; then
  log_error "Image not found: $IMAGE_NAME"
  exit 1
fi

log_ok "Image verified"


log_info "Running test suite..."

TEST_SCRIPT="$SCRIPT_DIR/test_docker.py"
TEST_ARGS=(
  --image "$IMAGE_NAME"
  --variant cli
)

if [[ "$VERBOSE" == "true" ]]; then
  TEST_ARGS+=(--verbose)
fi

if python3 "$TEST_SCRIPT" "${TEST_ARGS[@]}"; then
  log_ok "All tests passed"
  exit 0
else
  log_error "Tests failed"
  exit 1
fi
