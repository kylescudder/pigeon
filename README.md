<img src="assets/icon.svg" width="64" height="64" alt="">

# Pigeon

A modern, local-first IMAP/SMTP email client for NixOS. It uses an encrypted local store, fast search, and a Tauri + TanStack desktop shell.

- [Product and delivery plan](docs/product-plan.md)
- [Milestone 0 foundation spike](docs/foundation-spike.md)

## Test the foundation on NixOS

From the updated checkout:

```sh
git pull --ff-only
nix flake check -L
nix run
```

This is the foundation build, not a working mail client yet. Follow the
[NixOS verification checklist](docs/foundation-spike.md#to-finish-milestone-0-on-the-nixos-machine)
to check the real desktop, keyring, viewer isolation, and window lifecycle.
The x86_64 build and NixOS desktop behaviour still need verification.

## Development

```sh
nix develop          # toolchain: Rust, Bun, cargo-tauri, WebKitGTK
bun install
bun run tauri dev    # run the desktop app
nix flake check -L   # package, tests, and native self-test
```

After changing `bun.lock`, run `tools/update-node-modules-hash.sh`.
