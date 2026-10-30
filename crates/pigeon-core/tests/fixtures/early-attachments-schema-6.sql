-- Historical pre-integration PR #3 schema-6 addition (f6c8e7).
CREATE TABLE attachment_blobs (
        message INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
        part TEXT NOT NULL,
        content_type TEXT NOT NULL,
        data BLOB NOT NULL,
        stored_at INTEGER NOT NULL,
        PRIMARY KEY (message, part)
    );
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
PRAGMA user_version = 6;
