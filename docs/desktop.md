# Desktop integration (Milestone 4)

Status: background lifecycle, configurable close behaviour, `--background`, and the Home Manager module are implemented and checked in Linux containers (aarch64). Behaviour on the supported x86_64 NixOS desktop has **not been observed yet**. New-mail notifications are implemented (see below) and checked only through unit and engine tests; the popup and its coo have **not been seen or heard on a real desktop**. The optional tray comes in a later slice.

## Behaviour

- **Closing the window** hides it and keeps syncing by default. With `closeToBackground = false` it exits instead. The value is read when the window closes, so a Home Manager switch applies without a restart.
- **Quit Pigeon** in the sidebar always exits.
- **Reopening.** Starting Pigeon while it runs, from the launcher or `pigeon`, shows the existing window through the single-instance plugin. A second start with `--background` stays hidden, so a login start never pops up a window that is already running.
- **`pigeon --background`** starts with no window. The main window is created hidden and shown only for a normal start, so no window flashes up at login.
- **No tray required.** Reopening and quitting never depend on a tray.

## Settings file

`$XDG_CONFIG_HOME/pigeon/settings.json` holds only non-secret preferences:

```json
{ "closeToBackground": true, "notifications": "senderSubject", "tray": false }
```

- Missing keys use the defaults above, and unknown keys are ignored.
- An invalid file logs a warning, applies the defaults, and is never overwritten.
- A symlink (the Home Manager case) or a read-only file is treated as managed. The Settings page shows it read-only and saving is refused.
- `notifications` is acted on (see Notifications). `tray` is stored and acted on in a later slice.

The two IPC commands `desktop_settings` and `set_desktop_settings` are main-window only. The viewer isolation probe also tries both and must be denied.

## Home Manager module

`homeManagerModules.default` (also `homeModules.default`) provides `programs.pigeon`:

- `enable` installs the package.
- `package`
- `settings`: typed options written as the file above. `null` leaves the file to the app.
- `autostart.enable` and `autostart.target`: a systemd user service `pigeon.service` that runs `pigeon --background`, uses `Restart=on-failure` so a deliberate Quit stays quit, and is wanted by `graphical-session.target` by default.

There are no options for credentials. `checks.<system>.home-manager` evaluates the module in a real Home Manager configuration and asserts:

- the generated settings JSON and unit lines
- that `settings = null` and the autostart default produce no files
- that no generated file mentions passwords, secrets or tokens

See the README for the snippet and the Hyprland start hook (Hyprland without UWSM never reaches `graphical-session.target`).

## CI

The `nix` job runs `nix flake check -L` on `ubuntu-latest` (x86_64). It builds and tests the `x86_64-linux` package, core and keyring tests, frontend, native self-test and Home Manager check. Local runs here are aarch64 only.

## Checklist for the x86_64 NixOS machine

Record the revision (`git rev-parse HEAD`), the session (`echo $XDG_CURRENT_DESKTOP $XDG_SESSION_TYPE`) and the results.

1. `nix flake check -L` passes on the machine.
2. Enable `programs.pigeon` with `autostart.enable = true`, add the Hyprland hook from the README, rebuild, and log in to Hyprland. `systemctl --user status pigeon` shows it running and no window appears.
3. Launch Pigeon from rofi. The window appears and `pgrep -a pigeon` lists one process.
4. Close the window. The process is still running and the account keeps syncing (the last-synced time advances). Launch again from rofi and the same window returns.
5. Choose **Quit Pigeon**. The process exits and `systemctl --user status pigeon` shows it inactive, not restarting.
6. Set `programs.pigeon.settings.closeToBackground = false` and rebuild. Settings → *When the window closes* shows **Quit** selected, disabled, and "Managed by Home Manager". Closing the window now exits.
7. Set `settings = null`, rebuild, and change the option in the app. `~/.config/pigeon/settings.json` is now a normal file with the new value.
8. Quit first, because a running instance swallows new starts. Then repeat steps 3–5 under X11 (`GDK_BACKEND=x11 pigeon`, XWayland) and in the GNOME session. In GNOME the service should start at login with no Hyprland hook.

## Notifications

The background refresh (every minute, with the window hidden or open) announces unread mail that newly arrived in an Inbox as one notification through the desktop's notification service (D-Bus `org.freedesktop.Notifications`).

- `senderSubject` shows the sender's name and subject for one message, or a count with up to three "sender: subject" lines. `count` shows only "N new messages". `off` shows nothing. The text never includes message bodies or previews, and sender and subject are escaped and length-limited because notification servers render a small markup subset.
- Nothing is shown while the main window is visible and focused, for an account's first sync (its backlog), for mail already read, or for mail outside the Inbox (Junk, Trash, Archive).
- The setting is read on every refresh, so a Home Manager switch applies without a restart.
- Each notification plays a pigeon coo once, however many messages it announces, so `off` and the cases above stay silent. There is no separate sound setting.
- A missing or failing notification service or sound device is ignored.
- Clicking a notification does not open Pigeon yet.

Checked: the wording rules (`crates/pigeon-core/src/notify.rs` tests) and which messages count as new (`tests/notify.rs`). Not checked: the popup or the coo on a real desktop, and behaviour with the keyring locked.
