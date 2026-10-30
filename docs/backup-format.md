# Backup format and restore contract

Status: first slice, 2026-10-30. Manual backups of local-only data, protected by a passphrase. Implemented in `crates/pigeon-core/src/backup.rs` (format and table registry) and `crates/pigeon-core/src/mail/recovery.rs` (engine backup and restore).

## What a backup holds

| Included | Not included |
| --- | --- |
| Account settings (`mail_accounts`) | Cached messages and the search index; restored accounts download them again |
| Account passwords from the keyring | The folder list, which comes back with the next sync |
| Saved searches | The store's database key; a restored store gets this device's key, or a new one |
| Later: drafts, outbox, queued actions (see the contract below) | Sync progress, which is reset so each account starts again |

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
  "schema_version": 6,
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

1. A restore is refused while any account is syncing or being removed. While it runs, new syncs and removals are refused too.
2. The backup is decrypted and validated in memory before anything changes.
3. A complete new store is built as `.mail.db.restore-<random>`. It is encrypted with this device's existing key, or with a newly generated key if the keyring has lost it.
4. Account passwords are written to the keyring.
5. Under the store lock, the live store is closed and moved to `mail.db.before-restore-<unix time>`, never deleted. If needed, the new key is saved, and the new store is renamed into place. If any of these steps fails, they are undone.

The replaced store stays encrypted under its original key. Delete it from Pigeon's data folder once the restore looks right.

## Contract for outgoing work

Restoring must never send mail or apply server actions without review. Every store table is listed in `backup::TABLES` with a role:

- `Backed { on_restore }`: included in backups. The `on_restore` statements run after a restore, once the store is fully migrated.
- `Redownloaded`: server data, fetched again after a restore.
- `Device`: describes this device's store and is never backed up.

The tests enforce this:

- `every_store_table_declares_whether_it_is_backed_up` fails when a migration adds a table that is not in the registry.
- `backed_up_outgoing_work_must_pause_on_restore` fails when a backed-up table whose name suggests outgoing work (`outbox`, `queue`, `pending`, `send`, `scheduled`, `draft`) has no `on_restore` statements.
- `restored_outgoing_work_comes_back_paused` models an outbox whose rows all return as `paused_after_restore`.

When the outbox and queued-action tables land, they must:

1. Be registered as `Backed`, with `on_restore` moving every row to a paused state that only an explicit user action releases.
2. Treat `store_meta.restored_at` (Unix seconds, set by every restore) as a second guard. Never send or apply anything created before that time without the user's confirmation.
3. Keep "outcome unknown" sends paused after a restore, never retried automatically.

## Loss model

- Mail on the server can always be downloaded again.
- Local-only data survives losing the keyring only if a backup exists and its passphrase is known. Pigeon cannot reset or recover the passphrase.
- Losing the database key with no usable backup loses local-only data. Pigeon reports this as "key missing" and points to restore. It never generates a replacement key over an existing store.
- Store encryption and backups do not protect against malware or root acting in an unlocked session. Full-disk encryption remains complementary.
