//! Upgrading a schema-8 store (main before sending) to schema 9 keeps every
//! kind of local-only work: queued operations, kept attachments, queued
//! mailbox actions and cached mail. Drafts and the outbox work afterwards.

use std::sync::{Arc, Mutex};

use pigeon_core::mail::operations::{NewOperation, OperationState};
use pigeon_core::mail::smtp::{Envelope, SmtpConfig, SmtpServer, SmtpTransport, Submission};
use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, ActionKind, DraftInput, MailEngine, MailError, MessageContent, SearchQuery,
    SendStatus, SendingSettings, SentCopyStatus, TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use pigeon_core::unlock::{UnlockOutcome, open_store};
use zeroize::Zeroizing;

#[derive(Default)]
struct Server {
    appended: Mutex<Vec<String>>,
    submitted: Mutex<usize>,
}
struct Connector(Arc<Server>);
struct Session(Arc<Server>);
struct Smtp(Arc<Server>);

impl MailConnector for Connector {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        Ok(Box::new(Session(self.0.clone())))
    }
}

impl MailSession for Session {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(["INBOX", "Sent"]
            .map(|name| RemoteFolder {
                name: name.into(),
                kind: name.to_lowercase(),
                delimiter: Some("/".into()),
            })
            .to_vec())
    }
    fn snapshot(&mut self, folder: &str) -> Result<FolderSnapshot, MailError> {
        Ok(FolderSnapshot {
            uid_validity: 7,
            messages: if folder == "INBOX" {
                vec![RemoteFlag {
                    uid: 1,
                    unread: true,
                }]
            } else {
                Vec::new()
            },
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
                    subject: "Upgrade canary".into(),
                    body_text: "Kept across the schema upgrade".into(),
                    date: 1793005200,
                    ..Default::default()
                },
            })
            .collect())
    }
    fn append(&mut self, folder: &str, _: &[u8]) -> Result<(), MailError> {
        self.0.appended.lock().unwrap().push(folder.to_owned());
        Ok(())
    }
}

impl SmtpTransport for Smtp {
    fn verify(&self, _: &SmtpServer<'_>, _: &Secret) -> Result<(), MailError> {
        Ok(())
    }
    fn submit(
        &self,
        _: &SmtpServer<'_>,
        _: &Secret,
        _: &Envelope,
        _: &[u8],
        commit: &mut dyn FnMut() -> Result<(), MailError>,
    ) -> Submission {
        commit().unwrap();
        *self.0.submitted.lock().unwrap() += 1;
        Submission::Accepted
    }
}

fn engine(path: &std::path::Path, secrets: &Arc<MemoryStore>, server: &Arc<Server>) -> MailEngine {
    MailEngine::with_transports(
        path.to_owned(),
        secrets.clone(),
        Arc::new(Connector(server.clone())),
        Arc::new(Smtp(server.clone())),
    )
}

fn version(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn schema_8_store_upgrades_keeping_queued_work_attachments_actions_and_mail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let server = Arc::new(Server::default());

    // Local-only work of every kind main supports at schema 8.
    let (account, message, operation) = {
        let engine = engine(&path, &secrets, &server);
        let account = engine
            .add_account(
                AccountConfig {
                    name: "Upgrade".into(),
                    address: "mail@pigeon.test".into(),
                    host: "imap.pigeon.test".into(),
                    port: 993,
                    username: "mail@pigeon.test".into(),
                    security: TransportSecurity::Tls,
                    ca_certificate: None,
                    sending: SendingSettings {
                        smtp: Some(SmtpConfig {
                            host: "smtp.pigeon.test".into(),
                            port: 465,
                            security: TransportSecurity::Tls,
                            username: "mail@pigeon.test".into(),
                        }),
                        ..Default::default()
                    },
                },
                Zeroizing::new(b"test-password".to_vec()),
            )
            .unwrap()
            .id;
        engine.sync_account(&account).unwrap();
        let message = engine.search(&SearchQuery::default()).unwrap().messages[0].id;
        assert_eq!(
            engine
                .queue_actions(&[message], ActionKind::MarkRead)
                .unwrap(),
            1
        );
        let operation = engine
            .enqueue_operation(
                NewOperation::new("mailbox-future-kind", serde_json::json!({ "keep": true }))
                    .account(&account),
            )
            .unwrap()
            .id;
        (account, message, operation)
    };

    // Turn it into exactly the schema-8 layout: migration 9 only adds the
    // drafts and outbox tables. Keep an attachment the way main stores one.
    {
        let UnlockOutcome::Opened { store, .. } = open_store(secrets.as_ref(), &path) else {
            panic!("store should open");
        };
        let conn = store.connection();
        assert_eq!(version(conn), 9);
        conn.execute_batch("DROP TABLE outbox; DROP TABLE drafts; PRAGMA user_version = 8;")
            .unwrap();
        conn.execute(
            "INSERT INTO attachment_blobs(message,part,content_type,data,stored_at) VALUES (?1,'2','application/pdf',?2,1)",
            rusqlite::params![message, vec![7u8; 4096]],
        )
        .unwrap();
        assert_eq!(version(conn), 8);
    }

    let engine = engine(&path, &secrets, &server);
    // Opening migrates; everything from schema 8 is still there.
    assert!(engine.send_state().unwrap().drafts.is_empty());
    assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 1);
    assert_eq!(engine.stored_attachment_bytes().unwrap(), 4096);
    let actions = engine.pending_actions().unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(
        engine.operation(&operation).unwrap().state,
        OperationState::Pending
    );

    // Drafts and the outbox work on the upgraded store, alongside the
    // queued mailbox change and the unrelated queued operation.
    let draft = DraftInput {
        account_id: Some(account.clone()),
        to: "friend@pigeon.test".into(),
        subject: "After the upgrade".into(),
        body: "Still sending.".into(),
        ..Default::default()
    };
    let saved = engine.save_draft(draft.clone()).unwrap();
    engine.set_undo_send_seconds(0).unwrap();
    let queued = engine
        .send_draft(DraftInput {
            id: Some(saved.id),
            ..draft
        })
        .unwrap();
    assert!(engine.send_state().unwrap().drafts.is_empty());
    while engine.run_next_operation().unwrap() {}
    let item = engine
        .send_state()
        .unwrap()
        .outbox
        .into_iter()
        .find(|item| item.id == queued.id)
        .unwrap();
    assert!(matches!(item.send, SendStatus::Accepted { .. }), "{item:?}");
    assert_eq!(item.sent_copy, SentCopyStatus::Saved);
    assert_eq!(*server.submitted.lock().unwrap(), 1);
    assert_eq!(*server.appended.lock().unwrap(), ["Sent"]);
    assert_eq!(engine.pending_actions().unwrap().len(), 1);
    assert_eq!(
        engine.operation(&operation).unwrap().state,
        OperationState::Pending
    );
    drop(engine);

    let UnlockOutcome::Opened { store, .. } = open_store(secrets.as_ref(), &path) else {
        panic!("store should open");
    };
    assert_eq!(version(store.connection()), 9);
}
