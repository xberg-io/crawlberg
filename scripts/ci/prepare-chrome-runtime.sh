# shellcheck shell=bash
# ~keep Task invokes this through bash; declare its interpreter for static checks.
set -euo pipefail

max_attempts="$1"
retry_delay="$2"
shift 2

# ~keep A partial Chromium install can return success while its provider assertion fetch failed.
# Retry the providers independently before setup-chrome runs its launch smoke test.
for provider in "$@"; do
  attempt=1
  until sudo snap install "$provider"; do
    if [ "$attempt" -ge "$max_attempts" ]; then
      echo "Chromium runtime provider $provider failed after $attempt attempts" >&2
      exit 1
    fi
    sleep "$retry_delay"
    attempt=$((attempt + 1))
  done
done

# ~keep Reconnect an already-present partial install; a fresh Chromium install autoconnects these.
if sudo snap list chromium >/dev/null 2>&1; then
  sudo snap connect chromium:gnome-46-2404 gnome-46-2404
  sudo snap connect chromium:gpu-2404 mesa-2404
fi
