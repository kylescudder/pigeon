<img src="assets/icon.svg" width="64" height="64" alt="">

# Pigeon

A modern, local-first IMAP/SMTP email client for NixOS. It uses an encrypted local store, fast search, and a Tauri + TanStack desktop shell.

- [Product and delivery plan](docs/product-plan.md)
- [Receiving/search implementation and verification](docs/mail-engine.md)
- [Milestone 0 foundation spike](docs/foundation-spike.md)

## Try it on NixOS

Quit the previous background instance with **Quit Pigeon**, then from the checkout:

```sh
git pull --ff-only
nix flake check -L
nix run
```

This build connects IMAP accounts, downloads text bodies into SQLCipher,
reads cached mail offline, and searches across accounts/folders with saved
queries. Choose **Connect an account** and use the provider’s IMAP settings and
password/app password. The desktop Secret Service must be unlocked. TLS and
STARTTLS validate certificates; OAuth-only providers are not supported yet.

The receiving preview is read-only: opening mail does not mark it read on the
server. Sending, mailbox changes, and attachment downloads come in later
milestones. See the [receiving guide and test results](docs/mail-engine.md) for
progress/recovery behaviour, limits, and the NixOS verification checklist.

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

Run `bun run test` for typecheck and the production-build browser suite,
or `bun run test:e2e` for the browser suite alone. Install Chromium with
`bunx --no-install playwright install chromium` on a compatible browser-test host.
CI runs it on Ubuntu in dark, light, and narrow layouts. Native-command test
fixtures exercise frontend feedback; real keyring and viewer-isolation checks
remain part of the native self-test.

Run `tools/mail-test/run.sh` with Docker for six isolated Dovecot/TLS integration
tests. The explicit 100k-message search gate is:

```sh
cargo test --release --locked -p pigeon-core --test search_benchmark -- --ignored --nocapture
```

## Reporting a sync problem

If Pigeon reports that it could not parse a message, quit it from its menu and run:

```sh
nix run . -- --diagnose-sync
```

This retries uncached mail and prints a report that is safe to share. It keeps the structure of the failing server response and replaces values with their length and character types. It includes no message text, addresses, folder names, or credentials.
