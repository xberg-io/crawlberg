#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
variant="${TEST_VARIANT:-all}"
image="${IMAGE_NAME:-}"

while [ "$#" -gt 0 ]; do
  case "$1" in
  --variant)
    variant="${2:?missing variant}"
    shift 2
    ;;
  --image)
    image="${2:?missing image}"
    shift 2
    ;;
  --verbose) shift ;;
  --help)
    echo "Usage: $0 [--variant core|full|all] [--image IMAGE] [--verbose]"
    exit 0
    ;;
  *)
    echo "Unknown option: $1" >&2
    exit 1
    ;;
  esac
done

case "$variant" in
core | full) variants=("$variant") ;;
all) variants=(core full) ;;
*)
  echo "Invalid variant: $variant" >&2
  exit 1
  ;;
esac

for selected in "${variants[@]}"; do
  python3 "$script_dir/../ci/docker/test_config.py" --image "${image:-crawlberg:$selected}"
done
