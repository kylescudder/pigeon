use std::sync::Arc;

use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, MailEngine, MailError, MessageContent, SearchQuery, TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use std::sync::Mutex;
use zeroize::Zeroizing;

#[derive(Clone)]
struct ChangingServer(Arc<Mutex<ServerState>>);
struct ServerState {
    validity: u32,
    count: u32,
    fail_after: Option<u32>,
    deleted: Vec<u32>,
    seen: Vec<u32>,
}
struct ChangingSession {
    state: Arc<Mutex<ServerState>>,
}
impl MailConnector for ChangingServer {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        Ok(Box::new(ChangingSession {
            state: self.0.clone(),
        }))
    }
}
impl MailSession for ChangingSession {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(vec![RemoteFolder {
            name: "INBOX".into(),
            kind: "inbox".into(),
        }])
    }
    fn snapshot(&mut self, _: &str) -> Result<FolderSnapshot, MailError> {
        let state = self.state.lock().unwrap();
        Ok(FolderSnapshot {
            uid_validity: state.validity,
            messages: (1..=state.count)
                .filter(|uid| !state.deleted.contains(uid))
                .map(|uid| RemoteFlag {
                    uid,
                    unread: !state.seen.contains(&uid),
                })
                .collect(),
        })
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        let state = self.state.lock().unwrap();
        if state.fail_after.is_some_and(|after| uids[0] > after) {
            return Err(MailError::Connection);
        }
        Ok(uids
            .iter()
            .map(|uid| RemoteMessage {
                uid: *uid,
                content: MessageContent {
                    sender: "Alice <alice@pigeon.test>".into(),
                    recipients: "mail@pigeon.test".into(),
                    subject: format!("Epoch {} invoice {uid}", state.validity),
                    body_text: "Quarterly cafe receipt".into(),
                    date: 1793005200,
                    ..Default::default()
                },
            })
            .collect())
    }
}

#[test]
fn interrupted_sync_keeps_cached_mail_and_resumes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let state = Arc::new(Mutex::new(ServerState {
        validity: 1,
        count: 250,
        fail_after: Some(100),
        deleted: vec![],
        seen: vec![],
    }));
    let path = dir.path().join("mail.db");
    let account;
    {
        let engine = MailEngine::with_connector(
            path.clone(),
            secrets.clone(),
            Arc::new(ChangingServer(state.clone())),
        );
        account = engine
            .add_account(config(), Zeroizing::new(b"test-password".to_vec()))
            .unwrap();
        assert!(matches!(
            engine.sync_account(&account.id),
            Err(MailError::Connection)
        ));
        assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 100);
        assert_eq!(engine.accounts().unwrap()[0].status, "error");
    }
    state.lock().unwrap().fail_after = None;
    let engine = MailEngine::with_connector(
        path,
        secrets.clone(),
        Arc::new(ChangingServer(state.clone())),
    );
    engine.sync_account(&account.id).unwrap();
    assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 250);
    state.lock().unwrap().deleted = vec![1, 2];
    state.lock().unwrap().seen = vec![3];
    engine.sync_account(&account.id).unwrap();
    assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 248);
    assert_eq!(
        engine
            .search(&SearchQuery {
                unread_only: true,
                ..Default::default()
            })
            .unwrap()
            .total,
        247
    );
    secrets.set_state(KeyringState::Locked);
    assert!(matches!(
        engine.search(&SearchQuery::default()),
        Err(MailError::Locked)
    ));
    secrets.set_state(KeyringState::Unlocked);
    assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 248);
}

#[test]
fn uid_validity_reset_keeps_old_cache_until_replacement_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(ServerState {
        validity: 1,
        count: 3,
        fail_after: None,
        deleted: vec![],
        seen: vec![],
    }));
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(ChangingServer(state.clone())),
    );
    let account = engine
        .add_account(config(), Zeroizing::new(b"test-password".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    {
        let mut changed = state.lock().unwrap();
        changed.validity = 2;
        changed.fail_after = Some(0);
    }
    assert!(matches!(
        engine.sync_account(&account.id),
        Err(MailError::Connection)
    ));
    assert_eq!(
        engine
            .search(&SearchQuery {
                text: "subject:\"Epoch 1\"".into(),
                ..Default::default()
            })
            .unwrap()
            .total,
        3
    );
    state.lock().unwrap().fail_after = None;
    engine.sync_account(&account.id).unwrap();
    assert_eq!(
        engine
            .search(&SearchQuery {
                text: "subject:\"Epoch 2\"".into(),
                ..Default::default()
            })
            .unwrap()
            .total,
        3
    );
    assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 3);
}

#[test]
fn search_combines_human_filters_with_text_and_saved_search_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let engine = MailEngine::with_connector(path.clone(), secrets.clone(), Arc::new(TestServer));
    let account = engine
        .add_account(config(), Zeroizing::new(b"test-password".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    let query = SearchQuery {
        text: "receipt from:alice account:Personal after:2026-10-01 is:unread".into(),
        ..Default::default()
    };
    assert_eq!(engine.search(&query).unwrap().total, 1);
    assert_eq!(
        engine
            .search(&SearchQuery {
                text: "receipt before:2026-01-01".into(),
                ..Default::default()
            })
            .unwrap()
            .total,
        0
    );
    let saved = engine.save_search("Invoices", &query).unwrap();
    drop(engine);
    let reopened = MailEngine::with_connector(path, secrets, Arc::new(TestServer));
    let saved_searches = reopened.saved_searches().unwrap();
    assert_eq!(saved_searches[0].id, saved.id);
    assert_eq!(reopened.search(&saved_searches[0].query).unwrap().total, 1);
    reopened.delete_saved_search(&saved.id).unwrap();
    assert!(reopened.saved_searches().unwrap().is_empty());
}

struct TestServer;
impl MailConnector for TestServer {
    fn connect(
        &self,
        _config: &AccountConfig,
        password: &Secret,
    ) -> Result<Box<dyn MailSession>, MailError> {
        if password.as_slice() != b"test-password" {
            return Err(MailError::Authentication);
        }
        Ok(Box::new(TestSession))
    }
}
struct TestSession;
impl MailSession for TestSession {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(vec![RemoteFolder {
            name: "INBOX".into(),
            kind: "inbox".into(),
        }])
    }
    fn snapshot(&mut self, _folder: &str) -> Result<FolderSnapshot, MailError> {
        Ok(FolderSnapshot {
            uid_validity: 44,
            messages: vec![RemoteFlag {
                uid: 7,
                unread: true,
            }],
        })
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        Ok(uids
            .iter()
            .map(|uid| RemoteMessage {
                uid: *uid,
                content: MessageContent {
                    sender: "Alice <alice@pigeon.test>".into(),
                    recipients: "mail@pigeon.test".into(),
                    subject: "Invoice 4471".into(),
                    body_text: "Cafe receipt for the building work".into(),
                    date: 1793005200,
                    ..Default::default()
                },
            })
            .collect())
    }
}

#[test]
fn synced_mail_is_searchable_without_duplicates_and_readable_after_restart_offline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    {
        let engine =
            MailEngine::with_connector(path.clone(), secrets.clone(), Arc::new(TestServer));
        let account = engine
            .add_account(config(), Zeroizing::new(b"test-password".to_vec()))
            .unwrap();
        engine.sync_account(&account.id).unwrap();
        engine.sync_account(&account.id).unwrap();
        let results = engine
            .search(&SearchQuery {
                text: "receipt".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.total, 1);
        assert_eq!(results.messages[0].subject, "Invoice 4471");
    }
    struct Offline;
    impl MailConnector for Offline {
        fn connect(
            &self,
            _: &AccountConfig,
            _: &Secret,
        ) -> Result<Box<dyn MailSession>, MailError> {
            Err(MailError::Connection)
        }
    }
    let engine = MailEngine::with_connector(path, secrets, Arc::new(Offline));
    let results = engine.search(&SearchQuery::default()).unwrap();
    assert_eq!(results.total, 1);
    let message = engine.message(results.messages[0].id).unwrap();
    assert_eq!(message.body_text, "Cafe receipt for the building work");
    assert!(message.summary.unread);
}

fn config() -> AccountConfig {
    AccountConfig {
        name: "Personal".into(),
        address: "mail@pigeon.test".into(),
        host: "imap.pigeon.test".into(),
        port: 993,
        username: "mail@pigeon.test".into(),
        security: TransportSecurity::Tls,
        ca_certificate: None,
    }
}

#[test]
fn authenticated_account_is_available_after_reopening_without_exposing_password() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let account = {
        let engine =
            MailEngine::with_connector(path.clone(), secrets.clone(), Arc::new(TestServer));
        engine
            .add_account(config(), Zeroizing::new(b"test-password".to_vec()))
            .unwrap()
    };
    let reopened = MailEngine::with_connector(path, secrets, Arc::new(TestServer));
    let accounts = reopened.accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, account.id);
    assert_eq!(accounts[0].config.address, "mail@pigeon.test");
    assert!(
        !serde_json::to_string(&accounts)
            .unwrap()
            .contains("test-password")
    );
}

#[test]
fn cancellation_before_a_queued_search_starts_is_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(TestServer),
    );
    let account = engine
        .add_account(config(), Zeroizing::new(b"test-password".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    engine.cancel_search("queued-request");
    assert!(matches!(
        engine.search_request("queued-request", &SearchQuery::default()),
        Err(MailError::Cancelled)
    ));
    assert_eq!(
        engine
            .search_request("next-request", &SearchQuery::default())
            .unwrap()
            .total,
        1
    );
}

#[test]
fn credential_reverification_and_local_removal_do_not_damage_cached_mail_on_failure() {
    use pigeon_core::secrets::SecretStore;
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        secrets.clone(),
        Arc::new(TestServer),
    );
    let account = engine
        .add_account(config(), Secret::new(b"test-password".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    let folder_id = engine.folders().unwrap()[0].id.clone();
    assert!(!folder_id.is_empty());
    engine.sync_account(&account.id).unwrap();
    assert_eq!(engine.folders().unwrap()[0].id, folder_id);
    assert!(matches!(
        engine.update_password(&account.id, Secret::new(b"incorrect".to_vec())),
        Err(MailError::Authentication)
    ));
    engine.sync_account(&account.id).unwrap();
    assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 1);
    engine
        .update_password(&account.id, Secret::new(b"test-password".to_vec()))
        .unwrap();
    engine.remove_account(&account.id).unwrap();
    assert!(engine.accounts().unwrap().is_empty());
    assert!(engine.folders().unwrap().is_empty());
    assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 0);
    assert!(
        secrets
            .get(&format!("account:{}:password", account.id))
            .unwrap()
            .is_none()
    );
}

#[test]
fn slow_network_does_not_block_cached_search_and_sync_can_be_cancelled() {
    use std::sync::{
        Condvar,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;
    struct SlowServer {
        pause: Arc<AtomicBool>,
        started: mpsc::Sender<()>,
        released: Arc<(Mutex<bool>, Condvar)>,
    }
    struct SlowSession {
        pause: Arc<AtomicBool>,
        started: mpsc::Sender<()>,
        released: Arc<(Mutex<bool>, Condvar)>,
    }
    impl MailConnector for SlowServer {
        fn connect(
            &self,
            _: &AccountConfig,
            _: &Secret,
        ) -> Result<Box<dyn MailSession>, MailError> {
            Ok(Box::new(SlowSession {
                pause: self.pause.clone(),
                started: self.started.clone(),
                released: self.released.clone(),
            }))
        }
    }
    impl MailSession for SlowSession {
        fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
            TestSession.folders()
        }
        fn snapshot(&mut self, folder: &str) -> Result<FolderSnapshot, MailError> {
            if self.pause.load(Ordering::SeqCst) {
                self.started.send(()).unwrap();
                let (lock, changed) = &*self.released;
                let (released, timeout) = changed
                    .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |released| {
                        !*released
                    })
                    .unwrap();
                assert!(
                    *released && !timeout.timed_out(),
                    "test failed to release synthetic network"
                );
            }
            TestSession.snapshot(folder)
        }
        fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
            TestSession.fetch_messages(uids)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let pause = Arc::new(AtomicBool::new(false));
    let (started, wait) = mpsc::channel();
    let released = Arc::new((Mutex::new(false), Condvar::new()));
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(SlowServer {
            pause: pause.clone(),
            started,
            released: released.clone(),
        }),
    );
    let account = engine
        .add_account(config(), Secret::new(b"test-password".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    pause.store(true, Ordering::SeqCst);
    std::thread::scope(|scope| {
        let syncing = scope.spawn(|| engine.sync_account(&account.id));
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 1);
        assert!(matches!(
            engine.sync_account(&account.id),
            Err(MailError::Busy)
        ));
        assert!(matches!(
            engine.remove_account(&account.id),
            Err(MailError::Busy)
        ));
        engine.cancel_sync(&account.id);
        *released.0.lock().unwrap() = true;
        released.1.notify_all();
        assert!(matches!(syncing.join().unwrap(), Err(MailError::Cancelled)));
    });
    assert_eq!(engine.accounts().unwrap()[0].status, "cancelled");
    assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 1);
}

#[test]
fn malformed_queries_are_actionable_and_punctuation_never_becomes_sql() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(TestServer),
    );
    let account = engine
        .add_account(config(), Secret::new(b"test-password".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    for text in [
        "\"unfinished",
        "is:banana",
        "after:Monday",
        "has:virus",
        "receipt OR",
        "from:",
    ] {
        assert!(
            matches!(
                engine.search(&SearchQuery {
                    text: text.into(),
                    ..Default::default()
                }),
                Err(MailError::InvalidSearch(_))
            ),
            "{text}"
        );
    }
    for text in [
        "https://example.test",
        "from:%",
        "subject:' OR 1=1 --",
        "🕊️",
        "((((",
        "))))",
        "café",
    ] {
        let result = engine.search(&SearchQuery {
            text: text.into(),
            ..Default::default()
        });
        assert!(
            !matches!(result, Err(MailError::Storage(_))),
            "{text}: {result:?}"
        );
    }
    assert_eq!(
        engine
            .search(&SearchQuery {
                text: "receipt".into(),
                ..Default::default()
            })
            .unwrap()
            .total,
        1
    );
}
