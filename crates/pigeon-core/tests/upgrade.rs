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
                    managed: false,
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
        assert_eq!(version(conn), pigeon_core::store::SCHEMA_VERSION as i64);
        conn.execute_batch(
            "DROP TABLE recipient_history; DROP TABLE outbox; DROP TABLE drafts; DROP TABLE store_meta; ALTER TABLE messages DROP COLUMN reply_to; ALTER TABLE messages DROP COLUMN reply_refs; ALTER TABLE messages DROP COLUMN to_list; ALTER TABLE messages DROP COLUMN cc_list; PRAGMA user_version = 8;",
        )
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
    engine
        .send_draft(DraftInput {
            id: Some(saved.id),
            ..draft
        })
        .unwrap();
    assert!(engine.send_state().unwrap().drafts.is_empty());
    while engine.run_next_operation().unwrap() {}
    assert!(engine.send_state().unwrap().outbox.is_empty());
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
    assert_eq!(
        version(store.connection()),
        pigeon_core::store::SCHEMA_VERSION as i64
    );
}

#[test]
fn restore_keeps_real_drafts_outbox_and_mailbox_work_but_never_runs_them() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let server = Arc::new(Server::default());
    let source = engine(&dir.path().join("source.db"), &secrets, &server);
    let account = source
        .add_account(
            AccountConfig {
                managed: false,
                name: "Recovery".into(),
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
    source.sync_account(&account).unwrap();
    let message = source.search(&SearchQuery::default()).unwrap().messages[0].id;
    source
        .queue_actions(&[message], ActionKind::MarkRead)
        .unwrap();
    let draft = DraftInput {
        account_id: Some(account.clone()),
        to: "friend@pigeon.test".into(),
        subject: "Keep my draft".into(),
        body: "Draft canary".into(),
        ..Default::default()
    };
    let saved = source.save_draft(draft.clone()).unwrap();
    source.set_undo_send_seconds(60).unwrap();
    let queued = source
        .send_draft(DraftInput {
            subject: "Do not resend".into(),
            ..draft
        })
        .unwrap();
    let backup = dir.path().join("recovery.pigeon-backup");
    source
        .create_backup(&backup, "correct horse battery staple")
        .unwrap();
    let restored_secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let path = dir.path().join("restored.db");
    let restored = engine(&path, &restored_secrets, &server);
    restored
        .restore_backup(&backup, "correct horse battery staple")
        .unwrap();
    let state = restored.send_state().unwrap();
    assert_eq!(state.drafts.len(), 1);
    assert_eq!(state.drafts[0].id, saved.id);
    assert_eq!(state.drafts[0].body, "Draft canary");
    let item = state
        .outbox
        .iter()
        .find(|item| item.id == queued.id)
        .unwrap();
    assert!(matches!(item.send, SendStatus::Held), "{item:?}");
    assert_eq!(restored.pending_actions().unwrap()[0].state, "conflict");
    assert!(!restored.run_next_operation().unwrap());
    restored.sync_account(&account).unwrap();
    assert_eq!(*server.submitted.lock().unwrap(), 0);
    assert!(server.appended.lock().unwrap().is_empty());
    drop(restored);
    let reopened = engine(&path, &restored_secrets, &server);
    assert!(!reopened.run_next_operation().unwrap());
    assert!(matches!(
        reopened.send_state().unwrap().outbox[0].send,
        SendStatus::Held
    ));
    assert_eq!(reopened.pending_actions().unwrap()[0].state, "conflict");
    assert_eq!(*server.submitted.lock().unwrap(), 0);
    assert!(
        reopened.retry_send(&queued.id).is_err(),
        "ordinary retry cannot release restored work"
    );
    reopened
        .resolve_send(&queued.id, pigeon_core::mail::SendDecision::SendAgain)
        .unwrap();
    assert!(reopened.run_next_operation().unwrap());
    assert_eq!(*server.submitted.lock().unwrap(), 1);
    // Capture acceptance with the Sent copy still pending, then restore it.
    let second_backup = dir.path().join("accepted.pigeon-backup");
    reopened
        .create_backup(&second_backup, "correct horse battery staple")
        .unwrap();
    let final_secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let final_engine = engine(
        &dir.path().join("accepted-restored.db"),
        &final_secrets,
        &server,
    );
    final_engine
        .restore_backup(&second_backup, "correct horse battery staple")
        .unwrap();
    assert!(!final_engine.run_next_operation().unwrap());
    let item = &final_engine.send_state().unwrap().outbox[0];
    assert!(matches!(item.send, SendStatus::Accepted { .. }));
    assert!(matches!(item.sent_copy, SentCopyStatus::Failed { .. }));
    // Folder metadata is intentionally downloaded again after restore.
    final_engine.sync_account(&account).unwrap();
    assert_eq!(*server.submitted.lock().unwrap(), 1);
    final_engine.retry_sent_copy(&queued.id).unwrap();
    while final_engine.run_next_operation().unwrap() {}
    assert_eq!(
        *server.submitted.lock().unwrap(),
        1,
        "saving a restored Sent copy never resubmits mail"
    );
    assert_eq!(*server.appended.lock().unwrap(), ["Sent"]);
}
