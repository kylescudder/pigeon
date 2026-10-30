use std::fs;

use pigeon_core::store::{DbKey, Store, StoreError};

fn insert(store: &Store, subject: &str, body: &str) {
    store
        .connection()
        .execute(
            "INSERT INTO messages (account, folder, sender, recipients, subject, body_text, date)
             VALUES ('acct', 'INBOX', 'alice@pigeon.test', 'bob@pigeon.test', ?1, ?2, 0)",
            (subject, body),
        )
        .unwrap();
}

#[test]
fn diagnostics_report_sqlcipher_fts5_memory_temp_and_wal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("mail.db"), &DbKey::generate().unwrap()).unwrap();
    let checks = store.diagnostics().unwrap();
    for check in &checks {
        assert!(check.ok, "{check:?}");
    }
    assert!(
        checks
            .iter()
            .any(|c| c.name == "SQLCipher linked" && c.detail.contains("community"))
    );
}

#[test]
fn wrong_key_is_rejected_and_right_key_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let key = DbKey::generate().unwrap();
    {
        let store = Store::open(&path, &key).unwrap();
        insert(&store, "Quarterly report", "numbers inside");
    }
    let wrong = DbKey::generate().unwrap();
    assert!(matches!(
        Store::open(&path, &wrong),
        Err(StoreError::WrongKey)
    ));

    let same = DbKey::from_bytes(key.as_bytes()).unwrap();
    let store = Store::open(&path, &same).unwrap();
    let count: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn fts5_search_finds_body_and_subject_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("mail.db"), &DbKey::generate().unwrap()).unwrap();
    insert(
        &store,
        "Invoice 4471",
        "Please find the café receipt attached",
    );
    insert(&store, "Lunch", "Sandwiches at noon");

    let hits: Vec<String> = store
        .connection()
        .prepare(
            "SELECT m.subject FROM messages_fts f JOIN messages m ON m.id = f.rowid
             WHERE messages_fts MATCH ?1 ORDER BY rank",
        )
        .unwrap()
        .query_map(["cafe"], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(hits, ["Invoice 4471"]);

    store
        .connection()
        .execute("DELETE FROM messages WHERE subject = 'Invoice 4471'", [])
        .unwrap();
    let remaining: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'cafe'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

/// Neither the main file nor the WAL may contain plaintext headers, bodies, or
/// index terms, and the file must not carry the plaintext SQLite header.
#[test]
fn database_and_wal_contain_no_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let store = Store::open(&path, &DbKey::generate().unwrap()).unwrap();
    let marker = "CANARYzq7plaintextMARKER";
    insert(&store, marker, &format!("body {marker} body"));

    let wal = dir.path().join("mail.db-wal");
    assert!(
        fs::metadata(&wal).map(|m| m.len() > 0).unwrap_or(false),
        "expected WAL content"
    );
    let needle = marker.as_bytes();
    for file in [&path, &wal] {
        let bytes = fs::read(file).unwrap();
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "plaintext in {file:?}"
        );
    }
    assert!(!fs::read(&path).unwrap().starts_with(b"SQLite format 3"));

    // No plaintext temp files beside the store either.
    let extras: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| !["mail.db", "mail.db-wal", "mail.db-shm"].contains(&n.as_str()))
        .collect();
    assert!(extras.is_empty(), "unexpected files: {extras:?}");
    drop(store);
}

#[test]
fn debug_output_redacts_key() {
    let key = DbKey::generate().unwrap();
    let rendered = format!("{key:?}");
    assert_eq!(rendered, "DbKey(<redacted>)");
    assert!(!rendered.contains(&hex::encode(key.as_bytes())));
}

/// It locks and wipes every SQLite allocation, not just keys. With a normal
/// RLIMIT_MEMLOCK that fails on each one, logging millions of lines and
/// slowing sync about fourfold. Keys use SQLCipher's own locked heap either way.
#[test]
fn memory_security_stays_off() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("mail.db"), &DbKey::generate().unwrap()).unwrap();
    let state: String = store
        .connection()
        .query_row("PRAGMA cipher_memory_security", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "0");
}

/// A queued operation on the already-merged main must survive adding attachment storage.
#[test]
fn upgrading_main_schema_six_keeps_operations_and_adds_attachment_storage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let key = DbKey::generate().unwrap();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA key = \"x'{}'\";",
            hex::encode(key.as_bytes())
        ))
        .unwrap();
        conn.execute_batch(include_str!("fixtures/main-schema-6.sql"))
            .unwrap();
        conn.execute("INSERT INTO messages (id,account,folder,sender,recipients,subject,body_text,date) VALUES (42,'acct','INBOX','alice@pigeon.test','bob@pigeon.test','Keep this mail','body',0)", []).unwrap();
        conn.execute("INSERT INTO operations (id,account,kind,payload,state,not_before,created,updated) VALUES ('keep-op','acct','send',?1,'held',0,0,0)", [r#"{"draft":"keep-draft"}"#]).unwrap();
    }
    let store = Store::open(&path, &key).unwrap();
    let conn = store.connection();
    let op: (String, String) = conn
        .query_row(
            "SELECT state,payload FROM operations WHERE id='keep-op'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(op, ("held".into(), r#"{"draft":"keep-draft"}"#.into()));
    conn.execute("INSERT INTO attachment_blobs (message,part,content_type,data,stored_at) VALUES (42,'1','image/png',x'010203',0)", []).unwrap();
    let body: (String, Vec<u8>) = conn.query_row("SELECT subject,data FROM messages JOIN attachment_blobs ON messages.id=message WHERE messages.id=42", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(body, ("Keep this mail".into(), vec![1, 2, 3]));
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 7);
}

/// A developer may already have run the pre-integration attachments build at schema 6.
#[test]
fn upgrading_early_attachment_schema_keeps_blobs_and_adds_operations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let key = DbKey::generate().unwrap();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA key = \"x'{}'\";",
            hex::encode(key.as_bytes())
        ))
        .unwrap();
        let schema_five = include_str!("fixtures/main-schema-6.sql")
            .split("-- migration 6")
            .next()
            .unwrap();
        conn.execute_batch(schema_five).unwrap();
        conn.execute_batch(include_str!("fixtures/early-attachments-schema-6.sql"))
            .unwrap();
        conn.execute("INSERT INTO messages (id,account,folder,sender,recipients,subject,body_text,date) VALUES (42,'acct','INBOX','alice@pigeon.test','bob@pigeon.test','Kept attachment','body',0)", []).unwrap();
        conn.execute("INSERT INTO attachment_blobs (message,part,content_type,data,stored_at) VALUES (42,'1','image/png',x'010203',0)", []).unwrap();
    }
    let store = Store::open(&path, &key).unwrap();
    let conn = store.connection();
    let bytes: Vec<u8> = conn
        .query_row(
            "SELECT data FROM attachment_blobs WHERE message=42 AND part='1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bytes, vec![1, 2, 3]);
    conn.execute("INSERT INTO operations (id,kind,payload,state,not_before,created,updated) VALUES ('new-op','test','{}','held',0,0,0)", []).unwrap();
    let state: String = conn
        .query_row("SELECT state FROM operations WHERE id='new-op'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, "held");
}
