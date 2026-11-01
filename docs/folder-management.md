# IMAP server folders

Folder management is available from **Manage folders** beneath the selected
account in the sidebar. It uses the account's existing credentials and verified
IMAP transport. The UI calls one guarded main-window command; Rust owns name,
account, hierarchy, pending-work and confirmation checks.

- Create accepts a full server path using its advertised hierarchy separator.
- Rename keeps the same parent and preserves the opaque folder ID, cached
  message IDs and references, and saved searches scoped to that account and
  folder. Sync still checks UIDVALIDITY before applying cached identities to
  server mail.
- Delete requires an explicit confirmation checkbox. It permanently removes the
  selected server folder and its mail, rather than moving it to Trash. Cached
  messages, search entries and retained attachment blobs are removed only after
  the server folder is absent.
- Special folders cannot be renamed or deleted. Rename/delete currently support
  leaf folders only. Fresh LIST includes unselectable containers when checking
  descendants. Manage children first.
- New names currently support 1–200 ASCII characters, excluding `&`, the LIST
  wildcards `*` and `%`, control characters, empty hierarchy components and
  surrounding spaces. Modified UTF-7 authoring is not implemented. Existing
  server names remain discoverable. Case-insensitive duplicate names and INBOX
  are rejected. Name and confirmation checks run before connecting, so they
  read the same offline.

## Confirmation, offline behavior and reconciliation

Folder changes require an online connection. They are not optimistic changes
and are not placed on the offline mail queue. A connection failure before the
command leaves folder and mail caches unchanged. Every command is followed by
LIST, and only that list settles the result; the UI closes only after the
expected server state and local reconciliation are confirmed.

Before sending a command, an encrypted `folder_changes` row records its source
and destination. Account health reports that reconciliation is required. A
server refusal is shown as such once LIST proves nothing changed, and leaves
mail usable. A lost reply counts as done if LIST shows the change. If LIST
cannot be read either, or the process exits, that record stays. The user sees
an error with instructions to sync; restarting does not silently repeat the
command.

Account sync, including the background round, reconciles LIST before applying
mail operations. With only the destination present, a rename updates cache
references transactionally, replacing anything stale cached under the new name.
If both names exist, the source keeps its cache and sync's UIDVALIDITY check
discards it if that folder was recreated. If the old name remains and the new
one is absent, the intent is cleared without repeating it. Missing folders are
pruned according to server truth.

Changes are refused while that account has outstanding message or outgoing-mail
operations. During an unresolved folder change, new account mail actions and
operations are refused and existing operation execution is paused. Other
accounts remain independent. Special Sent/Drafts/Archive/Trash folders are
protected so outgoing copies and default message destinations stay intact.

A saved search for a folder that was deleted keeps its name and finds nothing,
as it would after a removal by another client. Free-text `folder:` terms are
left as typed.

Renames that happen outside Pigeon stay a removed folder plus a new one.

Reconciliation rows describe the current device cache, are excluded from
backups, and cascade away on account removal. Restored mail actions and outgoing
work retain the existing paused-on-restore behavior.

## Large folders

Deleting removes cached mail in batches of 500 before the final transaction, so
other views keep working. The `folder_changes` row stays until that transaction,
so an interrupted delete resumes at the next sync.

Renaming stays one transaction: a batched rename would leave mail under neither
name for a moment, and the reader could take that for a remote deletion. It
touches no search entries, since the index covers only message text
(migration 14). Other views wait for it while the dialog shows it is working.

Longest wait for another view, from the ignored benchmark in
`tests/search_benchmark.rs`. It ran on Linux in Docker on Apple Silicon, with
SQLCipher optimised and the database on tmpfs, so it includes no disk syncs. An
idle read takes about 11 ms at 100,000 messages.

| Change | 10,000 messages | 100,000 messages | 100,000 before |
| --- | --- | --- | --- |
| Rename | 79 ms | 760 ms | 1,550 ms |
| Delete | 6 ms | 28 ms | 1,330 ms |

Folders removed by another client still go in the single transaction sync has
always used.

## Verification

`tests/folder_management.rs` exercises the engine through the existing
MailConnector seam, including cached mail, account boundaries, pending work,
offline failure, confirmation, restart, refusals, lost replies with and without
a readable LIST, stale cached names and both names existing.
`tests/imap_server.rs` includes a disposable Dovecot lifecycle checked by a second
client. The Playwright mail suite covers the UI with a deliberate native IPC
fixture; it does not prove native IMAP or Tauri isolation. Task-specific results
and inspected screenshots are kept in ignored `evidence/folder-management/`.
