//! Replying to and forwarding a stored message.

use std::sync::Arc;

use pigeon_core::mail::reply::ReplyKind;
use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, MailEngine, MailError, MessageContent, SearchQuery, TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use zeroize::Zeroizing;

struct Server;
impl MailConnector for Server {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        Ok(Box::new(Session))
    }
}
struct Session;
impl MailSession for Session {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(vec![RemoteFolder {
            name: "INBOX".into(),
            kind: "inbox".into(),
            delimiter: Some("/".into()),
        }])
    }
    fn snapshot(&mut self, _: &str) -> Result<FolderSnapshot, MailError> {
        Ok(FolderSnapshot {
            uid_validity: 1,
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
                    sender: "Alice Example <alice@pigeon.test>".into(),
                    // Sync stores Bcc in `recipients`; replies must not use it.
                    recipients: "Me <mail@pigeon.test>, Bob <bob@pigeon.test>, secret@pigeon.test"
                        .into(),
                    to_list: "Me <mail@pigeon.test>, Bob <bob@pigeon.test>".into(),
                    references: "<root@pigeon.test>".into(),
                    subject: "Quarterly receipts".into(),
                    body_text: "Please find the receipts.".into(),
                    message_id: "<orig-1@pigeon.test>".into(),
                    date: 1793005200,
                    ..Default::default()
                },
            })
            .collect())
    }
}

fn engine() -> (tempfile::TempDir, MailEngine, i64) {
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::with_connector(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(Server),
    );
    let account = engine
        .add_account(
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
            },
            Zeroizing::new(b"pw".to_vec()),
        )
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    let id = engine.search(&SearchQuery::default()).unwrap().messages[0].id;
    (dir, engine, id)
}

#[test]
fn a_reply_is_saved_as_a_threaded_draft_from_the_receiving_account() {
    let (_dir, engine, message) = engine();
    let draft = engine.start_reply(message, ReplyKind::ReplyAll).unwrap();
    assert_eq!(draft.to, "Alice Example <alice@pigeon.test>");
    assert_eq!(draft.cc, "\"Bob\" <bob@pigeon.test>");
    assert_eq!(draft.subject, "Re: Quarterly receipts");
    assert_eq!(draft.in_reply_to, "orig-1@pigeon.test");
    assert_eq!(draft.references, "root@pigeon.test orig-1@pigeon.test");
    assert!(!format!("{draft:?}").contains("secret"));
    assert!(draft.body.contains("> Please find the receipts."));
    let accounts = engine.accounts().unwrap();
    assert_eq!(draft.account_id.as_deref(), Some(accounts[0].id.as_str()));
    // It is an ordinary draft the composer can open, and keeps its threading.
    let saved = engine.send_state().unwrap().drafts;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, draft.id);
    assert_eq!(saved[0].in_reply_to, "orig-1@pigeon.test");
}

#[test]
fn a_forward_starts_a_new_conversation() {
    let (_dir, engine, message) = engine();
    let draft = engine.start_reply(message, ReplyKind::Forward).unwrap();
    assert_eq!(draft.subject, "Fwd: Quarterly receipts");
    assert!(draft.to.is_empty() && draft.in_reply_to.is_empty());
    assert!(draft.body.contains("Please find the receipts."));
}

#[test]
fn replying_to_a_message_that_is_gone_fails_cleanly() {
    let (_dir, engine, message) = engine();
    assert!(matches!(
        engine.start_reply(message + 1000, ReplyKind::Reply),
        Err(MailError::NotFound)
    ));
    assert!(engine.send_state().unwrap().drafts.is_empty());
}
