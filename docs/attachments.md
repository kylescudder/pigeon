# Attachment support

Outgoing drafts copy files selected in a native chooser. Up to 32 files and
20 MiB of decoded attachment bytes are allowed per draft. Removing an attachment
changes the next autosave. On Unix, import opens with O_NOFOLLOW and O_NONBLOCK and validates the opened
file descriptor, rejecting symlinks, directories and pipes without a stat/open
race. Source paths are never stored; editing or deleting a
source file cannot alter the saved copy. The attachment bytes (base64 in the draft
JSON) live inside SQLCipher along with the existing local draft and outbox data.
No schema migration is needed, and older drafts default to no attachments.

Sending builds multipart MIME with binary-safe transfer encoding, an attachment
Content-Disposition and encoded filenames. Files use application/octet-stream.
The outbox freezes SMTP and Sent-copy MIME, preserving attachment bytes through
restart, undo, submission retries and independent Sent-copy retries. The existing
backup treatment of draft/outbox payloads also includes these bytes, with the
existing backup size limit and pause-on-restore behavior.

Forwarding copies message text only. The reader and composer explain that files
must be attached separately; forwarding never silently downloads attachments.

Received attachments have Save and Save and open actions. Both ask for a native
save destination before downloading, reusing MailEngine::attachment and its
session/encrypted retention caches. Server filenames supply only a sanitized
suggested basename. Export requires a new destination file: existing files and
symlinks are refused. Unix exports are created with mode 0600, and incomplete
writes are removed. Opening happens only after an explicit Save and open action
and successful write, through the OS default application. The interface explains
that export leaves encrypted storage and that only trusted files should be opened.
Pigeon creates no automatic plaintext attachment preview/cache directory.

The two new commands are main-window capabilities and also enforce the existing
Rust main-webview guard. Neither accepts filesystem paths from JavaScript. The
hostile viewer probe includes attempts to invoke both commands.

## Verification

On 2026-11-01, the focused Rust suites passed 46 tests (sending, attachments,
replies, store, upgrade). Sending fixtures now include binary attachments; explicit
unknown retries assert identical submitted MIME. A dedicated test covers draft
and outbox reopen, undo, Unicode filename decoding and exact binary bytes in
SMTP/Sent MIME. Invalid encoding, unsafe names and aggregate size are rejected.
The disposable Linux Dovecot/SMTP run passed 9 IMAP and 6 SMTP tests, including
sending an attachment, fetching it from Sent, retaining it encrypted and reopening.

The production SPA browser suite passed 59 tests with 1 existing skip across
1440×900 dark, 1000×1100 light and 390×844 dark. After the final attachment layout
adjustment, all 6 attachment-specific browser checks passed again. These use the
existing native IPC fixture, covering attach/remove/reload/queue/undo and chooser
failure/cancellation, and explicit save/open requests and errors. TypeScript and
native macOS compilation passed. Real screenshots were saved and inspected.
Evidence, commands and logs are in the ignored evidence/attachments directory;
final screenshots/reports are in evidence/attachments-ui.

Follow-up on 2026-11-04 added six real-filesystem tests for source snapshots,
size/directory rejection, exact export bytes, existing-file protection, private
permissions, symlinks (including dangling links) and nonblocking pipe rejection.
The desktop adapters now use these tested Rust functions. All 52 focused tests
passed, including sending, caches, replies, encrypted storage and upgrade/restore.
Native compilation,
formatting and diff checks passed. Dialogs are parented to the main window, and
external opening refuses lossy conversion of non-UTF-8 destination paths.

The broad macOS core run still fails three IMAP loopback tests with a
connection-closed error, including when run sequentially (40 passed); the full
suite is not claimed green. The interrupted Linux run's final log records
linkers killed with signal 9. Docker subsequently reports “Docker Desktop is
unable to start”, so a fresh Linux run is unavailable. Native chooser interactions,
external opening and the new hostile-viewer probe have not been run on NixOS.
Wayland/X11 and desktop file-association behavior remain runtime gates; the
filesystem tests and browser screenshots do not establish those boundaries.
Follow-up logs and reproduction commands are in evidence/attachments-followup.

## Integration with current main (2026-11-06)

Merged current main while retaining its rich-text editor, saved signatures,
conversation reader and successful-send outbox cleanup. Draft and outbox payloads
preserve attachments alongside body_html and signature. A new integration test
checks multipart/mixed containing both multipart/alternative and exact binary
attachment bytes, with the chosen signature in SMTP and Sent copies. The browser
attachment journey now uses rich text and checks recovery after reload and undo.

After resolving the conflicts, 63 focused Rust tests passed; the final sending
run passed 27 tests including the additional rich-text/attachment test. The merged
composer/reader browser suite passed 125 tests with 1 existing skip, followed by
6 passing attachment-specific capture checks. Native compilation, TypeScript,
formatting and diff checks passed. New desktop and narrow screenshots were
captured from this merged build for PR #25. Native dialog/opening and NixOS runtime
limitations above still apply. Evidence is in evidence/attachments-pr25.
