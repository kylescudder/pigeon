//! Explicit performance gate. Uses the public engine and a synthetic external
//! source; real IMAP transport is verified separately against Dovecot.
use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, Attachment, MailEngine, MailError, MessageContent, SearchQuery,
    TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use std::sync::Arc;
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
            },
            RemoteFolder {
                name: "Archive".into(),
                kind: "archive".into(),
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
                    name: format!("Mailbox {index}"),
                    address: format!("mailbox{index}@example.test"),
                    host: "imap.example.test".into(),
                    port: 993,
                    username: format!("mailbox{index}@example.test"),
                    security: TransportSecurity::Tls,
                    ca_certificate: None,
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
                name: "Indexing account".into(),
                address: "indexing@example.test".into(),
                host: "imap.example.test".into(),
                port: 993,
                username: "indexing@example.test".into(),
                security: TransportSecurity::Tls,
                ca_certificate: None,
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
