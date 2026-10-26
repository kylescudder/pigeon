#!/usr/bin/env bash
# Recomputes the per-system node_modules hashes in nix/package.nix after
# bun.lock changes. Installs for each target CPU with the flake's pinned Bun, so
# one builder can compute every hash. Uses the nixos/nix container when nix is
# not installed locally.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"

compute='
set -e
git config --global --add safe.directory /repo 2>/dev/null || true
nixx() { nix --extra-experimental-features "nix-command flakes" "$@"; }
bun=$(nixx eval --raw /repo#pigeon.nodeModules.nativeBuildInputs --apply "x: toString x")
for pair in x86_64-linux:x64 aarch64-linux:arm64; do
  d=$(mktemp -d); cp /repo/package.json /repo/bun.lock "$d"/
  (cd "$d" && HOME="$d" "$bun/bin/bun" install --frozen-lockfile --ignore-scripts --no-progress \
     --os=linux --cpu="${pair#*:}" >/dev/null && mkdir out && cp -R node_modules out/)
  echo "${pair%%:*} $(nixx hash path "$d/out")"
done'

if command -v nix >/dev/null; then
  hashes=$(sed "s|/repo|$repo|g" <<<"$compute" | bash)
else
  hashes=$(docker run --rm -v pigeon-nix:/nix -v "$repo":/repo:ro nixos/nix:latest sh -c "$compute")
fi

while read -r system hash; do
  sed -i.bak "s|\(${system} = \)\"[^\"]*\";|\1\"${hash}\";|" "$repo/nix/package.nix"
  echo "$system $hash"
done <<<"$hashes"
rm -f "$repo/nix/package.nix.bak"
