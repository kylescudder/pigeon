# Backup format and restore contract

Status: first slice, 2026-10-30. Manual backups of local-only data, protected by a passphrase. Implemented in `crates/pigeon-core/src/backup.rs` (format and table registry) and `crates/pigeon-core/src/mail/recovery.rs` (engine backup and restore).

## What a backup holds

| Included | Not included |
| --- | --- |
| Account settings (`mail_accounts`) | Cached messages and the search index; restored accounts download them again |
| Account passwords from the keyring | The folder list, which comes back with the next sync |
| Saved searches | The store's database key; a restored store gets this device's key, or a new one |
| Drafts, outbox, operations, pending mailbox actions and local settings | Sync progress, which is reset so each account starts again |

Because a backup contains account passwords, it is as sensitive as the keyring. It is not a copy of the encrypted database, so restoring needs only the backup file and its passphrase, not the old keyring.

## File format

A backup is a standard [age v1](https://age-encryption.org/v1) file, conventionally named `pigeon-YYYYMMDD-HHMM.pigeon-backup`. Pigeon uses the maintained `age` crate (rage).

- **Recipient.** There is exactly one `scrypt` stanza (age's passphrase mode), with work factor `log2(N) = 18`, using about 256 MiB of memory. The factor is fixed rather than calibrated so that memory use is the same on every device. Pigeon refuses backup passphrases shorter than 12 characters.
- **Integrity.** age authenticates its header and every 64 KiB payload chunk, including the end of the stream. A wrong passphrase, any altered byte, or a truncated file fails before any data is used.
- **Plaintext.** The plaintext is a UTF-8 JSON document. It is streamed straight into and out of age, so it never touches disk.

```json
{
  "format": "pigeon-backup",
  "version": 1,
  "created_at": 1793005200,
  "schema_version": 10,
  "tables": {
    "mail_accounts": { "columns": ["id", "config", "..."], "rows": [["3f…", "{…}", "..."]] },
    "saved_searches": { "columns": ["id", "name", "query"], "rows": [] }
  },
  "secrets": { "account:3f…:password": "<hex bytes>" }
}
```

- `schema_version` is the store's `PRAGMA user_version` when the backup was taken. A restore loads rows into a store migrated to exactly that version, then runs the remaining migrations, so backups from older versions of Pigeon restore normally. Backups with a newer `schema_version` or `version` are refused.
- Cell values are JSON `null`, numbers, and strings. Blobs are written as `{"hex": "…"}`.
- Only `account:<id>:password` secrets for an account present in the same backup are accepted. Any other name marks the file as damaged, so a crafted file cannot overwrite other keyring items, including the database key.
- Tables not marked `Backed` in the registry are rejected.

To inspect a backup outside Pigeon, use `age -d pigeon-….pigeon-backup` with the passphrase. Treat the output as secret, because it contains passwords.

## Taking a backup while Pigeon runs

The backup opens its own connection to the store and reads every table inside one read transaction. With SQLite's WAL, that is a consistent snapshot even while sync writes. The engine's main connection is not held, so reading, searching, and syncing carry on. Account passwords are then read from the keyring for the accounts in that snapshot. An account with no saved password is backed up without one and asks for it after a restore.

The file is written as `.<name>.partial-<random>` beside the destination with mode 0600, synced, and renamed into place. A failure leaves no partial file behind.

## Restore

1. A restore is refused while any account is syncing, being added, having its settings or password changed, being removed, or while an outgoing operation is active. Accepted outcomes awaiting storage recovery also block restore. While it runs, all of those are refused with "a backup is being restored", so no account change can land in the store that is about to be set aside.
2. The backup is decrypted and validated in memory before anything changes.
3. A complete new store is built as `.mail.db.restore-<random>`. It is encrypted with this device's existing key, or with a newly generated key if the keyring has lost it.
4. Account passwords are written to the keyring.
5. Under the store lock, the live store is closed and moved to `mail.db.before-restore-<unix time>`, never deleted. If needed, the new key is saved, and the new store is renamed into place.

Before step 4 writes anything, the keyring's current value (or absence) for each password in the backup is recorded. If any later step fails (a keyring write, moving the live store aside, saving a new key, or the final rename), the store files are moved back, a newly saved key is removed, and every password is put back to its recorded value or deleted. A failed restore therefore leaves the live accounts able to sync as before.

The replaced store stays encrypted under its original key. Delete it from Pigeon's data folder once the restore looks right.

## Contract for outgoing work

Restoring must never send mail or apply server actions without review. Every store table is listed in `backup::TABLES` with a role:

- `Backed { on_restore }`: included in backups. The `on_restore` statements run after a restore, once the store is fully migrated.
- `Redownloaded`: server data, fetched again after a restore.
- `Device`: describes this device's store and is never backed up.

The integrated schema is 10. Operations remains migration 6, attachments 7, mailbox actions 8 and drafts/outbox 9; `store_meta` is appended as migration 10.

On restore:
- Drafts and settings are preserved. Drafts never run automatically; Send is always explicit.
- Every nonterminal operation becomes `held`, including unknown outcomes and pending Sent-copy work. Completed and cancelled operations retain their terminal states, so accepted sends cannot be resubmitted automatically.
- The Outbox offers an explicit reviewed decision for held messages. Sending again requires the duplicate-risk confirmation; saving a held Sent copy is a separate explicit action and never resubmits the message.
- Mailbox actions and their UID records are preserved in `conflict` state with a restore explanation. Only an explicit Retry releases them; normal sync cannot apply them.
- Cached messages, folders, search indexes and attachment blobs are downloaded again rather than copied into a backup. The session attachment cache is cleared on a successful restore.
- `store_meta.restored_at` records the restore time for provenance; persisted queue states enforce the pause across restarts.

The registry test rejects any unclassified table, and the outgoing-work rule requires restore handling for execution tables. The real engine regression backs up and restores drafts, a queued send and a mailbox action, runs sync and the operation worker, restarts, and verifies no transmission. It then releases one send explicitly and separately restores an accepted send with a pending Sent copy, verifying copying never sends again. Failure-injection tests cover credential and filesystem rollback, and a blocked-handler test verifies restore cannot replace the store while server work is in flight.

## Loss model

- Mail on the server can always be downloaded again.
- Local-only data survives losing the keyring only if a backup exists and its passphrase is known. Pigeon cannot reset or recover the passphrase.
- Losing the database key with no usable backup loses local-only data. Pigeon reports this as "key missing" and points to restore. It never generates a replacement key over an existing store.
- Store encryption and backups do not protect against malware or root acting in an unlocked session. Full-disk encryption remains complementary.
