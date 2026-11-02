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
{ "closeToBackground": true, "notifications": "senderSubject", "tray": false, "senderImages": false, "remoteImages": false }
```

- Missing keys use the defaults above, and unknown keys are ignored.
- An invalid file logs a warning, applies the defaults, and is never overwritten.
- A symlink (the Home Manager case) or a read-only file is treated as managed. The Settings page shows it read-only and saving is refused.
- `notifications` is acted on (see Notifications). `tray` is stored and acted on in a later slice.
- `senderImages` is acted on (see Sender images). `remoteImages` is acted on (see Remote images).

The two IPC commands `desktop_settings` and `set_desktop_settings` are main-window only. The viewer isolation probe also tries both and must be denied.

## Sender images

Every sender shows initials on a colour taken from their address, with no network involved. Settings → *Sender images* → **Website icons** (`senderImages`) also looks up the icon of the sender's website. It is off by default, because of what it reveals.

- **What leaves the device:** the domains of the senders in the message list you are viewing, sent to DuckDuckGo (`icons.duckduckgo.com`) along with your IP address. Addresses, names, subjects and message text are not sent. Nothing is looked up during sync.
- **What senders learn:** nothing from Pigeon, which never connects to their servers, so a `From` header cannot be used for open tracking or to reach a private address. Subdomains are reduced to the registrable domain (`em.example.co.uk` becomes `example.co.uk`), because a sender can mint one per recipient.
- **What is skipped:** personal mail providers such as `gmail.com`, whose icon would only name the provider, and names that are not public domains (IP addresses, `localhost`, `.local`, `.test`).
- **What is kept:** icons stay in the encrypted store for 30 days, and a domain with no icon is asked about again after 7 days. A failed lookup is not remembered, and an expired icon keeps showing until a new one arrives. Backups leave them out. Choosing **Initials** stops lookups at once but keeps icons already stored; they are removed with the store.
- **What an icon means:** nothing about authenticity. `From` is not verified, so forged mail shows the forged domain's icon.

Icons are at most 256 KiB and must be PNG, JPEG, GIF, WebP or ICO by their contents. Rust fetches them over TLS 1.2 or later, follows no redirects, and hands the interface `data:` URLs, so the main window's content security policy keeps it off the network. `sender_image` is main-window only and returns nothing unless the setting is on.

## Remote images

Remote images are images a message loads from the web, as opposed to embedded (`cid:`) images sent inside it. They are blocked by default, because loading one tells the image's server that the message was opened, when, and from which IP address. Opening the formatted view of a message with blocked images shows a note in the main window saying how many images, from how many sources (hosts), were not loaded:

- **Load all images** reopens the formatted view with every remote image, for that opening only.
- **Always load all from *address*** trusts the sender for every source and reopens it.
- **Choose sources…** lists each blocked host with its image count. Tick some, then **Load selected** (this opening only) or **Always load selected from *address*** (trusts the sender for those hosts alone). Sources already loaded once stay loaded when the view reopens.

Trust is kept in the encrypted store, for every source (`image_trust`, migration 16) or per host (`image_trust_hosts`, migration 17), and included in backups. Trusting every source replaces a sender's host list. Settings → *Trusted senders* shows each sender with *All sources* or their hosts; remove a single host with its ×, or the whole sender with **Remove**.

Settings → *Remote images* → **Always load** (`remoteImages`) loads them in every message.

- **Trust is by `From` address**, which is not verified. Forged mail from a trusted address loads the same images.
- **Only https images load.** The sanitiser drops other image URLs. The viewer's CSP, sent as a header and in the document, is `img-src data:` when blocked, `img-src data: https:` when everything is allowed, and lists exactly the allowed hosts (`img-src data: https://cdn.example.com`) otherwise, so a partly allowed document cannot load from anywhere else, redirects included. Scripts, connections, frames and forms stay blocked, and the viewer sends no referrer.
- **Hosts are parsed strictly.** A source is the lowercased host and optional port of an `https://` URL, made only of ASCII letters, digits, dots and hyphens. Anything else (user info, backslashes, percent-encoding, single-label names) is never loaded, so the host shown, the host compared and the host in the CSP always agree, and nothing else can reach the policy.
- **The decision is made in Rust.** `open_mail_message` reads `remoteImages` itself and re-parses requested hosts. `trust_sender` takes a message id, not an address, and only accepts hosts that message loads from. The viewer webview can call none of these commands; the isolation probe checks.
- Reopening a message closes its earlier viewer window, so loading images replaces the blocked view rather than stacking windows.

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
