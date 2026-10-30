-- Historical main schema from c4cc37f; fixed upgrade fixture, not current migrations.
-- migration 1
CREATE TABLE messages (
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
    END;
-- migration 2
CREATE TABLE mail_accounts (
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
    CREATE TABLE saved_searches (id TEXT PRIMARY KEY, name TEXT NOT NULL, query TEXT NOT NULL);
-- migration 3
CREATE INDEX messages_search_metadata ON messages(id,account,folder,uid_validity,date,unread,has_attachments,sender,recipients,subject);
-- migration 4
CREATE INDEX messages_folder_counts ON messages(account,folder,uid_validity,unread);
    ALTER TABLE mail_folders ADD COLUMN id TEXT;
    UPDATE mail_folders SET id=lower(hex(randomblob(16)));
    CREATE UNIQUE INDEX mail_folder_ids ON mail_folders(id);
    CREATE TRIGGER mail_folders_ai AFTER INSERT ON mail_folders WHEN new.id IS NULL BEGIN
        UPDATE mail_folders SET id=lower(hex(randomblob(16))) WHERE rowid=new.rowid;
    END;
-- migration 5
ALTER TABLE mail_folders ADD COLUMN delimiter TEXT;
-- migration 6
CREATE TABLE operations (
        id TEXT PRIMARY KEY, account TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL,
        state TEXT NOT NULL, not_before INTEGER NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0, irreversible INTEGER NOT NULL DEFAULT 0,
        last_error TEXT, depends_on TEXT,
        created INTEGER NOT NULL, updated INTEGER NOT NULL
    );
    CREATE INDEX operations_due ON operations(state, not_before);
    CREATE INDEX operations_account ON operations(account);
PRAGMA user_version = 6;
