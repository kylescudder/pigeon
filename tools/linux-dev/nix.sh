#!/usr/bin/env bash
# Runs a nix command against this repository in a nixos/nix container with a
# persistent store volume. Usage: tools/linux-dev/nix.sh flake check -L
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
docker run --rm --privileged -v pigeon-nix:/nix -v "$repo":/src -w /src nixos/nix:latest sh -c '
  git config --global --add safe.directory /src
  exec nix --extra-experimental-features "nix-command flakes" --option sandbox true --option filter-syscalls false "$@"
' nix "$@"
