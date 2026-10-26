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

## Interface

Use `Ctrl+K` (`Cmd+K` on macOS) to open the command menu. Appearance settings
provide light, dark, and system themes plus comfortable or compact spacing;
these preferences are saved on the device. Foundation checks are under Diagnostics.

## Development

```sh
nix develop          # toolchain: Rust, Bun, cargo-tauri, WebKitGTK
bun install
bun run tauri dev    # run the desktop app
nix flake check -L   # package, tests, and native self-test
```

After changing `bun.lock`, run `tools/update-node-modules-hash.sh`.

The frontend browser suite is `bun run test:e2e` (install Chromium with
`bunx --no-install playwright install chromium` on a compatible browser-test host).
CI runs it on Ubuntu in dark, light, and narrow layouts. Native-command test
fixtures exercise frontend feedback; real keyring and viewer-isolation checks
remain part of the native self-test.
