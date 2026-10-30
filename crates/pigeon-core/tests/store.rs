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
