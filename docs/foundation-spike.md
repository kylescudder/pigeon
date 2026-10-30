# Milestone 0: foundation spike

Status: native diagnostics user-verified on NixOS, **13 of 13 passed** on 2026-10-26. Agent-run Linux checks also pass. The remaining desktop checklist items, including both Wayland/X11 environments, native deep-link reload, and background/reopen behaviour, remain unverified.

Validation source: foundation working tree on top of `7c2f0242dd29ae229a65bcc6e6e939175df7e900`, exercised on 2026-10-23 before its first commit. The results below describe those runs, not a completed x86_64 or NixOS desktop validation.

## User native validation, 2026-10-26

After publishing `b779cce`, the user ran the app on their NixOS machine, opened the isolation probe, and reran diagnostics. Their supplied screenshot shows **13 of 13 passed**, including an unlocked keyring, the existing encrypted store, SQLCipher/FTS5, in-memory temporary storage, WAL, schema migration, and viewer command denial. All four attempted native commands were denied, and the remote fetch failed.

The screenshot is retained unmodified with provenance under the ignored `evidence/native-validation/2026-10-26/` directory. It does not embed the exact binary revision, CPU architecture, or compositor. Treat it as user-provided runtime evidence for the reported checks, not proof of every desktop checklist item or an independent security audit.

## What exists

| Area | Location | Notes |
| --- | --- | --- |
| SPA | `src/`, `vite.config.ts` | TanStack Start SPA mode, Router, and Query. The shell is emitted as `dist/client/index.html`. |
| Core | `crates/pigeon-core` | SQLCipher store (`store`), keyring backends (`secrets`), the unlock state machine (`unlock`), and the HTML sanitiser/CSP (`viewer`). |
| Shell | `src-tauri` | A thin adapter over the core. It has a capability-gated command set, a `pigeon-viewer://` viewer scheme, hide-on-close, single-instance reopen, and a `PIGEON_SELFTEST=1` mode. |
| Nix | `flake.nix`, `nix/` | Provides the package, `nix run`, the dev shell, the formatter, and four checks (`package`, `core-tests`, `frontend`, `selftest`). |
| CI | `.github/workflows/ci.yml` | Runs `nix flake check`, nixfmt, rustfmt, and clippy on x86_64-linux. |
| Dev tools | `tools/` | Linux Docker image, container wrappers for nix and the self-test, and the node_modules hash updater. |

## Design decisions made during the spike

- **Raw 256-bit SQLCipher key.** The key is random, so SQLCipher's passphrase KDF is skipped. It is held in `Zeroizing` memory and its `Debug` output is redacted. `cipher_memory_security` is off: SQLCipher keeps keys in its own locked heap regardless, and the option adds a failing `mlock` to every SQLite allocation under normal memory limits.
- **The store fails closed.** If SQLCipher is not linked, FTS5 is absent, or the key is wrong, the open is refused. A missing key for an existing store is reported as `KeyMissing`. A replacement key is never generated, because that would orphan local-only data.
- **No plaintext secret fallback.** Keyring states are `Unavailable`, `Locked`, and `Unlocked`. Unlocking is never automatic; it is a separate `unlock()` called on user action. Session-only storage (`MemoryStore`) exists for the session-credential mode.
- **Viewer isolation uses separate webviews, not iframes.** Each message opens in its own webview labelled `viewer-*`, which no capability names. The app's commands are declared through `AppManifest`, so each one needs an explicit `allow-*` grant. Every command also checks that the caller's label is `main`, as defence in depth against the Linux limitation where frames cannot be told apart.
  - **Finding:** Tauri classifies the custom-scheme viewer as `URL: local`. Denial therefore depends on the webview label, not the origin. Message HTML must never be rendered inside the main webview, including in an iframe.
- **Remote content is blocked twice.** The sanitiser (ammonia) removes scripts, styles, frames, forms, event handlers, and non-image `data:` URLs. It keeps only `cid:`/`data:image` images unless remote images are allowed, and then only over https. The viewer CSP independently sets `default-src 'none'`, `connect-src 'none'`, and `img-src data:`.
- **The probe does not use IPC.** The isolation probe deliberately gets script execution and IPC connectivity. Its native calls must still be rejected by the ACL. It reports results by navigating to `/probe-done`, because IPC is not available to it.
- **Bun dependencies come from a fixed-output derivation pinned by `bun.lock`.** Hashes are per system. They are computed with the flake's pinned Bun using `--os/--cpu`, so one builder can produce all of them; run `tools/update-node-modules-hash.sh` after lockfile changes.

## Gate results

| Gate | Result | Evidence |
| --- | --- | --- |
| SQLCipher linkage plus FTS5 | **Pass** | SQLCipher 4.14.0 community (OpenSSL) is linked and FTS5 is a compile option. FTS search works, including diacritic folding, and the index stays consistent after deletes. |
| Readable only with the right key | **Pass** | A wrong key gives `WrongKey`, and the right key reopens the store with its data intact. |
| Encrypted main file and WAL | **Pass** | A canary string is absent from both `mail.db` and a non-empty `mail.db-wal`. The file has no `SQLite format 3` header and no stray temp files appear. |
| Memory-only temp storage | **Pass** | `temp_store=2`. |
| Keyring unlocked, locked, and unavailable | **Pass** | Tested against real GNOME Keyring in a throwaway D-Bus session: round trip, replace, and delete work. After `lock()`, the state is `Locked` and reads and writes are refused. With no bus, the state is `Unavailable`. |
| Keyring key-missing and wrong-key handling | **Pass** | Unit tests over `MemoryStore` show that a missing key is never replaced and the store file is left byte-identical. |
| Viewer native-command denial | **Pass** | Probe in real WebKitGTK under Xvfb, using both the Debian debug build and the Nix release build in the sandbox. `viewer_probe_reached`, `foundation_report`, `quit_app`, and `plugin:window\|close` were all denied by the ACL, and the backend breach flag stayed false. |
| Remote-content policy | **Pass (partial)** | Sanitiser and CSP tests pass and the probe's `fetch` failed. In the Nix sandbox there is no network at all, so only the Docker run is evidence that the CSP did the blocking. Image-loading behaviour has not been checked by eye. |
| Static assets and route navigation | **Partial** | `bun run build` produces `index.html` plus assets, and the Nix `frontend` check runs typecheck and build. In-app navigation and reloading on a deep link inside Tauri have not been checked. |
| Real NixOS launch and Wayland/X11 | **Partial, user-verified** | The user supplied a running native app screenshot with 13/13 diagnostics passed. Both compositor environments have not been individually confirmed. |
| Background and reopen | **Not run** | Implemented: closing hides the main window, relaunching through single-instance re-shows it, and Quit exits. Milestone 4 makes closing configurable, adds `--background` and the Home Manager login service; see the [desktop checklist](desktop.md). The behaviour has not been observed on a desktop. |
| x86_64-linux build | **Not run** | Everything above ran on aarch64-linux (Docker on Apple silicon). The x86_64 node_modules hash was computed cross-architecture with the pinned Bun, but the package has not been built on x86_64. CI covers this once pushed. |

## Commands

```sh
# Inside the dev shell (NixOS or any Linux with nix)
nix develop
bun install && bun run tauri dev     # interactive
nix flake check -L                   # all checks, including the native self-test
nix run                              # packaged app

# From a non-Linux host with Docker
tools/linux-dev/nix.sh flake check -L
docker build -t pigeon-linux-dev tools/linux-dev
docker run --rm -v "$PWD":/src pigeon-linux-dev tools/linux-dev/selftest.sh
```

## To finish Milestone 0 on the NixOS machine

1. `nix flake check -L`, then `nix run`. Confirm that the window opens under Wayland, and again with `GDK_BACKEND=x11`.
2. Open Diagnostics and confirm the keyring/store rows pass against the real login keyring. Lock the keyring, re-run the checks, and confirm the store reports locked rather than failing.
3. Open the isolation probe. Every row should read PASS, then re-run checks and confirm "probe completed".
4. Open the sanitised fixture and confirm no remote images, forms, or frames are visible.
5. Navigate to Diagnostics, then reload (Ctrl+R) and confirm the route survives.
6. Close the window and confirm the process keeps running. Relaunch from the launcher and confirm the same instance reappears. Use Quit Pigeon and confirm it exits.
7. Check display scaling and dark mode.

The native storage and viewer-denial diagnostics are now user-verified. Milestone 1 mail-engine work (IMAP sync, persistence, search) can proceed while the remaining desktop checks stay explicit release gates. The sync schema now extends the provisional `messages` table through migrations. See [Milestone 1 results](mail-engine.md) for the receiving engine and expanded native command boundary.
