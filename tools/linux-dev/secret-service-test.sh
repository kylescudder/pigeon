#!/usr/bin/env bash
# Runs the Secret Service tests inside a private D-Bus session with a throwaway
# GNOME Keyring. Intended for containers/CI only.
set -euo pipefail
export HOME="$(mktemp -d)"
cargo test -p pigeon-core --test secret_service --no-run
DBUS_SESSION_BUS_ADDRESS= cargo test -p pigeon-core --test secret_service missing_session_bus_is_unavailable -- --exact --ignored
dbus-run-session -- bash -c '
  echo -n "throwaway-test-password" | gnome-keyring-daemon --unlock --components=secrets >/dev/null
  cargo test -p pigeon-core --test secret_service round_trip_then_locked_state_is_explicit -- --exact --ignored
'
