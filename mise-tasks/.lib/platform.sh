# shellcheck shell=bash
# Platform guards for file tasks: a task gated to one platform fails loudly anywhere else.

# Fails unless the host OS (uname -s, lowercased; msys/mingw/cygwin count as windows) is $1.
require_os() {
  local os
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  case "$os" in mingw* | msys* | cygwin*) os=windows ;; esac
  if [ "$os" != "$1" ]; then
    echo "${MISE_TASK_NAME:-this task} runs only on $1, not $os" >&2
    exit 1
  fi
}

# Fails unless the host is $1 on architecture $2 (uname -m; aarch64 counts as arm64).
require_platform() {
  local arch
  require_os "$1"
  arch="$(uname -m)"
  if [ "$arch" = aarch64 ]; then arch=arm64; fi
  if [ "$arch" != "$2" ]; then
    echo "${MISE_TASK_NAME:-this task} runs only on $1/$2, not $1/$arch" >&2
    exit 1
  fi
}
