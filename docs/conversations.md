# Conversation grouping and reading

The conversation scope was expanded on 2026-11-01. Receiving, local storage,
search, and sending continue to use the existing Tauri/Rust/SQLCipher boundary.

Conversations are account-scoped connected components of Message-ID,
In-Reply-To, and References links. IDs preserve local-part case and normalize
ASCII domain case; bracketed and bare IDs are accepted, malformed tokens are
ignored, and multiple IDs in a Message-ID header do not create identity links.
Subjects never create links. Missing ancestors connect replies through placeholder
nodes. Cycles terminate; duplicate stored copies remain separately actionable.
Components are derived from current visible cache rows, excluding locally hidden
messages and obsolete folder epochs. No network access is needed for reading or
grouping. Removed ancestors can remain represented by references in their replies.

The default Conversations view groups **matching** search rows before pagination.
The newest matching message represents each group; its unread/attachment indicators
aggregate the matches. The count beside it is the number of matching stored
messages, not the number of downloaded ancestors; participant names describe the
matching messages. Selecting a group opens an inline conversation reader across
the same account's folders, ordered by date then local row ID. The subject appears
once, and compact sender/date/folder headers surround the open body. Bodies load
individually. Selecting a header opens its message; selecting it again collapses
it. Longer conversations disclose earlier/later history around the current
message. Message actions live in a menu on the open message, with reply controls
after its body. Narrow layouts reduce mailbox chrome to leave room for reading. Missing references get an explicit partial-cache note.
Junk/Trash context is omitted unless enabled, viewing a specific folder, or
searching with `folder:`; search results report this effective scope so the
reader matches the list. Conversation context refreshes with sync, completed
actions and the action queue, like search results. The
Messages selector preserves flat results and individual access. Saved searches
preserve their display choice; older searches default to Conversations in the UI.

Reader actions and reply/reply-all/forward always target the open message;
merely showing other conversation members does not mark them read. Marking an
already-read message unread while it is open now sticks, including via Shift+U. Bulk selection
targets matching rows only, including all matches represented by checked groups,
never out-of-filter context. Existing durable actions, drafts, outbox, formatted
viewer isolation and offline behavior are reused.

Schema 18 adds In-Reply-To and a header-backfill marker. Pre-upgrade cache rows
retain their bodies, row IDs and local state. Normal sync refetches those rows
once, in the existing resumable download batches, updating threading headers
without producing duplicate rows or arrival notifications. Until this completes,
legacy messages without References can remain separate. New mail records headers
on first download. This backfill currently uses the existing message fetch path,
so it also transfers body data even though only headers are updated.

The Rust conversation API is guarded and granted only to the main webview, with
an explicit denial attempt in the hostile-viewer probe. Browser tests use a
synthetic native-IPC fixture; they do not prove native isolation or IMAP transport.

## Verification and limitations

See the ignored `evidence/conversations/evidence.md` and accompanying logs and PNGs
for the first implementation's commands and results.
`evidence/conversation-reader-v2/evidence.md` records the revised inline reader,
interactive browser verification and inspected desktop/narrow screenshots.
Tests cover disconnected reading, incomplete context, flat and grouped search,
message action isolation, restart, account separation, duplicate/malformed IDs,
cycles, pagination, and upgrading a schema-12 cache. Existing composer, reply,
mailbox-action, sending, store and upgrade suites are also exercised.

Grouping currently scans cached threading headers and all matching summaries in
Rust before returning a bounded page. It does not fetch bodies for context, but
its cost grows with cache/match size; the 100,000-message / 200 ms p95 target has
**not** been verified for grouped queries. Conversation member summaries are
returned together; exceptionally large conversations have not been benchmarked.
NixOS Wayland/X11 runtime, live-server backfill and native hostile-viewer denial
remain platform verification gaps on the macOS test machine.
