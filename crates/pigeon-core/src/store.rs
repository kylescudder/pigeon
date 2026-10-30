//! SQLCipher-encrypted local store. The Rust layer owns migrations and writes.

use std::path::Path;

use rusqlite::{Connection, ErrorCode, OpenFlags};
use zeroize::Zeroizing;

use crate::diagnostics::Check;

/// A raw 256-bit SQLCipher key. Passed as a raw key so SQLCipher skips its
/// passphrase KDF; the key is random, not user-chosen.
pub struct DbKey(Zeroizing<[u8; 32]>);

impl DbKey {
    pub fn generate() -> Result<Self, StoreError> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut()).map_err(|e| StoreError::Random(e.to_string()))?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StoreError> {
        let array: [u8; 32] = bytes.try_into().map_err(|_| StoreError::MalformedKey)?;
        Ok(Self(Zeroizing::new(array)))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }

    fn pragma_value(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("\"x'{}'\"", hex::encode(self.0.as_ref())))
    }
}

impl std::fmt::Debug for DbKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DbKey(<redacted>)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the database key does not open this store")]
    WrongKey,
    #[error("the stored database key is malformed")]
    MalformedKey,
    #[error("SQLite is not linked against SQLCipher; refusing to store mail unencrypted")]
    NotSqlCipher,
    #[error("SQLite was built without FTS5")]
    NoFts5,
    #[error("random number generation failed: {0}")]
    Random(String),
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub struct Store {
    conn: Connection,
}

const MIGRATIONS: &[&str] = &[
    // 1: provisional message table and FTS5 index used to prove the foundation.
    // Milestone 1 replaces this with the real sync schema.
    "CREATE TABLE messages (
        id INTEGER PRIMARY KEY,
        account TEXT NOT NULL,
        folder TEXT NOT NULL,
        sender TEXT NOT NULL,
        recipients TEXT NOT NULL,
        subject TEXT NOT NULL,
        body_text TEXT NOT NULL,
        date INTEGER NOT NULL,
        unread INTEGER NOT NULL DEFAULT 1,
        has_attachments INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX messages_account_folder_date ON messages(account, folder, date DESC);
    CREATE VIRTUAL TABLE messages_fts USING fts5(
        sender, recipients, subject, body_text,
        content='messages', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts(rowid, sender, recipients, subject, body_text)
        VALUES (new.id, new.sender, new.recipients, new.subject, new.body_text);
    END;
    CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, sender, recipients, subject, body_text)
        VALUES ('delete', old.id, old.sender, old.recipients, old.subject, old.body_text);
    END;
    CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, sender, recipients, subject, body_text)
        VALUES ('delete', old.id, old.sender, old.recipients, old.subject, old.body_text);
        INSERT INTO messages_fts(rowid, sender, recipients, subject, body_text)
        VALUES (new.id, new.sender, new.recipients, new.subject, new.body_text);
    END;",
    "CREATE TABLE mail_accounts (
        id TEXT PRIMARY KEY, config TEXT NOT NULL,
        sync_status TEXT NOT NULL DEFAULT 'idle', last_error TEXT,
        last_synced INTEGER, sync_folder TEXT, sync_cached INTEGER NOT NULL DEFAULT 0,
        sync_total INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE mail_folders (
        account TEXT NOT NULL REFERENCES mail_accounts(id) ON DELETE CASCADE,
        name TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'other',
        uid_validity INTEGER, total INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (account, name)
    );
    ALTER TABLE messages ADD COLUMN uid INTEGER;
    ALTER TABLE messages ADD COLUMN uid_validity INTEGER;
    ALTER TABLE messages ADD COLUMN body_html TEXT;
    ALTER TABLE messages ADD COLUMN message_id TEXT NOT NULL DEFAULT '';
    ALTER TABLE messages ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]';
    CREATE UNIQUE INDEX messages_remote_identity ON messages(account,folder,uid_validity,uid);
    CREATE INDEX messages_date ON messages(date DESC,id DESC);
    CREATE INDEX messages_unread_date ON messages(unread,date DESC,id DESC);
    CREATE TABLE saved_searches (id TEXT PRIMARY KEY, name TEXT NOT NULL, query TEXT NOT NULL);",
    // Keep search candidates in a compact covering index. Full message bodies
    // must be read only after pagination, not for every full-text match.
    "CREATE INDEX messages_search_metadata ON messages(id,account,folder,uid_validity,date,unread,has_attachments,sender,recipients,subject);",
    "CREATE INDEX messages_folder_counts ON messages(account,folder,uid_validity,unread);
    ALTER TABLE mail_folders ADD COLUMN id TEXT;
    UPDATE mail_folders SET id=lower(hex(randomblob(16)));
    CREATE UNIQUE INDEX mail_folder_ids ON mail_folders(id);
    CREATE TRIGGER mail_folders_ai AFTER INSERT ON mail_folders WHEN new.id IS NULL BEGIN
        UPDATE mail_folders SET id=lower(hex(randomblob(16))) WHERE rowid=new.rowid;
    END;",
    // Filled in by the next sync of each folder; until then folders show flat.
    "ALTER TABLE mail_folders ADD COLUMN delimiter TEXT;",
    // Durable queue of server-changing work (mail/operations.rs). No foreign
    // key to accounts: unsent mail must never vanish with a cascade.
    "CREATE TABLE operations (
        id TEXT PRIMARY KEY, account TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL,
        state TEXT NOT NULL, not_before INTEGER NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0, irreversible INTEGER NOT NULL DEFAULT 0,
        last_error TEXT, depends_on TEXT,
        created INTEGER NOT NULL, updated INTEGER NOT NULL
    );
    CREATE INDEX operations_due ON operations(state, not_before);
    CREATE INDEX operations_account ON operations(account);",
    // 7: keep schema 6 unchanged; also repair early attachment builds that
    // used version 6 before the operations queue landed. Preserve their blobs.
    // Attachment payloads the user chose to keep. They go with their message
    // row, so pruning, UIDVALIDITY replacement and account removal clear them.
    "CREATE TABLE IF NOT EXISTS operations (
        id TEXT PRIMARY KEY, account TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL,
        state TEXT NOT NULL, not_before INTEGER NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0, irreversible INTEGER NOT NULL DEFAULT 0,
        last_error TEXT, depends_on TEXT,
        created INTEGER NOT NULL, updated INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS operations_due ON operations(state, not_before);
    CREATE INDEX IF NOT EXISTS operations_account ON operations(account);
    CREATE TABLE IF NOT EXISTS attachment_blobs (
        message INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
        part TEXT NOT NULL,
        content_type TEXT NOT NULL,
        data BLOB NOT NULL,
        stored_at INTEGER NOT NULL,
        PRIMARY KEY (message, part)
    );
    CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
];

impl Store {
    /// Opens (creating if absent) an encrypted store. Fails closed if SQLCipher
    /// is not linked or the key is wrong.
    pub fn open(path: &Path, key: &DbKey) -> Result<Self, StoreError> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Self::configure(conn, key)
    }

    fn configure(conn: Connection, key: &DbKey) -> Result<Self, StoreError> {
        let cipher_version: Option<String> = conn
            .query_row("PRAGMA cipher_version", [], |r| r.get(0))
            .ok();
        if cipher_version.as_deref().is_none_or(str::is_empty) {
            return Err(StoreError::NotSqlCipher);
        }

        conn.execute_batch(&format!("PRAGMA key = {};", key.pragma_value().as_str()))?;
        // Keep temp tables and sort spills in memory so plaintext never lands in
        // temp files. Leave cipher_memory_security off: keys already use
        // SQLCipher's locked private heap, and it adds an mlock to every SQLite
        // allocation, which floods stderr once RLIMIT_MEMLOCK is exhausted.
        conn.execute_batch("PRAGMA temp_store = MEMORY;")?;

        // The first read is where a wrong key surfaces.
        match conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        }) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::NotADatabase => {
                return Err(StoreError::WrongKey);
            }
            Err(e) => return Err(e.into()),
        }

        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;

        let store = Self { conn };
        if !store.compile_option("ENABLE_FTS5")? {
            return Err(StoreError::NoFts5);
        }
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), StoreError> {
        let current: usize =
            self.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", index + 1))?;
            tx.commit()?;
        }
        Ok(())
    }

    fn compile_option(&self, option: &str) -> Result<bool, StoreError> {
        let mut stmt = self.conn.prepare("PRAGMA compile_options")?;
        let found = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(Result::ok)
            .any(|o| o == option);
        Ok(found)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Foundation diagnostics; contains configuration only, never content.
    pub fn diagnostics(&self) -> Result<Vec<Check>, StoreError> {
        let cipher: String = self
            .conn
            .query_row("PRAGMA cipher_version", [], |r| r.get(0))?;
        let provider: String = self
            .conn
            .query_row("PRAGMA cipher_provider", [], |r| r.get(0))
            .unwrap_or_default();
        let temp_store: i64 = self.conn.query_row("PRAGMA temp_store", [], |r| r.get(0))?;
        let journal: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let schema: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        Ok(vec![
            Check::new("SQLCipher linked", true, format!("{cipher} ({provider})")),
            Check::new(
                "FTS5 available",
                self.compile_option("ENABLE_FTS5")?,
                "compile option",
            ),
            Check::new(
                "Temporary storage in memory",
                temp_store == 2,
                format!("temp_store={temp_store}"),
            ),
            Check::new(
                "Write-ahead log",
                journal == "wal",
                format!("journal_mode={journal}"),
            ),
            Check::new(
                "Schema migrated",
                schema as usize == MIGRATIONS.len(),
                format!("user_version={schema}"),
            ),
        ])
    }
}
