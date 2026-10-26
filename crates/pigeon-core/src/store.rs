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
        // Wipe SQLCipher-managed memory on free; keep temp tables and sort
        // spills in memory so plaintext never lands in temp files.
        conn.execute_batch("PRAGMA cipher_memory_security = ON; PRAGMA temp_store = MEMORY;")?;

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
