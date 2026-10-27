# Pigeon product and delivery plan

Status: confirmed by the user on 2026-10-23. Scope, architecture, acceptance gates, and service-first delivery order are agreed. Milestone 0 native diagnostics are user-verified. Milestone 1 receiving/search is implemented for local testing; see [mail-engine results](mail-engine.md) and [foundation results](foundation-spike.md) for checks and remaining desktop gates.

Planning baseline: `7c2f0242dd29ae229a65bcc6e6e939175df7e900`.

## Purpose and priorities

Build a modern, reliable personal email client for the user's NixOS machine. Its distinguishing capabilities are fast local search, useful filtering, and straightforward organisation. A maintained Nix flake is an essential deliverable.

The latest priority decision is **working mail infrastructure first**. Build and prove the local mail engine before investing in alternative interface designs. An ordinary functional interface is enough to exercise the engine during development.

The application connects directly to the user's IMAP and SMTP servers. There is no hosted Pigeon mail-ingestion service. The initial audience is the user; broader public distribution is not a first-release requirement. No delivery date or fixed effort budget has been agreed. Estimate milestones after the foundation spike.

## Agreed desktop architecture

- TanStack Start in SPA mode, React, and TypeScript for the interface.
- TanStack Router for navigation and TanStack Query for asynchronous data and mutations.
- Bun for frontend package management, scripts, builds, and tests. Commit `bun.lock`, preserve dependency version ranges, and use a consistent Bun version with frozen-lockfile installs in CI/builds.
- Tauri as the provisional desktop shell. A Rust backend owns mail connections, synchronisation, database access, credentials, queued operations, and desktop integration.
- Embedded SQLite with FTS5 for search, using SQLCipher to encrypt the local store. The Rust layer owns database migrations and writes.
- Typed, narrowly scoped native commands connect the interface to the backend. Database keys and stored credentials stay out of frontend state and bundles; an entered account password is held briefly in the setup form and passed once to Rust, then cleared.

TanStack Start produces the static frontend; runtime mail operations use native commands. Start server functions do not run inside a static Tauri frontend without a separately packaged server runtime. The foundation milestone must prove static assets, route navigation, reloads, and desktop launch work together. Bun is development/build tooling in this design; a Bun server process is not part of the agreed runtime.

Tauri remains provisional until Linux rendering, hostile-content isolation, packaging, and background operation pass the foundation checks. A failed security boundary requires reconsidering the viewer or shell before expanding the app.

## Mail and account behaviour

- Multiple IMAP/SMTP accounts, initially using credentials or app passwords. OAuth is required only where an actual supported account requires it; password-only support must not be presented as universal provider compatibility.
- Validate TLS certificates and protect authentication in transit. Connection and authentication failures must offer clear recovery without silently weakening security.
- Synchronise folder structure, read/unread state, moves, archiving, and deletions with the server. Other clients and webmail must observe completed changes.
- Folders and saved searches are the organisation model. No star-based workflow or custom tags.
- Support bulk move, archive, delete, and mark-read/unread actions. Delete normally moves to Trash; permanent deletion is explicit.
- Show pending work and failures. A queued change is not successful merely because the local interface updated.
- Reconcile offline actions with server changes. Flag conflicts that cannot be safely applied; do not silently resurrect remotely deleted mail. Preserve recoverable local work.
- Handle reconnects, interrupted initial sync, restarts, folder changes, and server capability differences. Incremental synchronisation is resumable and must not require a full re-download during ordinary use.

## Local data, offline operation, and search

- Download message headers and full bodies across accounts and folders, including Sent and archives. Expose initial-sync/index progress and failures. Search coverage is partial until the relevant mail has been downloaded and indexed.
- Index Junk and Trash, but exclude them from ordinary results unless the user includes them.
- Download attachments when requested, with an option to retain them locally. Downloaded attachments initially live inside encrypted storage.
- Reading, searching, composing, moving, archiving, deleting, and marking read/unread work offline. Persist pending operations across restarts and apply them after reconnection.
- Search sender, recipients, subject, body, dates, account, folder, read/unread status, and attachment presence. Offer clickable filters, keyboard-friendly query syntax, and saved searches across accounts.
- Use FTS5 for message text and appropriate ordinary indexes for structured filters. Keep indexing, network work, and long searches off the interface's critical path. Provide cancellation and bounded/paginated result retrieval.
- Benchmark a representative corpus of at least 100,000 messages across several accounts. The agreed target is a first result page within **200 ms at the 95th percentile for a documented representative set of ordinary indexed queries after indexing is complete**.
- Record reference hardware, query mix, corpus characteristics, cache state, and concurrent sync activity. Include warm/cold measurements and searches during indexing; do not claim every possible query meets the ordinary-query target.
- Search speed and interface responsiveness are measured acceptance criteria, not capabilities assumed from choosing SQLite.

## Composing, drafts, and sending

- Reply, reply-all, forward, CC/BCC, attachments, per-account signatures, history-based address autocomplete, basic rich text, and a plain-text option.
- Replies select the receiving account's identity. New messages clearly expose the sending account.
- Autosave drafts locally and recover them after restart or interruption. Local drafts are not promised to synchronise to server Drafts in the first release.
- An explicit Send while offline queues the message in a visible persistent outbox. Send after reconnection when unlocked, accounting for the configured undo-send delay.
- Include a short configurable undo-send delay, implemented before transmission rather than described as recall after delivery.
- Distinguish queued, sending, accepted, failed, and unknown sending outcomes. Preserve messages when recovery is necessary.
- If connection loss makes SMTP acceptance uncertain, show an unknown outcome and require an explicit decision before retrying. Do not blindly retry an ambiguous submission or promise that Message-ID prevents duplicates.
- Treat SMTP acceptance and saving the Sent copy as separate outcomes. A failed Sent-folder update must not trigger another SMTP submission.

## Security and recovery

- Store credentials and the generated database key through Linux Secret Service. Missing or locked keyrings have explicit states; never silently store secrets as plaintext. Session-only credentials are available where secure credential persistence is unavailable.
- Unlock the encrypted mail store through the desktop keyring. Pause work requiring that store, including mail sync/sending, while it is locked. Reconcile and resume visibly after unlock.
- Encrypt headers, bodies, FTS indexes, local drafts, queued actions, and outbox data. Keep retained attachments protected inside the encrypted store initially.
- Verify actual SQLCipher linkage, FTS5 support, database readability only with the right key, encrypted journal/WAL contents, and memory-only SQLite temporary storage. Prevent accidental plaintext application caches, debug logs, and preview files.
- Provide explicit encrypted backup and restore for local-only data and required recovery material. Use maintained cryptographic components and a documented recovery format. Do not keep an unprotected recovery key next to the encrypted data.
- Verify backup consistency while the application is active. Restored outgoing mail remains paused for review so restoring a backup cannot silently resend messages.
- Explain the loss model: server mail can be downloaded again; losing the database key and recovery material loses local-only data. Full-disk protection remains complementary. Encryption does not promise confidentiality against malware or root accessing an unlocked session.
- Display sanitised HTML email inside a proven unprivileged viewer, with scripts disabled, restrictive network behaviour, and no native command access. Provide a plain-text view.
- Block remote content by default. Offer per-message image loading, optional trusted-sender exceptions, and a global preference. Embedded images can display without fetching an external image resource. Explicit remote loading can still disclose activity to that resource's operator.
- Open links in the user's normal browser. Attachments are never executed automatically; export or external opening creates an explicit boundary outside the encrypted store.
- Keep credentials, keys, personal mail, and sensitive content out of logs, test evidence, Nix expressions, and the Nix store.

**Critical Linux viewer caveat:** Tauri documents that Linux cannot distinguish iframe capability requests from those of the enclosing window. A message iframe inside a privileged application window is therefore not sufficient evidence of native API isolation. The foundation spike must test a separately unprivileged viewer/context and deny hostile mail native capabilities before the app handles real mail content.

## Navigation and desktop behaviour

- Top level: an estate overview showing account unread counts, connection/sync health, pending outgoing work, and saved searches.
- From the overview, support both merged mail views and individual-account views. The estate overview is distinct from the merged inbox.
- A practical three-pane interface with adjustable density, light/dark themes, keyboard navigation, and a command palette remains the functional baseline.
- The visual direction is a compact Raycast/iOS-inspired desktop interface: layered surfaces, restrained rounding, grouped controls, and clear typography. The foundation shell now includes a composed empty overview, persistent theme/density settings, keyboard command navigation, and a secondary Diagnostics view. The receiving build adds real account setup, estate/account views, cached reading, and search.
- Message grouping and conversation threading are deferred; use a flat message list initially.
- Closing the window keeps the application running and synchronising by default. Make this configurable and provide an explicit Quit action and optional launch-at-login.
- Desktop notifications omit message bodies by default.
- Reopening through the desktop launcher must work with the background application. Support operation without a tray; tray availability must not determine whether the app can be reopened or quit.

## Five-design evaluation, deferred

The user requested five genuinely different interactive style/layout alternatives with an easy dropdown picker. Keep this requirement, but schedule it after the mail engine is working reliably. The user explicitly deprioritised an early synthetic-mailbox design prototype.

The eventual picker should preserve account, search, selection, and draft state so comparisons are fair. Each alternative covers estate overview, merged/account views, search, reader, and composer; light/dark mode is independent of the alternative. Choose or combine the strongest design after trialling them. Exact visual directions and the long-term lifetime of the picker remain decisions for that later design milestone, not blockers for service implementation.

## Nix delivery and maintenance

- First supported target: `x86_64-linux` on NixOS. Wayland is primary; check X11 too. Other architectures and operating systems are deferred.
- Provide the default package, `nix run`, `nix develop`, and meaningful `nix flake check` checks.
- Provide a Home Manager module for non-secret configuration and startup behaviour. Integrate runtime secrets through the keyring; never embed them in generated store paths.
- Commit flake, Cargo, and Bun locks. Use a consistent toolchain, frozen frontend dependency installs, and sandboxed Nix builds with declared dependency sources.
- Linux CI builds the package and runs appropriate tests/checks. Validate actual NixOS runtime behaviour in addition to a successful build.
- Cover GTK/WebKit integration, display scaling, notifications, keyring availability, launcher reopening, and tray/no-tray behaviour on supported desktop environments.
- App updates follow the Nix configuration. Do not introduce a separate in-app updater. Maintain the flake and native/web dependencies through ordinary reviewed updates and repeat relevant checks.

## Delivery sequence and gates

### Milestone 0: prove the foundation

Create the Bun/TanStack SPA, minimal Tauri shell, Rust core, encrypted SQLite connection, and flake. Establish Linux build/test automation and the test mail-server environment.

Gate: real NixOS launch; route/asset handling; SQLCipher plus FTS5; keyring locked/missing/unlocked states; HTML-viewer native-command denial and remote-content policy; background/reopen feasibility. Stop and revisit architecture if a boundary fails. A native Linux security result is required; a browser-only fixture does not prove Tauri IPC isolation.

### Milestone 1: receive, persist, and search

Implement account setup, inbound sync, restart recovery, encrypted local persistence, offline reading, indexed search/filtering, saved searches, and observable sync/index health. Use a minimal interface or engineering tools as needed to exercise the engine.

Gate: a real test IMAP server and deterministic mailbox fixtures; interruption/reconnection/restart tests; server changes observed; partial-index states clear; 100,000-message search benchmark with documented results and no interface stalls.

### Milestone 2: change mailbox state safely

Implement bulk organisation, the durable offline action queue, reconciliation, conflict visibility, and Trash/permanent-delete semantics.

Gate: assertions against server state and a second client/session; disconnects and crashes around operations; queued actions surviving restart; remote deletion/move conflicts; no unintended deletion of other-client work.

### Milestone 3: drafts, sending, and recovery

Implement the composer, local autosave, attachments, identities/signatures, persistent outbox, undo delay, SMTP outcome handling, Sent-copy handling, and encrypted backup/restore.

Gate: a real test SMTP service, controlled failures around server acceptance, explicit unknown outcomes, independent Sent-copy retries, offline queued sends, draft recovery, and restore tests that cannot trigger automatic duplicate sending.

### Milestone 4: usable daily client

Complete the functional estate overview, merged/account navigation, reader/composer flows, keyboard controls, desktop notifications, background lifecycle, and Home Manager integration.

Gate: actual end-to-end workflows, persistence assertions, screenshots from the delivered build, and NixOS Wayland/X11 integration checks. Validate the user's actual mail-server compatibility before declaring it a replacement for their current client. Retain the existing client until the acceptance gates pass.

### Milestone 5: trial and select the interface

Build the five interactive variants and dropdown picker on top of proven mail behaviour. Review them through the same real workflows and settle the production design after hands-on use.

Gate: state preserved while switching; accessible keyboard/focus behaviour; actual screenshots and interactive verification of each variant. Evaluate layout at relevant desktop/window sizes. Do not present generated mockups as evidence of running behaviour.

## Explicitly deferred scope

Automatic filing rules, automatic categorisation, conversation grouping, attachment-content indexing, full calendar/contacts management, AI features, scheduled sending, mail merge, templates, cross-device preference sync, other operating systems/architectures, and end-to-end message encryption are outside the initial release. Local storage encryption does not imply end-to-end email encryption.

## Verification and evidence policy

Use a real disposable IMAP/SMTP test environment with synthetic messages for repeatable tests. Cover successful operations and important failure/recovery paths, including authentication failure, server disconnects, wrong or missing keys, partially indexed mail, concurrent server changes, uncertain SMTP outcomes, malformed/hostile HTML, and backup restoration.

Follow the e2e-testing skill for changed interface journeys and tdd when working test-first or adding integration tests. Use web-browser for interactive browser checks and pr-screenshots for saved, inspected UI evidence. Native Tauri boundaries and Linux desktop integrations additionally require native checks.

Store evidence in task-specific ignored or temporary directories and record source revision, scenario, environment, commands, and observed results. Keep fixtures free of personal mail and secrets. A build alone is not evidence of correct synchronisation or sending. Report unavailable platform checks as gaps.

## Research basis and remaining implementation uncertainty

Read-only inspection found only `README.md` and `assets/icon.svg` at the planning baseline, with a clean worktree and no existing app, flake, or repository-local AGENTS file. This section records the original planning research; implementation progress is tracked in [foundation results](foundation-spike.md).

Primary references used for the provisional design:

- [TanStack Start SPA mode](https://tanstack.com/start/latest/docs/framework/react/guide/spa-mode) and [server functions](https://tanstack.com/start/latest/docs/framework/react/guide/server-functions).
- [Tauri frontend configuration](https://v2.tauri.app/start/frontend/), [Linux capability limitations](https://tauri.app/security/capabilities/), and [system tray](https://v2.tauri.app/learn/system-tray/).
- [Nixpkgs Tauri packaging](https://nixos.org/manual/nixpkgs/unstable/#sec-tauri) and [NixOS Tauri integration](https://wiki.nixos.org/wiki/Tauri).
- [SQLite FTS5](https://www.sqlite.org/fts5.html).
- [SQLCipher design](https://www.zetetic.net/sqlcipher/design/), [key material](https://www.zetetic.net/sqlcipher/database-key-material/), and [API](https://www.zetetic.net/sqlcipher/sqlcipher-api/).
- [Secret Service specification](https://specifications.freedesktop.org/secret-service/latest/) and [security limits](https://specifications.freedesktop.org/secret-service/latest/ch10.html).
- [SMTP duplicate-delivery ambiguity, RFC 5321 section 4.5.3.2.6](https://www.rfc-editor.org/rfc/rfc5321.html#section-4.5.3.2.6).
- [Remote content privacy](https://support.mozilla.org/en-US/kb/remote-content-in-messages).

Specific protocol libraries, backup format, and further module interfaces will be selected and tested as implementation progresses. See [foundation results](foundation-spike.md) for the implementation versions, checks already exercised, and remaining gaps. The [mail-engine results](mail-engine.md) record search measurements, Dovecot compatibility, recovery checks, and the remaining target-desktop/provider verification.
