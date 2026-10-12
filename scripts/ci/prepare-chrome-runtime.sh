# shellcheck shell=bash
# ~keep Task invokes this through bash; declare its interpreter for static checks.
set -euo pipefail

max_attempts="$1"
retry_delay="$2"
shift 2

if ! [[ "$max_attempts" =~ ^[1-9][0-9]*$ && "$retry_delay" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  echo "Invalid Chromium runtime retry configuration" >&2
  exit 1
fi

# ~keep A partial Chromium install can return success while its provider assertion fetch failed.
# Retry the providers independently before setup-chrome runs its launch smoke test.
for provider in "$@" chromium; do
  install_args=("$provider")
  if [ "$provider" = chromium ]; then
    install_args+=(--classic)
  fi
  attempt=1
  until sudo snap install "${install_args[@]}"; do
    if [ "$attempt" -ge "$max_attempts" ]; then
      echo "Chromium runtime provider $provider failed after $attempt attempts" >&2
      exit 1
    fi
    sleep "$retry_delay"
    attempt=$((attempt + 1))
  done
done

# ~keep Fresh and partial installs both require explicit content connections before launch.
sudo snap connect chromium:gnome-46-2404 gnome-46-2404
sudo snap connect chromium:gpu-2404 mesa-2404
