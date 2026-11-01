use pigeon_core::{
    mail::source::*,
    mail::{
        AccountConfig, ActionKind, FolderChange, MailEngine, MailError, SearchQuery,
        TransportSecurity,
    },
    secrets::{KeyringState, MemoryStore, Secret},
};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct State {
    names: Vec<String>,
    offline: bool,
    lose_reply: bool,
    lose_connection: bool,
    refuse: bool,
}
#[derive(Clone)]
struct Server(Arc<Mutex<State>>);
impl MailConnector for Server {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        if self.0.lock().unwrap().offline {
            return Err(MailError::Connection);
        }
        Ok(Box::new(self.clone()))
    }
}
impl MailSession for Server {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        let s = self.0.lock().unwrap();
        if s.offline {
            return Err(MailError::Connection);
        }
        Ok(s.names
            .iter()
            .map(|n| RemoteFolder {
                name: n.clone(),
                kind: if n == "INBOX" { "inbox" } else { "other" }.into(),
                delimiter: Some("/".into()),
            })
            .collect())
    }
    fn snapshot(&mut self, _: &str) -> Result<FolderSnapshot, MailError> {
        Ok(FolderSnapshot {
            uid_validity: 1,
            messages: vec![RemoteFlag {
                uid: 1,
                unread: true,
            }],
        })
    }
    fn fetch_messages(&mut self, _: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        Ok(vec![RemoteMessage {
            uid: 1,
            content: pigeon_core::mail::MessageContent {
                subject: "cached receipt".into(),
                ..Default::default()
            },
        }])
    }
    fn create_folder(&mut self, name: &str) -> Result<(), MailError> {
        self.0.lock().unwrap().names.push(name.into());
        Ok(())
    }
    fn rename_folder(&mut self, from: &str, to: &str) -> Result<(), MailError> {
        let mut s = self.0.lock().unwrap();
        if s.refuse {
            return Err(MailError::Imap {
                operation: "renaming a folder",
                reason: "the server rejected the command (IMAP NO)",
            });
        }
        *s.names.iter_mut().find(|n| *n == from).unwrap() = to.into();
        s.offline = s.lose_connection;
        if s.lose_reply || s.lose_connection {
            Err(MailError::Connection)
        } else {
            Ok(())
        }
    }
    fn delete_folder(&mut self, name: &str) -> Result<(), MailError> {
        self.0.lock().unwrap().names.retain(|n| n != name);
        Ok(())
    }
}
fn config() -> AccountConfig {
    AccountConfig {
        name: "test".into(),
        address: "a@example.test".into(),
        host: "imap.example.test".into(),
        port: 993,
        username: "a".into(),
        security: TransportSecurity::Tls,
        ca_certificate: None,
        sending: Default::default(),
        managed: false,
    }
}
#[test]
fn server_confirmed_folder_lifecycle_and_interrupted_rename_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let state = Arc::new(Mutex::new(State {
        names: vec!["INBOX".into()],
        ..Default::default()
    }));
    let connector = Arc::new(Server(state.clone()));
    let engine = MailEngine::with_connector(path.clone(), secrets.clone(), connector.clone());
    let a = engine
        .add_account(config(), Secret::new(b"test".to_vec()))
        .unwrap();
    engine.sync_account(&a.id).unwrap();
    for name in [
        "",
        " INBOX",
        "a\r\nb",
        "a//b",
        "INBOX",
        "caf\u{e9}",
        "a*",
        "a%",
    ] {
        assert!(
            engine
                .change_folder(&a.id, FolderChange::Create { name: name.into() })
                .is_err()
        );
    }
    engine
        .change_folder(
            &a.id,
            FolderChange::Create {
                name: "Projects".into(),
            },
        )
        .unwrap();
    engine.sync_account(&a.id).unwrap();
    let folder = engine
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.name == "Projects")
        .unwrap();
    let message = engine
        .search(&SearchQuery {
            account_id: Some(a.id.clone()),
            folder: Some(folder.name.clone()),
            ..Default::default()
        })
        .unwrap()
        .messages[0]
        .clone();
    let other = engine
        .add_account(config(), Secret::new(b"test".to_vec()))
        .unwrap();
    assert!(
        engine
            .change_folder(
                &other.id,
                FolderChange::Delete {
                    folder_id: folder.id.clone(),
                    confirmed: true
                }
            )
            .is_err()
    );
    state.lock().unwrap().offline = true;
    assert!(
        engine
            .change_folder(
                &a.id,
                FolderChange::Create {
                    name: "Offline".into()
                }
            )
            .is_err()
    );
    assert!(
        !engine
            .folders()
            .unwrap()
            .iter()
            .any(|f| f.name == "Offline")
    );
    state.lock().unwrap().offline = false;
    engine
        .queue_actions(&[message.id], ActionKind::MarkRead)
        .unwrap();
    assert!(
        engine
            .change_folder(
                &a.id,
                FolderChange::Rename {
                    folder_id: folder.id.clone(),
                    name: "Renamed".into()
                }
            )
            .is_err()
    );
    let action = engine.pending_actions().unwrap()[0].id;
    engine.discard_action(action).unwrap();
    state.lock().unwrap().lose_connection = true;
    assert!(
        engine
            .change_folder(
                &a.id,
                FolderChange::Rename {
                    folder_id: folder.id.clone(),
                    name: "Renamed".into()
                }
            )
            .is_err()
    );
    assert!(
        engine
            .queue_actions(&[message.id], ActionKind::MarkRead)
            .is_err()
    );
    drop(engine);
    state.lock().unwrap().offline = false;
    let engine = MailEngine::with_connector(path, secrets, connector);
    engine.sync_account(&a.id).unwrap();
    let renamed = engine
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.id == folder.id)
        .unwrap();
    assert_eq!(renamed.name, "Renamed");
    assert_eq!(
        engine.message(message.id).unwrap().summary.folder,
        "Renamed"
    );
    assert!(
        engine
            .change_folder(
                &a.id,
                FolderChange::Delete {
                    folder_id: folder.id.clone(),
                    confirmed: false
                }
            )
            .is_err()
    );
    engine
        .change_folder(
            &a.id,
            FolderChange::Delete {
                folder_id: folder.id.clone(),
                confirmed: true,
            },
        )
        .unwrap();
    assert!(engine.message(message.id).is_err());
    assert!(!engine.folders().unwrap().iter().any(|f| f.id == folder.id));
    assert!(
        engine
            .search(&SearchQuery {
                account_id: Some(a.id.clone()),
                folder: Some("Renamed".into()),
                ..Default::default()
            })
            .unwrap()
            .messages
            .is_empty()
    );
    let inbox = engine
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.account_id == a.id && f.kind == "inbox")
        .unwrap();
    assert!(
        engine
            .change_folder(
                &a.id,
                FolderChange::Delete {
                    folder_id: inbox.id,
                    confirmed: true
                }
            )
            .is_err()
    );
    state
        .lock()
        .unwrap()
        .names
        .extend(["Parent".into(), "Parent/Child".into()]);
    engine.sync_account(&a.id).unwrap();
    let parent = engine
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.account_id == a.id && f.name == "Parent")
        .unwrap();
    assert!(
        engine
            .change_folder(
                &a.id,
                FolderChange::Delete {
                    folder_id: parent.id.clone(),
                    confirmed: true
                }
            )
            .is_err()
    );
    assert!(
        engine
            .change_folder(
                &a.id,
                FolderChange::Rename {
                    folder_id: parent.id,
                    name: "ChangedParent".into()
                }
            )
            .is_err()
    );
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: Arc<Mutex<State>>,
    engine: MailEngine,
    account: String,
}

fn synced(names: &[&str]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(State {
        names: names.iter().map(|n| n.to_string()).collect(),
        ..Default::default()
    }));
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(Server(state.clone())),
    );
    let account = engine
        .add_account(config(), Secret::new(b"test".to_vec()))
        .unwrap()
        .id;
    engine.sync_account(&account).unwrap();
    Fixture {
        _dir: dir,
        state,
        engine,
        account,
    }
}

impl Fixture {
    fn folder(&self, name: &str) -> pigeon_core::mail::Folder {
        self.engine
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.account_id == self.account && f.name == name)
            .unwrap()
    }
    fn query(&self, folder: &str) -> SearchQuery {
        SearchQuery {
            account_id: Some(self.account.clone()),
            folder: Some(folder.into()),
            ..Default::default()
        }
    }
    fn message(&self, folder: &str) -> i64 {
        self.engine.search(&self.query(folder)).unwrap().messages[0].id
    }
    fn rename(&self, from: &str, to: &str) -> Result<(), MailError> {
        self.engine.change_folder(
            &self.account,
            FolderChange::Rename {
                folder_id: self.folder(from).id,
                name: to.into(),
            },
        )
    }
}

#[test]
fn a_refused_change_reports_the_server_and_leaves_mail_usable() {
    let f = synced(&["INBOX", "Projects"]);
    f.state.lock().unwrap().refuse = true;
    assert!(matches!(
        f.rename("Projects", "Research"),
        Err(MailError::Imap { .. })
    ));
    assert_eq!(f.folder("Projects").kind, "other");
    let account = f.engine.accounts().unwrap().remove(0);
    assert_eq!(account.status, "idle");
    assert_eq!(account.error, None);
    f.engine
        .queue_actions(&[f.message("Projects")], ActionKind::MarkRead)
        .unwrap();
}

#[test]
fn a_lost_reply_counts_as_done_once_the_folder_list_shows_it() {
    let f = synced(&["INBOX", "Projects"]);
    f.state.lock().unwrap().lose_reply = true;
    f.rename("Projects", "Research").unwrap();
    assert_eq!(f.folder("Research").kind, "other");
    f.engine
        .queue_actions(&[f.message("Research")], ActionKind::MarkRead)
        .unwrap();
}

#[test]
fn a_rename_replaces_a_stale_cached_folder_and_keeps_saved_searches() {
    let f = synced(&["INBOX", "Projects", "Removed"]);
    let id = f.folder("Projects").id;
    let message = f.message("Projects");
    let saved = f
        .engine
        .save_search("Projects mail", &f.query("Projects"))
        .unwrap();
    // Another client removed it; this cache has not noticed yet.
    f.state.lock().unwrap().names.retain(|n| n != "Removed");
    f.rename("Projects", "Removed").unwrap();
    f.engine.sync_account(&f.account).unwrap();
    assert_eq!(f.folder("Removed").id, id);
    assert_eq!(f.message("Removed"), message);
    // One entry per message: the stale copy's is gone and the moved one's kept.
    let everywhere = |text: &str| {
        f.engine
            .search(&SearchQuery {
                text: text.into(),
                account_id: Some(f.account.clone()),
                include_junk_trash: true,
                ..Default::default()
            })
            .unwrap()
    };
    let found = everywhere("cached");
    assert_eq!(
        found
            .messages
            .iter()
            .filter(|m| m.folder == "Removed")
            .count(),
        1
    );
    assert_eq!(
        found
            .messages
            .iter()
            .filter(|m| m.folder == "Projects")
            .count(),
        0
    );
    let saved = f
        .engine
        .saved_searches()
        .unwrap()
        .into_iter()
        .find(|s| s.id == saved.id)
        .unwrap();
    assert_eq!(saved.query.folder.as_deref(), Some("Removed"));
}

#[test]
fn both_names_after_a_lost_reply_settle_without_pausing_the_account() {
    let f = synced(&["INBOX", "Projects"]);
    f.state.lock().unwrap().lose_connection = true;
    assert!(f.rename("Projects", "Research").is_err());
    assert!(
        f.engine
            .queue_actions(&[f.message("Projects")], ActionKind::MarkRead)
            .is_err()
    );
    {
        // Another client recreated the old name before this device reconnected.
        let mut server = f.state.lock().unwrap();
        server.offline = false;
        server.names.push("Projects".into());
    }
    f.engine.sync_account(&f.account).unwrap();
    assert_eq!(f.folder("Projects").kind, "other");
    assert_eq!(f.folder("Research").kind, "other");
    f.engine
        .queue_actions(&[f.message("Research")], ActionKind::MarkRead)
        .unwrap();
}

#[test]
fn local_checks_report_the_same_offline() {
    let f = synced(&["INBOX", "Projects"]);
    f.state.lock().unwrap().offline = true;
    assert!(matches!(
        f.engine
            .change_folder(&f.account, FolderChange::Create { name: "a&b".into() }),
        Err(MailError::InvalidAction(_))
    ));
    assert!(matches!(
        f.engine.change_folder(
            &f.account,
            FolderChange::Delete {
                folder_id: f.folder("Projects").id,
                confirmed: false
            }
        ),
        Err(MailError::InvalidAction(_))
    ));
}
