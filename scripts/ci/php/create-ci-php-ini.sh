#!/bin/bash

set -e


SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../" && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-.}"
INI_FILE="$OUTPUT_DIR/php-crawlberg.ini"

echo "=== Creating CI PHP ini file ==="
echo "Repo root: $REPO_ROOT"
echo "Output file: $INI_FILE"
echo ""

if [[ "$OSTYPE" == "linux-gnu"* ]]; then
  EXT_FILE="libcrawlberg_php.so"
elif [[ "$OSTYPE" == "darwin"* ]]; then
  EXT_FILE="libcrawlberg_php.dylib"
elif [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "win32" ]]; then
  EXT_FILE="crawlberg_php.dll"
else
  echo "Warning: Unknown OS type: $OSTYPE - assuming Linux"
  EXT_FILE="libcrawlberg_php.so"
fi

BUILT_EXT=""
TARGET_DIR=""
for candidate_dir in "$REPO_ROOT/target/release" "$REPO_ROOT/target/debug"; do
  if [ -f "$candidate_dir/$EXT_FILE" ]; then
    BUILT_EXT="$candidate_dir/$EXT_FILE"
    TARGET_DIR="$candidate_dir"
    break
  fi
done

if [ -z "$BUILT_EXT" ]; then
  echo "ERROR: Built extension $EXT_FILE not found in target/release or target/debug"
  for candidate_dir in "$REPO_ROOT/target/release" "$REPO_ROOT/target/debug"; do
    echo ""
    echo "Available files in $candidate_dir:"
    find "$candidate_dir" -maxdepth 1 -iname "*crawlberg*" -type f 2>/dev/null || echo "  (directory missing or empty)"
  done
  exit 1
fi

echo "Target dir: $TARGET_DIR"

echo "Found built extension: $BUILT_EXT"
echo "Extension file size: $(du -h "$BUILT_EXT" | cut -f1)"
echo ""

if [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "win32" ]]; then
  DISPLAY_DIR="${TARGET_DIR//\\/\/}"
else
  DISPLAY_DIR="$TARGET_DIR"
fi

DEFAULT_EXT_DIR="$(php -r 'echo ini_get("extension_dir");' 2>/dev/null || true)"
if [ -z "$DEFAULT_EXT_DIR" ]; then
  DEFAULT_EXT_DIR="$(php-config --extension-dir 2>/dev/null || true)"
fi
if [ -z "$DEFAULT_EXT_DIR" ]; then
  echo "ERROR: could not determine PHP extension_dir"
  exit 1
fi

echo "Detected PHP extension_dir: $DEFAULT_EXT_DIR"

if cat >"$INI_FILE" <<EOF; then
; Crawlberg PHP Extension Configuration for CI Testing
; This file is generated automatically by create-ci-php-ini.sh
; It allows loading the locally-built extension without system-wide installation

; Load the Crawlberg PHP extension using full path
extension="$DISPLAY_DIR/$EXT_FILE"

; Mirror the active PHP's extension_dir so PHPUnit-required extensions resolve
extension_dir = $DEFAULT_EXT_DIR

; PHPUnit requires: dom, json, libxml, mbstring, tokenizer, xml, xmlwriter, ctype
extension = ctype
extension = dom
extension = libxml
extension = mbstring
extension = tokenizer
extension = xml
extension = xmlwriter
EOF
  echo "✓ INI file created: $INI_FILE"
  echo ""
  echo "INI file contents:"
  cat "$INI_FILE"
  echo ""
  echo "To use this file with PHPUnit:"
  echo "  php -c $INI_FILE vendor/bin/phpunit"
  echo ""
  echo "Or pass it to task:"
  echo "  task php:test:ci"
else
  echo "✗ Failed to create INI file"
  exit 1
fi
