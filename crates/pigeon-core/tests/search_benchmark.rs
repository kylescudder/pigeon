//! Explicit performance gate. Uses the public engine and a synthetic external
//! source; real IMAP transport is verified separately against Dovecot.
use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, Attachment, FolderChange, MailEngine, MailError, MessageContent, SearchQuery,
    TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

struct Corpus;
struct CorpusSession;
impl MailConnector for Corpus {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        Ok(Box::new(CorpusSession))
    }
}
impl MailSession for CorpusSession {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(vec![
            RemoteFolder {
                name: "INBOX".into(),
                kind: "inbox".into(),
                delimiter: Some("/".into()),
            },
            RemoteFolder {
                name: "Archive".into(),
                kind: "archive".into(),
                delimiter: Some("/".into()),
            },
        ])
    }
    fn snapshot(&mut self, _: &str) -> Result<FolderSnapshot, MailError> {
        Ok(FolderSnapshot {
            uid_validity: 1,
            messages: (1..=12_500)
                .map(|uid| RemoteFlag {
                    uid,
                    unread: uid % 3 == 0,
                })
                .collect(),
        })
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        Ok(uids.iter().map(|uid| RemoteMessage { uid: *uid, content: MessageContent {
            sender: if uid % 2 == 0 { "Alice <alice@example.test>" } else { "Bob <bob@example.test>" }.into(),
            recipients: "Kyle <kyle@example.test>".into(), subject: format!("Project quarterly report {uid}"),
            body_text: format!("Invoice receipt {uid}. Coastal migration project update. The annual report covers progress across the engineering team. Meeting notes and next actions follow. {}", "We reviewed the quarterly budget and prepared the next release. ".repeat(8)),
            date: 1_793_005_200 + i64::from(*uid), attachments: if uid % 5 == 0 { vec![Attachment { part: "2".into(), filename: "report.pdf".into(), content_type: "application/pdf".into(), size: 8192, content_id: None }] } else { vec![] }, ..Default::default()
        } }).collect())
    }
}

#[test]
#[ignore = "explicit 100k-message performance gate; run with --ignored --nocapture"]
fn indexed_search_first_page_p95_is_under_200_ms_for_100k_messages() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        secrets.clone(),
        Arc::new(Corpus),
    );
    let start = Instant::now();
    for index in 1..=4 {
        let account = engine
            .add_account(
                AccountConfig {
                    managed: false,
                    name: format!("Mailbox {index}"),
                    address: format!("mailbox{index}@example.test"),
                    host: "imap.example.test".into(),
                    port: 993,
                    username: format!("mailbox{index}@example.test"),
                    security: TransportSecurity::Tls,
                    ca_certificate: None,
                    sending: Default::default(),
                },
                Zeroizing::new(b"benchmark-only".to_vec()),
            )
            .unwrap();
        engine.sync_account(&account.id).unwrap();
    }
    assert_eq!(
        engine.search(&SearchQuery::default()).unwrap().total,
        100_000
    );
    eprintln!("Indexed 100000 messages in {:?}", start.elapsed());
    drop(engine);
    let cold_start = Instant::now();
    let engine = MailEngine::with_connector(dir.path().join("mail.db"), secrets, Arc::new(Corpus));
    assert_eq!(
        engine
            .search(&SearchQuery {
                text: "receipt".into(),
                ..Default::default()
            })
            .unwrap()
            .total,
        100_000
    );
    eprintln!(
        "Cold SQLCipher connection + first search: {:?}; OS filesystem caches retained",
        cold_start.elapsed()
    );
    let queries = [
        "receipt",
        "migration",
        "\"annual report\"",
        "from:alice",
        "is:unread",
        "has:attachment",
        "folder:Archive receipt",
        "account:\"Mailbox 2\" report",
        "project AND quarterly",
        "after:2026-01-01 invoice",
    ];
    let mut timings = Vec::new();
    let rounds = 10;
    for _ in 0..rounds {
        for text in queries {
            let start = Instant::now();
            let result = engine
                .search(&SearchQuery {
                    text: text.into(),
                    ..Default::default()
                })
                .unwrap();
            assert!(!result.messages.is_empty(), "{text}");
            assert!(result.messages.len() <= 50);
            let elapsed = start.elapsed();
            if timings.len() < 10 {
                eprintln!("query={text:?} elapsed={elapsed:?}");
            }
            timings.push(elapsed);
        }
    }
    timings.sort();
    let p95 = timings[(timings.len() * 95).div_ceil(100).saturating_sub(1)];
    eprintln!(
        "queries=100, page=50, accounts=4, folders=8, p50={:?}, p95={p95:?}, max={:?}",
        timings[timings.len() / 2],
        timings[timings.len() - 1]
    );
    assert!(
        p95 < Duration::from_millis(200),
        "p95 {p95:?} exceeds the 200ms target"
    );
    let extra = engine
        .add_account(
            AccountConfig {
                managed: false,
                name: "Indexing account".into(),
                address: "indexing@example.test".into(),
                host: "imap.example.test".into(),
                port: 993,
                username: "indexing@example.test".into(),
                security: TransportSecurity::Tls,
                ca_certificate: None,
                sending: Default::default(),
            },
            Secret::new(b"benchmark-only".to_vec()),
        )
        .unwrap();
    std::thread::scope(|scope| {
        let indexing = scope.spawn(|| engine.sync_account(&extra.id));
        let mut concurrent = Vec::new();
        for _ in 0..2 {
            for text in queries {
                let start = Instant::now();
                let result = engine
                    .search(&SearchQuery {
                        text: text.into(),
                        ..Default::default()
                    })
                    .unwrap();
                assert!(!result.messages.is_empty());
                concurrent.push(start.elapsed());
            }
        }
        indexing.join().unwrap().unwrap();
        concurrent.sort();
        eprintln!(
            "During fifth-account indexing (100k to 125k messages): queries={}, p50={:?}, p95={:?}, max={:?}",
            concurrent.len(),
            concurrent[10],
            concurrent[18],
            concurrent[19]
        );
    });
}

const LARGE_FOLDER: u32 = 100_000;

/// One ordinary folder, large enough to show any stall in folder management.
#[derive(Clone)]
struct LargeFolder(Arc<Mutex<Vec<String>>>);
impl MailConnector for LargeFolder {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        Ok(Box::new(self.clone()))
    }
}
impl MailSession for LargeFolder {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|name| RemoteFolder {
                name: name.clone(),
                kind: if name == "INBOX" { "inbox" } else { "other" }.into(),
                delimiter: Some("/".into()),
            })
            .collect())
    }
    fn snapshot(&mut self, folder: &str) -> Result<FolderSnapshot, MailError> {
        let total = if folder == "INBOX" { 0 } else { LARGE_FOLDER };
        Ok(FolderSnapshot {
            uid_validity: 1,
            messages: (1..=total)
                .map(|uid| RemoteFlag { uid, unread: false })
                .collect(),
        })
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        CorpusSession.fetch_messages(uids)
    }
    fn rename_folder(&mut self, from: &str, to: &str) -> Result<(), MailError> {
        let mut names = self.0.lock().unwrap();
        *names.iter_mut().find(|name| *name == from).unwrap() = to.into();
        Ok(())
    }
    fn delete_folder(&mut self, name: &str) -> Result<(), MailError> {
        self.0.lock().unwrap().retain(|folder| folder != name);
        Ok(())
    }
}

/// Runs `change`, returning its duration and the longest a concurrent reader
/// waited, polling as the sidebar would. The second is what a person feels.
fn timed(engine: &MailEngine, change: impl FnOnce() + Send) -> (Duration, Duration) {
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut longest = Duration::ZERO;
            while !done.load(Ordering::SeqCst) {
                let start = Instant::now();
                engine.folders().unwrap();
                longest = longest.max(start.elapsed());
                std::thread::sleep(Duration::from_millis(5));
            }
            longest
        });
        let start = Instant::now();
        change();
        let total = start.elapsed();
        done.store(true, Ordering::SeqCst);
        (total, reader.join().unwrap())
    })
}

#[test]
#[ignore = "explicit 100k-message performance gate; run with --ignored --nocapture"]
fn managing_a_100k_message_folder_leaves_other_views_responsive() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(LargeFolder(Arc::new(Mutex::new(vec![
            "INBOX".into(),
            "Large".into(),
        ])))),
    );
    let account = engine
        .add_account(
            AccountConfig {
                managed: false,
                name: "Mailbox".into(),
                address: "mailbox@example.test".into(),
                host: "imap.example.test".into(),
                port: 993,
                username: "mailbox@example.test".into(),
                security: TransportSecurity::Tls,
                ca_certificate: None,
                sending: Default::default(),
            },
            Zeroizing::new(b"benchmark-only".to_vec()),
        )
        .unwrap()
        .id;
    let start = Instant::now();
    engine.sync_account(&account).unwrap();
    println!("Indexed in {:?}", start.elapsed());
    let matches = |folder: &str| {
        engine
            .search(&SearchQuery {
                text: "coastal".into(),
                account_id: Some(account.clone()),
                folder: Some(folder.into()),
                ..Default::default()
            })
            .unwrap()
            .total
    };
    assert_eq!(matches("Large"), u64::from(LARGE_FOLDER));
    let folder = engine
        .folders()
        .unwrap()
        .into_iter()
        .find(|folder| folder.name == "Large")
        .unwrap()
        .id;

    let (idle, idle_wait) = timed(&engine, || std::thread::sleep(Duration::from_millis(250)));
    println!("Idle: {idle:?}, longest read {idle_wait:?}");

    let (total, rename_wait) = timed(&engine, || {
        engine
            .change_folder(
                &account,
                FolderChange::Rename {
                    folder_id: folder.clone(),
                    name: "Renamed".into(),
                },
            )
            .unwrap();
    });
    println!("Rename: {total:?}, longest read {rename_wait:?}");
    assert_eq!(matches("Renamed"), u64::from(LARGE_FOLDER));
    // Atomic by design, so readers wait for it, but only for index upkeep: a
    // rename that also rewrites search entries takes about twice as long.
    assert!(rename_wait < Duration::from_secs(1), "{rename_wait:?}");

    let (total, delete_wait) = timed(&engine, || {
        engine
            .change_folder(
                &account,
                FolderChange::Delete {
                    folder_id: folder.clone(),
                    confirmed: true,
                },
            )
            .unwrap();
    });
    println!("Delete: {total:?}, longest read {delete_wait:?}");
    assert_eq!(matches("Renamed"), 0);
    assert!(delete_wait < Duration::from_millis(250), "{delete_wait:?}");
}
