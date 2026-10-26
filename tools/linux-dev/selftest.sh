#!/usr/bin/env bash
# Builds the desktop shell and runs its headless self-test under Xvfb with a
# private D-Bus session and throwaway keyring. Containers/CI only.
set -euo pipefail
cargo build -p pigeon
export HOME="$(mktemp -d)" XDG_DATA_HOME="" XDG_CONFIG_HOME=""
dbus-run-session -- bash -c '
  echo -n "throwaway-test-password" | gnome-keyring-daemon --unlock --components=secrets >/dev/null
  PIGEON_SELFTEST=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 xvfb-run -a target/debug/pigeon
'
