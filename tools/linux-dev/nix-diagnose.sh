#!/usr/bin/env bash
# Runs the packaged binary's sync diagnostic with a throwaway HOME and keyring.
# Runs inside the nixos/nix container, via tools/linux-dev/nix.sh shell.
set -euo pipefail
export HOME="$(mktemp -d)" XDG_RUNTIME_DIR="$(mktemp -d)"
bin=$(nix --extra-experimental-features "nix-command flakes" build .#pigeon --no-link --print-out-paths --option filter-syscalls false)
dbus-run-session --config-file="$DBUS_SESSION_CONF" -- bash -c "
  echo -n throwaway-test-password | gnome-keyring-daemon --unlock --components=secrets >/dev/null
  xvfb-run -a $bin/bin/pigeon --diagnose-sync
"
