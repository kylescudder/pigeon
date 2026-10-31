//! Finding the mail a sync just brought in, for desktop notifications.

use std::sync::{Arc, Mutex};

use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{AccountConfig, MailEngine, MailError, MessageContent, TransportSecurity};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use zeroize::Zeroizing;

/// (folder, kind) -> messages as (uid, unread).
type Folders = Vec<(&'static str, &'static str, Vec<(u32, bool)>)>;

struct Server(Arc<Mutex<Folders>>);
impl MailConnector for Server {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        Ok(Box::new(Session(self.0.clone())))
    }
}
struct Session(Arc<Mutex<Folders>>);
impl MailSession for Session {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(name, kind, _)| RemoteFolder {
                name: (*name).into(),
                kind: (*kind).into(),
                delimiter: Some("/".into()),
            })
            .collect())
    }
    fn snapshot(&mut self, folder: &str) -> Result<FolderSnapshot, MailError> {
        let folders = self.0.lock().unwrap();
        let (_, _, messages) = folders.iter().find(|(name, _, _)| *name == folder).unwrap();
        Ok(FolderSnapshot {
            uid_validity: 1,
            messages: messages
                .iter()
                .map(|(uid, unread)| RemoteFlag {
                    uid: *uid,
                    unread: *unread,
                })
                .collect(),
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
                    subject: format!("Message {uid}"),
                    body_text: "text".into(),
                    date: 1793005200 + i64::from(*uid),
                    ..Default::default()
                },
            })
            .collect())
    }
}

fn config() -> AccountConfig {
    AccountConfig {
        managed: false,
        name: "Personal".into(),
        address: "mail@pigeon.test".into(),
        host: "imap.pigeon.test".into(),
        port: 993,
        username: "mail@pigeon.test".into(),
        security: TransportSecurity::Tls,
        ca_certificate: None,
        sending: Default::default(),
    }
}

#[test]
fn only_new_unread_inbox_mail_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let folders: Arc<Mutex<Folders>> = Arc::new(Mutex::new(vec![
        ("INBOX", "inbox", vec![(1, true), (2, false)]),
        ("Junk", "junk", vec![(1, true)]),
        ("Archive", "archive", vec![(1, true)]),
    ]));
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(Server(folders.clone())),
    );
    let account = engine
        .add_account(config(), Zeroizing::new(b"pw".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    // The first sync's unread Inbox mail counts (the shell discards it as backlog)
    // and taking it clears it.
    assert_eq!(engine.take_arrivals(&account.id).unwrap().total, 1);
    assert_eq!(engine.take_arrivals(&account.id).unwrap().total, 0);

    {
        let mut folders = folders.lock().unwrap();
        folders[0].2.extend([(3, true), (4, false), (5, true)]);
        folders[1].2.push((2, true));
        folders[2].2.push((2, true));
    }
    engine.sync_account(&account.id).unwrap();
    let arrived = engine.take_arrivals(&account.id).unwrap();
    let subjects: Vec<_> = arrived.newest.iter().map(|m| m.subject.as_str()).collect();
    // Newest first; the read message and the Junk and Archive arrivals are out.
    assert_eq!(subjects, ["Message 5", "Message 3"]);
    assert_eq!(arrived.total, 2);
    assert!(
        arrived
            .newest
            .iter()
            .all(|m| m.unread && m.folder == "INBOX")
    );
    assert_eq!(engine.take_arrivals("someone-else").unwrap().total, 0);
}

#[test]
fn a_reused_row_id_does_not_hide_a_new_message() {
    let dir = tempfile::tempdir().unwrap();
    let folders: Arc<Mutex<Folders>> = Arc::new(Mutex::new(vec![(
        "INBOX",
        "inbox",
        vec![(1, false), (2, false)],
    )]));
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(Server(folders.clone())),
    );
    let account = engine
        .add_account(config(), Zeroizing::new(b"pw".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    engine.take_arrivals(&account.id).unwrap();
    // The newest stored message goes away and a new one arrives in the same
    // sync: SQLite hands the new message the freed row id.
    folders.lock().unwrap()[0].2 = vec![(1, false), (3, true)];
    engine.sync_account(&account.id).unwrap();
    let arrived = engine.take_arrivals(&account.id).unwrap();
    assert_eq!(arrived.total, 1);
    assert_eq!(arrived.newest[0].subject, "Message 3");
}
