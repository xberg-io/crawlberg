# shellcheck shell=bash
# ~keep Task invokes this through bash; declare its interpreter for static checks.
set -euo pipefail

max_attempts="$1"
retry_delay="$2"

if ! [[ "$max_attempts" =~ ^[1-9][0-9]*$ && "$retry_delay" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  echo "Invalid Chromium runtime retry configuration" >&2
  exit 1
fi

install_snap() {
  local name="$1" attempt=1
  until sudo snap install "$name"; do
    if [ "$attempt" -ge "$max_attempts" ]; then
      echo "Chromium runtime provider $name failed after $attempt attempts" >&2
      return 1
    fi
    sleep "$retry_delay"
    attempt=$((attempt + 1))
  done
}

install_snap chromium

# ~keep Detect the installed revision's plugs, including unconnected ones on a partial install.
plugs=$(sudo snap connections --all | awk '$1 == "content" && $2 ~ /^chromium:/ { print $2 }')
has_runtime_plugs() {
  grep -Fxq "chromium:$1" <<<"$plugs" && grep -Fxq "chromium:$2" <<<"$plugs"
}

if has_runtime_plugs gnome-core26 gpu-2604; then
  gnome=gnome-core26 gpu=gpu-2604 mesa=mesa-2604
elif has_runtime_plugs gnome-46-2404 gpu-2404; then
  gnome=gnome-46-2404 gpu=gpu-2404 mesa=mesa-2404
else
  printf 'Unsupported Chromium content plugs:\n%s\n' "$plugs" >&2
  exit 1
fi

# ~keep Snapd can report a Chromium install as successful even when provider assertions failed.
install_snap "$gnome"
install_snap "$mesa"
sudo snap connect "chromium:$gnome" "$gnome"
sudo snap connect "chromium:$gpu" "$mesa"
