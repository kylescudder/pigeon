# Milestone 1: receive, persist, and search

Status: implemented for local testing. The receiving engine and browser flows pass the checks below. The user's actual provider and this milestone's NixOS desktop account flow still need verification. Sync itself stays read-only; since Milestone 2, changes you make (read state, moves, deletion) reach the server through the [action queue](#milestone-2-changing-mailbox-state-2026-10-30).

## Try it on NixOS

Quit the existing background instance using **Quit Pigeon**, then:

```sh
git pull --ff-only
nix flake check -L
nix run
```

Choose **Connect an account**, enter its IMAP host, username, and password or app password, and keep TLS on port 993 unless the provider specifies STARTTLS on another port. Advanced settings accept an additional PEM CA certificate for a private mail server; certificate and hostname verification stay enabled. OAuth-only accounts are not supported yet.

The desktop Secret Service must be available and unlocked. The database key and account passwords are kept there. There is no plaintext credential fallback. The session-only backend exists for tests but is not yet exposed as an account-setup option.

After connection, Pigeon downloads headers and text bodies, indexes them inside SQLCipher, and reports the current folder's progress. The estate overview shows account/cache counts and sync errors. Open an account inbox, **All inboxes**, or **All mail** to read and search. Sync runs on connection, on explicit refresh, and through a background coordinator about once a minute after the preceding round finishes. A long account sync can delay that coordinator's other accounts; explicit account syncs can run independently.

Search includes downloaded mail only. Initial sync failures leave committed batches available; retry resumes without fetching those bodies again. Stop sync takes effect between network operations/batches. A blocked network operation must return or time out first. A failed account stays visible alongside its cached mail.

Search supports words, quoted phrases, prefix `*`, full-text `AND`/`OR`/`NOT`, and these filters:

```text
receipt from:alice after:2026-01-01
subject:"quarterly report" account:Personal
folder:Archive is:unread has:attachment
```

Dates use the device's local timezone. Structured filters combine with AND; Boolean operators apply to the full-text expression. Clickable account/folder/unread/attachment filters combine with typed terms. Junk/Trash are indexed but excluded by default unless selected or included. Results are sorted newest first, returned in pages of 50, and retain exact counts. Saved searches survive restart in the encrypted database. Explicit sync refreshes an open cached message too; background reader polling detects remote removal/replacement, and closing the reader discards its query cache. Unsaved search text stays in session memory; URLs contain opaque navigation identifiers, not mail addresses, folder names, or search terms.

Plain text is rendered as text in the main window. **Open formatted view** opens sanitised HTML in a separate unprivileged webview with remote content blocked. Embedded (`cid:`) images are downloaded on opening and shown inline; see [attachments and embedded images](#attachments-and-embedded-images-2026-10-30). Attachment filenames/types are visible; saving or opening attachments is not available yet.

Use **Manage account** on its overview card to verify a replacement password or remove its local cache and keyring credential. Removal leaves server mail untouched. Password verification failure preserves the preceding working credential.

## Engine boundary and persistence

`MailEngine` is the service interface. It owns credentials, connection lifecycle, folder snapshots, cursors, transactions, index maintenance, saved searches, cancellation, and recovery. Tauri exposes narrowly scoped typed commands; the frontend has no SQL or IMAP access. `MailConnector`/`MailSession` are the external-server boundary used for deterministic recovery tests. Production uses verified native TLS and the IMAP adapter.

Network operations run outside the encrypted-store mutex and off the interface thread. A sync snapshots each folder's UIDVALIDITY, UIDs, and flags using EXAMINE. It fetches only missing headers and text MIME sections using PEEK, committing/indexing batches of 100. Sync never sends STORE, MOVE, EXPUNGE, or message deletion commands; only the action queue writes, as described below. External seen/deleted changes are reflected after a successful snapshot. Folder renames are handled as a removed folder plus a new folder, so they require a re-download.

Remote deletions are pruned only after the full folder snapshot is fetched successfully. If UIDVALIDITY changes, the old epoch stays readable until its replacement completes; partially received replacement batches survive restart. Ordinary sync still scans UID/flag metadata; CONDSTORE/QRESYNC/IDLE are not implemented. Concurrent changes that invalidate a snapshot surface as a retryable sync error rather than guessed message identity.

Migrations extend the foundation database without resetting it: account/folder/cursor and saved-search tables; message UID/HTML/attachment metadata; compact covering indexes for search and counts; opaque folder identifiers. Global first-open serialisation prevents diagnostics and background polling from generating different database keys. Existing wrong/missing-key handling remains fail-closed.

FTS5 indexes sender, recipients, subject, and decoded text. Search counts/candidates use compact metadata, and a materialised paginated ID set selects the wide message rows only after limiting the result page. This avoids decrypting full bodies for every match. SQLite's default 2 MB page cache, SQLCipher memory protection, encrypted WAL, and memory-only temporary storage are unchanged. Native searches are cancellable and have a ten-second SQLite execution deadline. That deadline begins after acquiring the store mutex; it is not a total wall-clock guarantee.

TLS requires at least 1.2 and validates certificates/hostnames. STARTTLS must succeed before LOGIN. TCP connection attempts and socket reads/writes have timeouts; DNS resolution and a sequence of many responses do not have a single total deadline. IMAP wire debugging is disabled because it can expose credentials. Raw HTML never crosses into the privileged frontend. Lock/unavailable errors hide mail views and discard frontend message/search query caches; polling also closes decrypted viewer windows. This is not a guarantee of immediate memory erasure on keyring lock.

The adapter currently uses `imap` 2.4 and `imap-proto` 0.10. Rust reports a future-compatibility warning in the latter; this remains a dependency maintenance item. The connector seam allows replacing the adapter without changing the UI/cache interface. Provider compatibility is established by tests against Dovecot, not a claim of universal IMAP support. Non-ASCII mailbox names are not decoded from modified UTF-7 yet.

Text parts above 16 MiB, headers above 256 KiB, excessive nesting, or unsupported/malformed MIME responses stop that sync with an error and retain committed mail. The size checks occur around parsed responses and are not a complete defence against an unbounded malicious server response. Attachment names using uncommon MIME parameter encodings may need further compatibility work.

## Verification on 2026-10-27

Source: milestone working tree based on `2ed50fb`; the delivery commit identifies the exact source. Local raw logs and inspected screenshots live under the ignored `evidence/mail-engine/` directory.

| Check | Observed result |
| --- | --- |
| `cargo test --locked -p pigeon-core` | 23 ordinary tests pass; explicit server/benchmark/Secret Service tests are separate. Wrong-key tests deliberately emit SQLCipher decrypt-failure diagnostics. |
| `tools/mail-test/run.sh` | Six real Dovecot tests pass: verified TLS authentication and folder discovery; STARTTLS; rejection of untrusted certificates and wrong passwords without saving accounts; read-only incremental sync; MIME/Unicode/HTML indexing; remote flags/deletions; a dropped TCP connection with recovery. |
| Attachment transfer | A fixture with a >9 MB attachment required 11,823 encrypted server-to-client bytes during account setup and complete text sync through an opaque counting relay. Attachment payload and script canaries were absent from the index. |
| Recovery service tests | Interrupted 100-of-250-message sync resumes after restart; UIDVALIDITY replacement preserves the old cache until complete; remote deletion/seen state, credential failure, local removal, early search cancellation, keyring lock/unlock, and concurrent key creation all pass. A deliberately stalled external session permits cached search and cancellation. |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check` | Pass. The dependency future-compatibility notice described above is still emitted. |
| `tools/linux-dev/nix.sh flake check -L` | All four checks pass on aarch64-linux: package, frontend, core tests plus both real Secret Service cases, and packaged WebKitGTK self-test. All **26 native diagnostics** pass, including ACL denial of every added mail command. x86_64-linux was not executed by this local builder. |
| `bun run test` | Typecheck plus 21 Chromium tests pass against a production frontend build in dark 1440×900, light 1000×1100, and narrow 390×844 layouts. |
| Browser journeys | Failed/successful account setup, password clearing, literal hostile text, cached reading, filtering, saved search after reload/delete, empty/invalid query recovery, locked-store hiding/unlock, reader refresh after sync, local removal confirmation/reload, navigation, themes/density, and diagnostics feedback. |

The browser suite deliberately substitutes the native IPC boundary with synthetic fixtures; its localStorage fixture persistence is test code only. It proves the delivered UI flows, not actual native keyring/IMAP persistence. Rust server tests prove the real protocol/cache side. Screenshots were captured from the production frontend with those fixtures and inspected at desktop and narrow widths. A narrow Chromium viewport is not native mobile support.

### Search measurements

Reference environment: Apple M5 Pro host; aarch64 Linux Docker VM with 6 vCPUs and approximately 11.7 GiB assigned RAM; Rust release build; SQLCipher encrypted file on Docker storage. Other host/container workloads were present. Repeat on the target NixOS hardware before treating these as its latency figures.

Corpus: 100,000 deterministic synthetic messages, four accounts, eight folders, roughly 700-byte plain-text bodies, alternating senders, unread and attachment metadata. Attachment payloads are not indexed. Every full-text ordinary term matches many messages. Ten query shapes run ten times each, using the public engine, exact counts, and a first page of up to 50 rows:

```text
receipt
migration
"annual report"
from:alice
is:unread
has:attachment
folder:Archive receipt
account:"Mailbox 2" report
project AND quarterly
after:2026-01-01 invoice
```

| Scenario | Result |
| --- | --- |
| Index 100,000 synthetic messages (no network latency) | 17.69 seconds |
| First search on a newly opened SQLCipher connection | 82.74 ms; one sample, OS filesystem caches retained |
| After indexing, 100 queries | p50 80.09 ms; **p95 92.18 ms**; max 98.63 ms |
| Concurrent fifth-account indexing from 100k toward 125k, 20 queries | p50 81.08 ms; p95 96.15 ms; max 97.72 ms |

The automated gate is p95 <200 ms after indexing. The cold measurement resets the connection/SQLite cache, not the host filesystem cache; a physically cold disk/reboot was not measured. The concurrent run schedules a sync beside the queries; it is not a controlled sustained import or fairness benchmark. Large bodies, much larger corpora, complex/pathological queries, and a different disk/CPU need separate measurements.

Reproduce the explicit performance gate in a Linux dev shell:

```sh
cargo test --release --locked -p pigeon-core --test search_benchmark -- --ignored --nocapture
```

`tools/mail-test/run.sh` creates a test-only Dovecot container, private CA, synthetic Maildir, and encrypted-byte counting relay. It exposes no host ports and removes the disposable server/PKI volume on exit. CI runs it as a separate job because sandboxed Nix checks cannot connect to this external fixture. Docker Cargo caches are retained for repeat runs.

## Remaining delivery gates and later work

Run this milestone on the user's x86_64 NixOS desktop: connect the actual provider, observe initial coverage and errors, read/search after quitting and reopening offline, confirm filters/saved searches, open a real formatted message, run the expanded isolation probe, then exercise background reopen and both Wayland/X11 where available. No actual user credentials or private mail were used during development.

SMTP/composing/drafts, mailbox mutations and the durable offline action queue, attachment saving/opening, remote-content controls, backups/recovery export, OAuth, notifications, Home Manager startup configuration, and the five-design picker remain later work in the agreed product plan. This receiving build is ready for testing, not a complete daily-client replacement.

## Partial-sync diagnosis, 2026-10-30

The user reported that Refresh briefly resumes work and then stops with the same generic connection error. The cause of that provider-side failure has not yet been reproduced locally. A separate, confirmed diagnostic defect hid the distinction: established-session IMAP NO/BAD replies and response parsing errors were all translated to `Connection`.

The adapter now reports the operation (folder listing/open, UID/flags, headers/MIME structure, or text download) and a safe failure category: rejection, parsing, timeout, closed connection, or other transport failure. It never formats raw library errors, which can include private server responses or message content. Failed authentication remains distinct; interrupted authentication no longer falsely reports a rejected password. TLS verification, sync batching, and committed-cache recovery are unchanged.

A disposable scripted server regression exercises the actual adapter's LIST response path for NO, BAD, malformed response, timeout, and disconnect. It checks both useful detail and omission of a synthetic private-response canary. This identifies diagnostic loss, not the user's underlying sync failure. The existing real Dovecot and interrupted-cache recovery tests are also rerun. Local evidence is under `evidence/sync-diagnostics/` (ignored).

To collect the next signal, quit the running background instance, pull the update, launch with `nix run`, and Refresh the affected account. Share the new error text, whether its cached count advances between retries, and the provider/server name; omit credentials and message content. No extra wire logging is needed.

## Continue past incompatible message responses, 2026-10-30

The next user screenshot identifies a parser failure specifically during header/MIME-structure fetching. Other accounts have completed sync. The exact response from the affected provider remains unavailable; no personal message data was collected.

A regression reproduces the blocking behaviour at the engine boundary: one unparseable message stops a two-folder, 500-message fixture after 100 cached messages. Recovery now splits a failed UID batch, reconnecting and reselecting the folder after each parser failure. A fresh EXAMINE must confirm the original UIDVALIDITY before recovery can proceed. Readable sub-batches commit normally; a failed singleton is counted as incomplete coverage, and later batches/folders continue. Previously downloaded bodies remain cached across retry/restart. Once the problematic response becomes readable, retry fills the missing message and clears the error.

This is recovery around parser incompatibility, not a claim that all MIME structures are supported. Persistent incompatible messages remain undownloaded, and the account stays in an error state with a missing-message count instead of reporting a successful full sync. An incomplete folder is not finalised: deletion pruning, full flag reconciliation, and promotion of a replacement UIDVALIDITY epoch wait for complete coverage. A broadly incompatible response set stops recovery after 31 fetch attempts for that original batch, preventing unbounded splitting. Ordinary network/authentication failures still stop that sync normally.

A second regression uses actual IMAP response bytes and the real parser over a disposable loopback server: a BODYSTRUCTURE containing multiple extension values triggers the then-current dependency's parser failure. Recovery downloads the surrounding messages and reports one unresolved message. The extension sequence follows the grammar in [RFC 3501 section 9](https://www.rfc-editor.org/rfc/rfc3501#section-9); this synthetic response is an example of a parser limitation, not proof of the affected provider's exact response. No dependency fork, unsafe parser bypass, full-message attachment download, or relaxed TLS check is introduced.

Verification includes 27 ordinary core tests, real Dovecot/TLS recovery tests, the existing browser suite, and packaged Nix/native checks. Detailed commands, results, source revision, and remaining target-machine gaps are recorded in ignored `evidence/partial-sync-recovery/`. After updating, retry the affected account without deleting its existing cache. The expected improvement is that readable messages after the failing batch continue downloading; a remaining incomplete-sync warning is intentional if some messages are still unsupported.

## Zoho parser compatibility, 2026-10-30

The user identified `imappro.zoho.eu` as the affected server, with one message still pending after the readable mail finished. This confirms the provider and persistent failure, not the exact response bytes of that message.

Parser compatibility tests reproduce three unsupported response forms locally: unquoted Content-ID atoms (historically reported in Zoho BODYSTRUCTURE responses), Zoho wildcard FLAGS outside PERMANENTFLAGS, and repeated body-extension fields. The first two have primary-source reports: [Zoho's community bug report](https://help.zoho.com/portal/hi/community/topic/bug-in-imap-server-protocol) and [upstream parser PR #91](https://github.com/djc/imap-proto/pull/91). Repeated body-extension values follow RFC 3501 section 9. These fixtures do not establish which form occurs in the user's one remaining message.

Pigeon now selects a narrowly patched, repository-local copy of `imap-proto` 0.10.2 for both its direct dependency and the transitive `imap` dependency. Its provenance, original archive checksum, licences, exact changes and removal conditions are documented in [the vendor note](../vendor/imap-proto/README.pigeon.md). Nix includes this source explicitly, with no runtime downloads or mutable Cargo-cache edits. The standalone vendor lockfile is for running the unchanged upstream test suite; the root lockfile controls app builds. CI runs both application compatibility tests and the upstream suite.

The actual scripted FETCH regression now requires all three messages to download successfully, including extended MIME metadata and an unquoted Content-ID. Malformed numeric/extension syntax is still rejected. Existing bounded recovery remains available for other unsupported responses. TLS, read-only EXAMINE/PEEK operations, attachment-payload selection, encryption and message-viewer permissions are unchanged. The parser dependency's existing Nom/lifetime warnings remain documented maintenance work.

Verification: 30 application core tests, 39 upstream parser tests, six real Dovecot tests and 21 production-build browser tests pass. Packaged Nix/native results and exact source revision are recorded in ignored `evidence/zoho-parser/`. This is a tested compatibility fix; the original Zoho message has not been fetched by the development environment. Retry it on the target machine after updating; no cache reset or account removal is needed.

## Attachments and embedded images, 2026-10-30

Sync still downloads only headers, MIME structure and text parts. Attachment payloads are fetched one part at a time, only on request:

- **Session-only by default.** A downloaded part is held in memory for the session, up to 256 MiB in total (oldest evicted first), and discarded when the keyring locks or Pigeon quits. Kept parts are stored in the encrypted database (`attachment_blobs`, migration 7), never as plain files. They are removed with their message, so server deletion, UIDVALIDITY replacement and account removal clear them. A part is only stored, or read back, if the row still has the account, folder, UIDVALIDITY and UID it was downloaded for; this is checked in the same SQL statement, because SQLite can reuse a removed message's row id. A stored setting, `keep_attachments` (off by default), makes keeping automatic; its interface arrives with attachment saving.
- **Checked against the cached message.** A part can only be requested if it is listed in that message's cached metadata. Before fetching, the folder is reopened read-only (EXAMINE) and its UIDVALIDITY must match. The part's BODYSTRUCTURE type and size must also match the cache, which provides the transfer encoding. Parts over 64 MiB decoded are refused before download. The fetch uses `BODY.PEEK[part]`, so read state is unchanged and other parts are not transferred. Part numbers are validated as dotted digits before reaching a command.
- **Embedded images.** Opening the formatted view finds the `cid:` images that survive sanitising, then downloads the matching `image/*` parts over one mail connection. Only PNG, JPEG, GIF, WebP and BMP parts are inlined as `data:` URLs, up to 10 MiB per image and 25 MiB per message. The per-message budget is applied before each image is read from the store or downloaded, so an image that cannot fit is never fetched. Other types, SVG, oversized or unresolved images lose their source and show their alt text; no `cid:` reference reaches the viewer. The viewer CSP is unchanged (`img-src data:`), so the document itself still cannot fetch anything. If parts cannot be downloaded (for example offline), the view opens without them, and the reader says how many are missing.

Verification: synthetic fixtures only.

| Check | Observed result |
| --- | --- |
| `cargo test --release -p pigeon-core` | Viewer: inline cid mapping (percent-encoded, bracketed, upper-case `CID:`), unresolved/SVG/HTML/oversized parts dropped, shared document budget, `cid:` in links, `srcset` and `background` removed, hostile fixture still clean. Engine: session-only payload absent from `mail.db`/WAL and re-fetched after restart; kept payload served offline after restart and still absent as plaintext from the files; payload removed with its message and account; changed UIDVALIDITY refuses without fetching; unlisted part numbers rejected; keyring lock discards session payloads; one connection for a message's embedded images; offline opening; keep setting. |
| `tools/mail-test/run.sh` | The real Dovecot adapter decodes a base64 2 MB attachment and the existing 9 MB `report.pdf` byte-exactly. Opening a newsletter inlines its real PNG logo for 3,717 encrypted bytes through the counting relay, without its 2 MB sibling attachment. Both messages remain unread on the server. |
| Native self-test | A synthetic message built through the same sanitiser, document wrapper and CSP is opened in a real viewer webview. An injected check reports that the inline PNG rendered, the remote image and an unresolved `cid:` image have no source, no `cid:` remains, and no network resource entries exist. All existing ACL denials still pass. |
## Durable operations queue, 2026-10-30

Outgoing mail and mailbox changes share one durable queue, `pigeon_core::mail::operations`. Rows sit in the encrypted store (`operations` table, migration 6). Each kind registers one `OperationHandler`. A single background worker in the shell claims due work and runs the handler without holding the store lock. While the keyring is locked, the worker does nothing.

States:
- `pending` waits for its `not_before` time, which is how the undo delay works, and for any parent operation to finish.
- `running`, `done`, `failed` (safe to retry, waits for the user) and `cancelled`.
- `unknown` means the operation may have taken effect on the server. Only an explicit resolution moves it on: run again, mark done or discard. Neither ordinary retry nor a restart retries it.
- `held` is never run automatically. Restored backups can use it to keep outgoing mail paused for review.

A handler calls `begin_irreversible()` just before any step that cannot be safely repeated, such as sending the final end-of-message marker in SMTP. After a crash, any work left `running` is queued again if it had not reached that point, and becomes `unknown` if it had. If a handler finishes while the store is locked, its outcome is kept in memory and written when the store reopens. It is never re-run.

A new kind needs a handler with a stable `kind()` name and a JSON payload. Queue work with `MailEngine::enqueue_operation`, or with `operations::insert` inside your own transaction. Operations have no foreign key to accounts, so removing an account cannot silently drop unsent work.

`cargo test -p pigeon-core --test operations` covers:
- claim and undo races, backoff, held work and parent ordering;
- recovery after a crash on either side of the irreversible point, and panics;
- locked-store pausing, and outcomes recorded after unlock.

## Milestone 2: changing mailbox state, 2026-10-30

Select messages with their checkboxes (Ctrl/Cmd-click toggles a row, Shift-click selects a range, and the list header selects the page), then use **Mark read**, **Mark unread**, **Archive**, **Delete**, **Move to…** or **Delete permanently**. The reader toolbar offers the same for the open message, and opening an unread message marks it read. Shortcuts in the message list: `E` archives, `Delete` or `#` moves to Trash, `Shift+Delete` deletes permanently, `Shift+I` and `Shift+U` mark read and unread, and `X` selects the open message.

**Delete** moves mail to the account's Trash folder (special-use `\Trash`, or one named Trash or Deleted Items). If an account has no Trash, Delete is disabled and only **Delete permanently** is offered; Pigeon never creates server folders. Mail already in Trash is deleted permanently. Permanent deletion always asks for confirmation. Archive uses the account's `\Archive` folder (or one named Archive); `\All` is also treated as an archive, so Gmail-style labels are not yet modelled. Moves between accounts are not supported.

### Durable action queue

A change is queued in the encrypted store before anything else happens, so it survives restarts and works offline. The local view updates immediately: messages being moved or deleted are hidden, and read state changes. Queued work is applied in order when an account syncs, after the queue changes, and by the background coordinator about once a minute, so it resumes after reconnecting. A change counts as done only after the server has been checked; the local view alone is not success. **Pending changes** in the sidebar shows outstanding work with a count, and turns red when something has failed or conflicts. Its page offers **Retry** and **Discard**; discarding a waiting change restores the message's local view.

Mailbox changes use their own tables (`mail_actions`, `mail_action_items`, migration 8), separate from the operations queue above; moving them onto that queue is later work. The tables are general enough for other queued work such as an outbox to reuse: a kind, an optional payload, a state (`queued`, `applying`, `failed`, `conflict`), a write-ahead `attempted` flag, a `step` for multi-command changes, and a safe error message. Completed changes are deleted.

### Safety rules

- **Only targeted writes.** Pigeon sends `UID STORE ±FLAGS.SILENT (\Seen)`, `UID MOVE`, and `UID EXPUNGE` for exactly the selected UIDs. It never sends a plain `EXPUNGE`, which would also remove mail another client has flagged `\Deleted`. Permanent deletion needs UIDPLUS; without it, the change fails visibly and nothing is sent. Moves use MOVE, or UIDPLUS `UID COPY` then `UID EXPUNGE` where MOVE is missing; without either, the move fails.
- **Identity check before writing.** Each change records the folder, its UIDVALIDITY and UIDs. Before writing, Pigeon opens the folder read-write (only the queue does this; sync keeps EXAMINE), confirms the UIDVALIDITY, and fetches which UIDs are still there.
- **Conflicts, never resurrection.** If the folder was reset, removed, or a message vanished before Pigeon acted, the change becomes a conflict. Pigeon never re-appends mail or guesses where it went by Message-ID, and the next sync removes the local copy. A read/unread change for mail that no longer exists is simply dropped, since there is nothing to preserve.
- **Uncertain outcomes.** `attempted` is committed before a write is sent. If the connection drops or the app stops mid-write, the change is re-checked on the next run: flag changes and permanent deletion are idempotent, and a move whose UIDs are gone counts as done. For the copy fallback, an uncertain `UID COPY` becomes a conflict ("retrying could duplicate them"), and only an explicit Retry copies again. A change cut off in `applying` resets to `queued` when the store reopens.
- **Server refusals.** A NO/BAD reply, a missing capability, or a malformed response fails that change (Retry/Discard) rather than blocking sync of the account. Connection and sign-in failures leave it queued.
- **Sync respects pending changes.** While a read/unread change is pending, sync does not overwrite the local read state, and hidden messages stay hidden. Failed, conflicting or discarded changes restore the local view; the next sync then shows the server's state.

Moved mail is removed locally from its source folder and downloaded again by the next sync of the destination, because the `imap` 2.4 adapter does not return COPYUID. That costs a body download per moved message. Relocating cached rows using COPYUID is a later optimisation. A long initial sync holds the account, so changes queued meanwhile apply when it finishes; the local view already reflects them.

### Verification

Source: `feature/mailbox-changes`; the delivery commit identifies the exact source. Logs are in ignored `evidence/mailbox-changes/`.

| Check | Observed result |
| --- | --- |
| `cargo test --release -p pigeon-core` | All ordinary suites pass, including 13 new queue tests against a deterministic multi-folder server model: offline queueing across restart, Trash/Archive/move, no-Trash refusal, permanent delete sparing another client's `\Deleted` mail, missing UIDPLUS, remote deletion conflicts without resurrection, UIDVALIDITY change, connection loss before and after a write, the copy fallback's ambiguity guard, interrupted apply, discard, locked keyring, an unusable server response not blocking sync, and a canary showing queued folder names are absent from the database and WAL bytes. |
| `tools/mail-test/run.sh` | Eight real Dovecot tests pass. Two new ones use a second real IMAP session to observe `\Seen` set and cleared, moves into Archive and Trash, permanent deletion from a folder and from Trash with another client's `\Deleted` message surviving, an offline-queued change applied after restart, and a message deleted by the second client becoming a conflict that is not recreated in any folder. |
| Search benchmark (100,000 messages) | p95 106 ms after indexing (p50 88 ms); 121 ms during concurrent indexing. The search and folder-count indexes now include the hidden flag. |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check` | Pass. |
| `bun run test` | Typecheck plus 29 Chromium tests pass in the three layouts, including multi-select, offline queueing with the pending badge, a conflict with Retry and Discard, confirmed permanent delete, mark-read on open, and the archive shortcut. |
| `nix flake check -L` | All four checks pass on aarch64-linux: package, frontend, core tests with both Secret Service cases, and the packaged WebKitGTK self-test. All **30 native diagnostics** pass; the viewer is denied all four new queue commands by the ACL. x86_64-linux was not executed by this local builder. |

After merging main at `0958b1b` (operations schema 6, attachments schema 7), mailbox actions became migration 8. A frozen copy of main's schema-7 SQL (`tests/fixtures/main-schema-7.sql`) upgrades with a queued operation, a kept attachment and a message intact, and the new action tables and columns usable. On the merged tree: all core suites pass (15 queue tests), 9 real Dovecot tests including attachment downloads, and 41 browser tests. Moving or permanently deleting a message also removes any attachment kept with its source row, as remote deletion already does.

Not yet verified: x86_64 NixOS, and writes against the user's provider (Zoho). Try it on a disposable folder first: move a message, check webmail, then delete it permanently from Trash.
## Plain-text sending, drafts and the outbox, 2026-10-30

This is the first Milestone 3 slice. Replies and forwarding, rich text, attachments, address autocomplete and multiple identities per account come later. Encrypted backup and restore is separate work.

- **Accounts.** An account gains a `sending` section: the SMTP server (TLS or STARTTLS, both always verified), a display name, a plain-text signature, and whether to save a Sent copy. It is stored with the account settings, so accounts added earlier simply have no SMTP server until it is added. Pigeon signs in to SMTP with the account's saved password. Saving new SMTP settings checks the sign-in first.
- **Drafts.** Drafts are stored in the encrypted store (`drafts` table, migration 9). The composer saves 0.8 s after typing stops, and again when it is left. Saves are made one at a time.
- **Sending.** Send moves the draft into the `outbox` table in the same transaction that queues an `smtp_submit` operation, delayed by the undo time (default 10 s, 0–60). The message bytes are built once, at that point. Undo, or Edit after a failure, cancels the operation and restores the draft in one transaction. Once sending has started, undo is refused.
- **SMTP outcomes** come from a small client (`mail/smtp.rs`):
  - Failures before the end-of-data marker are either retried with backoff (offline, or a 4xx reply) or failed (5xx, refused recipient, sign-in or TLS failure).
  - The operation records its point of no return just before the marker is written.
  - A lost connection or missing reply after the marker is **unknown**. Unknown is never retried automatically, including after a restart. The Outbox explains it and offers "It was sent", "Send again…" (with a confirmation) and "Discard".
  - Server reply text is never shown; only reply codes are.
- **Sent copy.** Once a message is accepted, a separate `sent_copy` operation appends it, marked as read and including Bcc, to the folder IMAP reports as `\Sent`. It retries with backoff and checks the Message-ID first, so a lost reply does not leave two copies. Its failure is shown and can be retried on its own; it never submits the message again.
- **Account removal** is refused while the account has messages in the outbox that are not settled.

### Verification

| Check | Result |
| --- | --- |
| `cargo test --release -p pigeon-core` | Passes, including 7 SMTP unit tests on scripted servers, 13 sending-engine tests and 13 operations-queue tests |
| `tools/mail-test/run.sh` | 6 IMAP and 6 SMTP tests pass against Dovecot plus an aiosmtpd submission server on 465 (TLS) and 587 (STARTTLS) |
| `bun run typecheck`, Playwright | 38 browser tests pass in dark, light and narrow layouts, 12 of them for compose, outbox and settings |
| `nix flake check -L` | aarch64-linux only (x86_64 cannot be checked from the macOS dev machine); results in the PR |

The SMTP unit tests cover dot-stuffing and CRLF normalisation, recipient refusal without echoing server text, 4xx and 5xx after data, a dropped or stalled final reply, a failed commit hook sending no marker, rejected credentials, and STARTTLS being required with pre-encryption data refused.

The real-server tests cover:
- a STARTTLS send delivered exactly once, with the Sent copy read back by sync;
- every fault mode mapped to the right outcome with no duplicate delivery;
- an unknown outcome surviving a restart without being resent;
- a Sent copy failing while the Sent folder is missing, then saved on retry without resubmitting;
- an offline send waiting, then delivering after reconnecting;
- undo delivering nothing.

Evidence is in `evidence/composer-sending/` (ignored). It used synthetic mail and credentials only, and no real mail was sent.

Still to check on the user's NixOS desktop: send through the real provider, check that the Sent copy appears in webmail, and decide whether that provider files sent mail itself (if so, turn off "Save a copy").

