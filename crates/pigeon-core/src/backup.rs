//! Encrypted backups of local-only data. The file format is documented in
//! docs/backup-format.md: an age v1 file, protected by a passphrase through
//! age's scrypt recipient, whose plaintext is one JSON document.
//!
//! Plaintext never reaches disk. Backups stream JSON straight into the age
//! writer, and restores parse straight from the age reader.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;
use rusqlite::Connection;
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::store::{DbKey, SCHEMA_VERSION, Store, StoreError};

pub const FORMAT: &str = "pigeon-backup";
pub const VERSION: u32 = 1;
pub const FILE_EXTENSION: &str = "pigeon-backup";
pub const MIN_PASSPHRASE_CHARS: usize = 12;
const MAX_PASSPHRASE_BYTES: usize = 4096;
/// scrypt cost N = 2^18: about 256 MiB and well under a second on a laptop.
/// Fixed rather than calibrated so memory use is predictable on every device.
pub const SCRYPT_LOG_N: u8 = 18;
/// Local-only data is small; anything far larger is not a backup we wrote.
const MAX_PAYLOAD_BYTES: u64 = 256 << 20;

/// What a store table means for backups. Every table must be listed in
/// [`TABLES`]; a test fails when a migration adds one that is not.
#[derive(Debug, Clone, Copy)]
pub enum TableRole {
    /// Local-only data, written to backups. `on_restore` runs after a restore
    /// once the store is fully migrated. Tables holding outgoing work (an
    /// outbox, queued server actions) must pause every row here, so that a
    /// restore can never send or apply anything without review.
    Backed { on_restore: &'static [&'static str] },
    /// Copied from the server and downloaded again after a restore.
    Redownloaded,
    /// Describes this device's store rather than the user's data.
    Device,
}

pub const TABLES: &[(&str, TableRole)] = &[
    (
        "mail_accounts",
        TableRole::Backed {
            // Sync progress belongs to the store the backup came from; the
            // restored accounts start again and download their mail.
            on_restore: &[
                "UPDATE mail_accounts SET sync_status='idle',last_error=NULL,last_synced=NULL,sync_folder=NULL,sync_cached=0,sync_total=0",
            ],
        },
    ),
    ("saved_searches", TableRole::Backed { on_restore: &[] }),
    ("settings", TableRole::Backed { on_restore: &[] }),
    ("recipient_history", TableRole::Backed { on_restore: &[] }),
    // Drafts never execute; only an explicit Send creates an operation.
    ("drafts", TableRole::Backed { on_restore: &[] }),
    (
        "operations",
        TableRole::Backed {
            on_restore: &[
                "UPDATE operations SET state='held',last_error='Restored from backup; review before running' WHERE state NOT IN ('done','cancelled')",
            ],
        },
    ),
    (
        "outbox",
        TableRole::Backed {
            on_restore: &[
                "UPDATE operations SET state='held',last_error='Restored from backup; review before sending' WHERE id IN (SELECT submit_op FROM outbox UNION SELECT sent_copy_op FROM outbox) AND state NOT IN ('done','cancelled')",
            ],
        },
    ),
    (
        "mail_actions",
        TableRole::Backed {
            on_restore: &[
                "UPDATE mail_actions SET state='conflict',error='Restored from backup; review before Retry'",
            ],
        },
    ),
    (
        "mail_action_items",
        TableRole::Backed {
            on_restore: &[
                "UPDATE mail_actions SET state='conflict',error='Restored from backup; review before Retry' WHERE id IN (SELECT action FROM mail_action_items)",
            ],
        },
    ),
    ("attachment_blobs", TableRole::Redownloaded),
    ("mail_folders", TableRole::Redownloaded),
    ("messages", TableRole::Redownloaded),
    ("messages_fts", TableRole::Redownloaded),
    ("messages_fts_config", TableRole::Redownloaded),
    ("messages_fts_data", TableRole::Redownloaded),
    ("messages_fts_docsize", TableRole::Redownloaded),
    ("messages_fts_idx", TableRole::Redownloaded),
    ("sender_images", TableRole::Redownloaded),
    ("store_meta", TableRole::Device),
];

/// Set in `store_meta` by every restore. Outgoing work created before this
/// time came from a backup and must not leave the device without review.
pub const RESTORED_AT: &str = "restored_at";

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("use a backup passphrase of at least {MIN_PASSPHRASE_CHARS} characters")]
    WeakPassphrase,
    #[error("the passphrase does not open this backup, or the backup has been altered")]
    WrongPassphrase,
    #[error("this file is damaged or is not a Pigeon backup")]
    Damaged,
    #[error("this backup was made by a newer version of Pigeon; update Pigeon to restore it")]
    Newer,
    #[error("finish or cancel mail syncing, account changes, and account removal before restoring")]
    Busy,
    #[error("could not access the backup file: {0}")]
    Io(String),
    #[error("could not restore into encrypted storage: {0}")]
    Storage(String),
}

impl From<StoreError> for BackupError {
    fn from(error: StoreError) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<rusqlite::Error> for BackupError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

/// Counts shown to the user after a backup or restore. Never content.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Summary {
    pub accounts: usize,
    pub passwords: usize,
    pub saved_searches: usize,
    /// After a restore, the file name the replaced store was moved to.
    pub previous_store: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob { hex: String },
}

#[derive(Debug, Serialize, Deserialize)]
struct TableDump {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Payload {
    format: String,
    version: u32,
    created_at: i64,
    schema_version: usize,
    tables: BTreeMap<String, TableDump>,
    /// Keyring items by name, hex encoded. Only account passwords.
    secrets: BTreeMap<String, String>,
}

impl Drop for Payload {
    fn drop(&mut self) {
        for secret in self.secrets.values_mut() {
            secret.zeroize();
        }
        for row in self.tables.values_mut().flat_map(|t| t.rows.iter_mut()) {
            for value in row {
                match value {
                    Value::Text(text) | Value::Blob { hex: text } => text.zeroize(),
                    _ => {}
                }
            }
        }
    }
}

/// A keyring item name and its secret, restored from a backup.
pub type KeyringItem = (String, zeroize::Zeroizing<Vec<u8>>);

pub fn password_name(account_id: &str) -> String {
    format!("account:{account_id}:password")
}

impl Payload {
    pub fn account_ids(&self) -> Vec<String> {
        let Some(table) = self.tables.get("mail_accounts") else {
            return Vec::new();
        };
        let Some(column) = table.columns.iter().position(|c| c == "id") else {
            return Vec::new();
        };
        table
            .rows
            .iter()
            .filter_map(|row| match row.get(column) {
                Some(Value::Text(id)) => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn add_password(&mut self, account_id: &str, password: &[u8]) {
        self.secrets
            .insert(password_name(account_id), hex::encode(password));
    }

    /// Account passwords to write back to the keyring. Only names of the form
    /// `account:<id>:password` for an account inside this backup are
    /// accepted, so a crafted file cannot replace any other keyring item.
    pub fn passwords(&self) -> Result<Vec<KeyringItem>, BackupError> {
        let accounts = self.account_ids();
        self.secrets
            .iter()
            .map(|(name, value)| {
                let known = accounts.iter().any(|id| password_name(id) == *name);
                if !known {
                    return Err(BackupError::Damaged);
                }
                let bytes = hex::decode(value).map_err(|_| BackupError::Damaged)?;
                Ok((name.clone(), zeroize::Zeroizing::new(bytes)))
            })
            .collect()
    }

    pub fn summary(&self) -> Summary {
        let rows = |name: &str| self.tables.get(name).map_or(0, |t| t.rows.len());
        Summary {
            accounts: rows("mail_accounts"),
            passwords: self.secrets.len(),
            saved_searches: rows("saved_searches"),
            previous_store: None,
        }
    }
}

pub fn check_passphrase(passphrase: &str) -> Result<(), BackupError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS || passphrase.len() > MAX_PASSPHRASE_BYTES
    {
        return Err(BackupError::WeakPassphrase);
    }
    Ok(())
}

pub fn default_file_name() -> String {
    format!(
        "pigeon-{}.{FILE_EXTENSION}",
        chrono::Local::now().format("%Y%m%d-%H%M")
    )
}

/// Copies every backed-up table. Call inside a read transaction so that all
/// tables come from one consistent snapshot of the store.
pub fn snapshot(conn: &Connection) -> Result<Payload, BackupError> {
    snapshot_tables(conn, TABLES)
}

fn snapshot_tables(
    conn: &Connection,
    tables: &[(&str, TableRole)],
) -> Result<Payload, BackupError> {
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let mut dumps = BTreeMap::new();
    for (name, role) in tables {
        if !matches!(role, TableRole::Backed { .. }) {
            continue;
        }
        let columns = table_columns(conn, name)?;
        if columns.is_empty() {
            continue;
        }
        let sql = format!(
            "SELECT {} FROM {} ORDER BY rowid",
            columns
                .iter()
                .map(|c| quote(c))
                .collect::<Vec<_>>()
                .join(","),
            quote(name)
        );
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
            .query_map([], |row| {
                (0..columns.len())
                    .map(|i| {
                        Ok(match row.get_ref(i)? {
                            ValueRef::Null => Value::Null,
                            ValueRef::Integer(v) => Value::Integer(v),
                            ValueRef::Real(v) => Value::Real(v),
                            ValueRef::Text(v) => Value::Text(String::from_utf8_lossy(v).into()),
                            ValueRef::Blob(v) => Value::Blob {
                                hex: hex::encode(v),
                            },
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        dumps.insert((*name).to_owned(), TableDump { columns, rows });
    }
    Ok(Payload {
        format: FORMAT.into(),
        version: VERSION,
        created_at: chrono::Utc::now().timestamp(),
        schema_version: schema_version as usize,
        tables: dumps,
        secrets: BTreeMap::new(),
    })
}

/// Encrypts `payload` to `dest`. The file appears only once complete: it is
/// written beside the destination under a temporary name, synced, and renamed.
pub fn write(payload: &Payload, dest: &Path, passphrase: &str) -> Result<(), BackupError> {
    check_passphrase(passphrase)?;
    let partial = sibling(dest, "partial")?;
    let result = write_partial(payload, &partial, passphrase).and_then(|()| {
        std::fs::rename(&partial, dest).map_err(io_error)?;
        sync_parent(dest);
        Ok(())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn write_partial(payload: &Payload, path: &Path, passphrase: &str) -> Result<(), BackupError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(path).map_err(io_error)?;
    let mut recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.to_owned()));
    recipient.set_work_factor(SCRYPT_LOG_N);
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|e| BackupError::Io(e.to_string()))?;
    let mut writer = encryptor
        .wrap_output(BufWriter::new(file))
        .map_err(io_error)?;
    serde_json::to_writer(&mut writer, payload).map_err(|e| BackupError::Io(e.to_string()))?;
    let file = writer
        .finish()
        .map_err(io_error)?
        .into_inner()
        .map_err(|e| io_error(e.into_error()))?;
    file.sync_all().map_err(io_error)
}

/// Decrypts and validates a backup without touching the store.
pub fn read(src: &Path, passphrase: &str) -> Result<Payload, BackupError> {
    if passphrase.is_empty() || passphrase.len() > MAX_PASSPHRASE_BYTES {
        return Err(BackupError::WrongPassphrase);
    }
    let file = File::open(src).map_err(io_error)?;
    let decryptor = age::Decryptor::new_buffered(BufReader::new(file)).map_err(decrypt_error)?;
    if !decryptor.is_scrypt() {
        return Err(BackupError::Damaged);
    }
    let identity = age::scrypt::Identity::new(SecretString::from(passphrase.to_owned()));
    let reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(decrypt_error)?;
    // age authenticates each chunk and the end of the stream, so a damaged or
    // truncated file fails here as an I/O error rather than parsing.
    let payload: Payload = serde_json::from_reader(BufReader::new(reader.take(MAX_PAYLOAD_BYTES)))
        .map_err(|_| BackupError::Damaged)?;
    if payload.format != FORMAT {
        return Err(BackupError::Damaged);
    }
    if payload.version > VERSION || payload.schema_version > SCHEMA_VERSION {
        return Err(BackupError::Newer);
    }
    if payload.schema_version == 0
        || payload
            .tables
            .values()
            .any(|t| t.rows.iter().any(|row| row.len() != t.columns.len()))
    {
        return Err(BackupError::Damaged);
    }
    payload.passwords()?;
    Ok(payload)
}

pub fn summary(src: &Path, passphrase: &str) -> Result<Summary, BackupError> {
    Ok(read(src, passphrase)?.summary())
}

/// Builds a complete store at `path` from `payload`, encrypted with `key`.
/// The caller swaps it into place; a failure here leaves the live store alone.
pub fn build_store(path: &Path, key: &DbKey, payload: &Payload) -> Result<(), BackupError> {
    build_store_with(path, key, payload, TABLES, "")
}

fn build_store_with(
    path: &Path,
    key: &DbKey,
    payload: &Payload,
    tables: &[(&str, TableRole)],
    extra_schema: &str,
) -> Result<(), BackupError> {
    for name in payload.tables.keys() {
        let backed = tables
            .iter()
            .any(|(t, role)| t == name && matches!(role, TableRole::Backed { .. }));
        if !backed {
            return Err(BackupError::Damaged);
        }
    }
    let store = Store::open_at_version(path, key, payload.schema_version)?;
    let conn = store.connection();
    conn.execute_batch(extra_schema)?;
    let tx = conn.unchecked_transaction()?;
    for (name, _) in tables {
        let Some(dump) = payload.tables.get(*name) else {
            continue;
        };
        let existing = table_columns(&tx, name)?;
        if dump.columns.is_empty() || dump.columns.iter().any(|c| !existing.contains(c)) {
            return Err(BackupError::Damaged);
        }
        let sql = format!(
            "INSERT INTO {}({}) VALUES ({})",
            quote(name),
            dump.columns
                .iter()
                .map(|c| quote(c))
                .collect::<Vec<_>>()
                .join(","),
            vec!["?"; dump.columns.len()].join(",")
        );
        let mut statement = tx.prepare(&sql)?;
        for row in &dump.rows {
            let values = row
                .iter()
                .map(|value| {
                    Ok(match value {
                        Value::Null => SqlValue::Null,
                        Value::Integer(v) => SqlValue::Integer(*v),
                        Value::Real(v) => SqlValue::Real(*v),
                        Value::Text(v) => SqlValue::Text(v.clone()),
                        Value::Blob { hex } => {
                            SqlValue::Blob(hex::decode(hex).map_err(|_| BackupError::Damaged)?)
                        }
                    })
                })
                .collect::<Result<Vec<_>, BackupError>>()?;
            statement
                .execute(rusqlite::params_from_iter(values))
                .map_err(|_| BackupError::Damaged)?;
        }
    }
    tx.commit()?;
    store.migrate_to(SCHEMA_VERSION)?;
    let tx = conn.unchecked_transaction()?;
    for (_, role) in tables {
        if let TableRole::Backed { on_restore } = role {
            for sql in *on_restore {
                tx.execute_batch(sql)?;
            }
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO store_meta(key,value) VALUES (?1,?2)",
        (RESTORED_AT, chrono::Utc::now().timestamp().to_string()),
    )?;
    tx.commit()?;
    // Leave a single self-contained file to rename into place.
    conn.execute_batch("PRAGMA journal_mode = DELETE;")?;
    Ok(())
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, BackupError> {
    let mut statement = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let columns = statement
        .query_map([table], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(columns)
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// A unique hidden path beside `path`, for files that must be renamed into
/// place on the same filesystem.
pub fn sibling(path: &Path, purpose: &str) -> Result<PathBuf, BackupError> {
    let name = path
        .file_name()
        .ok_or_else(|| BackupError::Io("choose a file name for the backup".into()))?
        .to_string_lossy();
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).map_err(|e| BackupError::Io(e.to_string()))?;
    Ok(path.with_file_name(format!(".{name}.{purpose}-{}", hex::encode(bytes))))
}

pub fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

fn io_error(error: io::Error) -> BackupError {
    BackupError::Io(error.to_string())
}

fn decrypt_error(error: age::DecryptError) -> BackupError {
    match error {
        age::DecryptError::DecryptionFailed
        | age::DecryptError::KeyDecryptionFailed
        | age::DecryptError::NoMatchingKeys => BackupError::WrongPassphrase,
        age::DecryptError::Io(e)
            if !matches!(
                e.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
            ) =>
        {
            BackupError::Io(e.to_string())
        }
        _ => BackupError::Damaged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTGOING: &[&str] = &[
        "outbox",
        "queue",
        "pending",
        "send",
        "scheduled",
        "operation",
        "mail_action",
    ];

    fn store(dir: &Path, name: &str, key: &DbKey) -> Store {
        Store::open(&dir.join(name), key).unwrap()
    }

    fn user_tables(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn every_store_table_declares_whether_it_is_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let key = DbKey::generate().unwrap();
        let store = store(dir.path(), "mail.db", &key);
        for table in user_tables(store.connection()) {
            assert!(
                TABLES.iter().any(|(name, _)| *name == table),
                "table `{table}` is not classified in backup::TABLES; decide whether backups \
                 include it, and pause any outgoing work it holds in `on_restore`"
            );
        }
    }

    #[test]
    fn backed_up_outgoing_work_must_pause_on_restore() {
        for (name, role) in TABLES {
            if let TableRole::Backed { on_restore } = role
                && OUTGOING.iter().any(|word| name.contains(word))
            {
                assert!(
                    !on_restore.is_empty(),
                    "`{name}` looks like outgoing work; pause it in on_restore"
                );
            }
        }
    }

    #[test]
    fn restored_outgoing_work_comes_back_paused() {
        let dir = tempfile::tempdir().unwrap();
        let key = DbKey::generate().unwrap();
        let source = store(dir.path(), "source.db", &key);
        source.connection().execute_batch("INSERT INTO operations(id,kind,payload,state,not_before,created,updated) VALUES ('a','send','{}','pending',0,0,0),('b','send','{}','running',0,0,0),('c','send','{}','unknown',0,0,0),('d','send','{}','done',0,0,0),('e','send','{}','cancelled',0,0,0);").unwrap();
        let payload = snapshot(source.connection()).unwrap();
        let target = dir.path().join("restored.db");
        build_store(&target, &key, &payload).unwrap();
        let restored = Store::open(&target, &key).unwrap();
        let states: Vec<String> = restored
            .connection()
            .prepare("SELECT state FROM operations ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(states, ["held", "held", "held", "done", "cancelled"]);
        let restored_at: String = restored
            .connection()
            .query_row(
                "SELECT value FROM store_meta WHERE key=?1",
                [RESTORED_AT],
                |r| r.get(0),
            )
            .unwrap();
        assert!(restored_at.parse::<i64>().unwrap() > 0);
    }

    #[test]
    fn backup_from_an_older_schema_restores_into_the_current_one() {
        let dir = tempfile::tempdir().unwrap();
        let key = DbKey::generate().unwrap();
        // Version 2 introduced accounts and saved searches.
        let old = Store::open_at_version(&dir.path().join("old.db"), &key, 2).unwrap();
        old.connection()
            .execute_batch(
                "INSERT INTO mail_accounts(id,config,sync_status) VALUES ('a1','{}','syncing');
                 INSERT INTO saved_searches VALUES ('s1','Invoices','{\"text\":\"invoice\"}');",
            )
            .unwrap();
        let payload = snapshot(old.connection()).unwrap();
        assert_eq!(payload.schema_version, 2);
        let target = dir.path().join("restored.db");
        build_store(&target, &key, &payload).unwrap();
        let restored = Store::open(&target, &key).unwrap();
        let version: i64 = restored
            .connection()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version as usize, SCHEMA_VERSION);
        let status: String = restored
            .connection()
            .query_row("SELECT sync_status FROM mail_accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "idle");
    }

    #[test]
    fn crafted_backups_cannot_write_other_keyring_items_or_tables() {
        let dir = tempfile::tempdir().unwrap();
        let key = DbKey::generate().unwrap();
        let source = store(dir.path(), "source.db", &key);
        source
            .connection()
            .execute_batch("INSERT INTO mail_accounts(id,config) VALUES ('a1','{}');")
            .unwrap();
        let mut payload = snapshot(source.connection()).unwrap();
        payload
            .secrets
            .insert("database-key".into(), hex::encode([0u8; 32]));
        assert!(matches!(payload.passwords(), Err(BackupError::Damaged)));

        let mut payload = snapshot(source.connection()).unwrap();
        payload.tables.insert(
            "messages".into(),
            TableDump {
                columns: vec!["id".into()],
                rows: vec![],
            },
        );
        let target = dir.path().join("restored.db");
        assert!(matches!(
            build_store(&target, &key, &payload),
            Err(BackupError::Damaged)
        ));
    }
}
