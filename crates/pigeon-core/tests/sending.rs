//! Drafts, the outbox, and send outcomes against scripted SMTP and IMAP
//! servers. Real-server coverage is in tests/smtp_server.rs.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use pigeon_core::mail::smtp::{Envelope, SmtpConfig, SmtpServer, SmtpTransport, Submission};
use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, DraftInput, MailEngine, MailError, SavedSignature, SendDecision, SendStatus,
    SendingSettings, SentCopyStatus, TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use zeroize::Zeroizing;

/// Scripted SMTP: each submission pops the next outcome (default accepted).
#[derive(Default)]
struct FakeSmtp {
    outcomes: Mutex<VecDeque<Submission>>,
    sent: Mutex<Vec<(Envelope, String)>>,
    /// Whether the commit hook ran before each submission's outcome.
    commits: Mutex<Vec<bool>>,
    /// Locks this keyring after the message is sent, before the outcome is
    /// recorded: the server accepted it while Pigeon lost its store.
    lock_on_accept: Mutex<Option<Arc<MemoryStore>>>,
}

impl FakeSmtp {
    fn then(&self, outcome: Submission) {
        self.outcomes.lock().unwrap().push_back(outcome);
    }
    fn submissions(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
}

impl SmtpTransport for FakeSmtp {
    fn verify(&self, _: &SmtpServer<'_>, _: &Secret) -> Result<(), MailError> {
        Ok(())
    }
    fn submit(
        &self,
        _: &SmtpServer<'_>,
        _: &Secret,
        envelope: &Envelope,
        message: &[u8],
        commit: &mut dyn FnMut() -> Result<(), MailError>,
    ) -> Submission {
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Submission::Accepted);
        let reached_data = !matches!(outcome, Submission::NotSent { .. });
        if reached_data && commit().is_err() {
            return Submission::NotSent {
                retry: true,
                reason: "commit failed".into(),
            };
        }
        self.commits.lock().unwrap().push(reached_data);
        if let Some(secrets) = self.lock_on_accept.lock().unwrap().take() {
            secrets.set_state(KeyringState::Locked);
        }
        self.sent.lock().unwrap().push((
            envelope.clone(),
            String::from_utf8_lossy(message).into_owned(),
        ));
        outcome
    }
}

#[derive(Default)]
struct FakeImap {
    sent_folder: Mutex<bool>,
    appended: Mutex<Vec<(String, String)>>,
    append_failures: Mutex<u32>,
}

struct Session(Arc<FakeImap>);
struct Connector(Arc<FakeImap>);

impl MailConnector for Connector {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        Ok(Box::new(Session(self.0.clone())))
    }
}

impl MailSession for Session {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        let mut folders = vec![RemoteFolder {
            name: "INBOX".into(),
            kind: "inbox".into(),
            delimiter: Some("/".into()),
        }];
        if *self.0.sent_folder.lock().unwrap() {
            folders.push(RemoteFolder {
                name: "Sent Items".into(),
                kind: "sent".into(),
                delimiter: Some("/".into()),
            });
        }
        Ok(folders)
    }
    fn snapshot(&mut self, _: &str) -> Result<FolderSnapshot, MailError> {
        Ok(FolderSnapshot {
            uid_validity: 1,
            messages: Vec::new(),
        })
    }
    fn fetch_messages(&mut self, _: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        Ok(Vec::new())
    }
    fn append(&mut self, folder: &str, message: &[u8]) -> Result<(), MailError> {
        let mut failures = self.0.append_failures.lock().unwrap();
        if *failures > 0 {
            *failures -= 1;
            return Err(MailError::Connection);
        }
        self.0.appended.lock().unwrap().push((
            folder.to_owned(),
            String::from_utf8_lossy(message).into_owned(),
        ));
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    secrets: Arc<MemoryStore>,
    smtp: Arc<FakeSmtp>,
    imap: Arc<FakeImap>,
    engine: MailEngine,
    account: String,
}

fn sending() -> SendingSettings {
    SendingSettings {
        smtp: Some(SmtpConfig {
            host: "smtp.pigeon.test".into(),
            port: 465,
            security: TransportSecurity::Tls,
            username: "mail@pigeon.test".into(),
        }),
        display_name: "Pigeon Tester".into(),
        signature: "Sent from a test".into(),
        save_sent_copy: true,
        signatures: Vec::new(),
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let smtp = Arc::new(FakeSmtp::default());
    let imap = Arc::new(FakeImap::default());
    *imap.sent_folder.lock().unwrap() = true;
    let engine = MailEngine::with_transports(
        dir.path().join("mail.db"),
        secrets.clone(),
        Arc::new(Connector(imap.clone())),
        smtp.clone(),
    );
    let account = engine
        .add_account(
            AccountConfig {
                managed: false,
                name: "Test".into(),
                address: "mail@pigeon.test".into(),
                host: "imap.pigeon.test".into(),
                port: 993,
                username: "mail@pigeon.test".into(),
                security: TransportSecurity::Tls,
                ca_certificate: None,
                sending: sending(),
            },
            Zeroizing::new(b"test-password".to_vec()),
        )
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    engine.set_undo_send_seconds(0).unwrap();
    Fixture {
        _dir: dir,
        secrets,
        smtp,
        imap,
        engine,
        account: account.id,
    }
}

impl Fixture {
    fn draft(&self) -> DraftInput {
        DraftInput {
            id: None,
            account_id: Some(self.account.clone()),
            to: "Alice <alice@pigeon.test>, bob@pigeon.test".into(),
            cc: "carol@pigeon.test".into(),
            bcc: "dave@pigeon.test".into(),
            subject: "Quarterly receipts".into(),
            body: "Hello\n.leading dot".into(),
            ..Default::default()
        }
    }
    fn drain(&self) {
        while self.engine.run_next_operation().unwrap() {}
    }
    fn item(&self) -> pigeon_core::mail::OutboxItem {
        let state = self.engine.send_state().unwrap();
        assert_eq!(state.outbox.len(), 1);
        state.outbox[0].clone()
    }
    fn assert_outbox_empty(&self) {
        let outbox = self.engine.send_state().unwrap().outbox;
        assert!(outbox.is_empty(), "{outbox:?}");
    }
}

#[test]
fn sending_submits_once_then_saves_a_sent_copy_with_bcc() {
    let f = fixture();
    let queued = f.engine.send_draft(f.draft()).unwrap();
    assert!(matches!(queued.send, SendStatus::Queued { .. }));
    assert_eq!(queued.sent_copy, SentCopyStatus::NotRequested);
    assert_eq!(
        queued.recipients,
        "Alice, bob@pigeon.test, carol@pigeon.test"
    );
    assert!(f.engine.send_state().unwrap().drafts.is_empty());
    f.drain();

    assert_eq!(f.smtp.submissions(), 1);
    let (envelope, message) = f.smtp.sent.lock().unwrap()[0].clone();
    assert_eq!(envelope.from, "mail@pigeon.test");
    assert_eq!(
        envelope.recipients,
        [
            "alice@pigeon.test",
            "bob@pigeon.test",
            "carol@pigeon.test",
            "dave@pigeon.test"
        ]
    );
    assert_eq!(
        mailparse::parse_mail(message.as_bytes())
            .unwrap()
            .ctype
            .mimetype,
        "text/plain"
    );
    assert!(message.contains("Pigeon Tester"));
    assert!(message.contains("Subject: Quarterly receipts"));
    assert!(message.contains("Sent from a test"));
    assert!(
        !message.contains("dave@pigeon.test"),
        "Bcc must not be in the sent headers"
    );
    assert!(f.smtp.commits.lock().unwrap()[0]);

    let appended = f.imap.appended.lock().unwrap().clone();
    assert_eq!(appended.len(), 1);
    assert_eq!(appended[0].0, "Sent Items");
    assert!(appended[0].1.contains("dave@pigeon.test"));
    f.assert_outbox_empty();
}

#[test]
fn a_send_without_a_sent_copy_leaves_the_outbox_once_accepted() {
    let f = fixture();
    let mut settings = sending();
    settings.save_sent_copy = false;
    f.engine.update_sending(&f.account, settings).unwrap();
    f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    assert_eq!(f.smtp.submissions(), 1);
    assert!(f.imap.appended.lock().unwrap().is_empty());
    f.assert_outbox_empty();
}

#[test]
fn undo_within_the_delay_restores_the_draft_and_sends_nothing() {
    let f = fixture();
    f.engine.set_undo_send_seconds(30).unwrap();
    let queued = f.engine.send_draft(f.draft()).unwrap();
    let SendStatus::Queued { send_at, .. } = queued.send else {
        panic!("expected queued");
    };
    assert!(send_at > queued.created + 29_000);
    f.drain();
    assert_eq!(f.smtp.submissions(), 0);

    let draft = f.engine.return_to_drafts(&queued.id).unwrap();
    assert_eq!(draft.subject, "Quarterly receipts");
    let state = f.engine.send_state().unwrap();
    assert!(state.outbox.is_empty());
    assert_eq!(state.drafts.len(), 1);
    f.drain();
    assert_eq!(f.smtp.submissions(), 0);
}

#[test]
fn a_failed_sent_copy_never_submits_again() {
    let f = fixture();
    *f.imap.append_failures.lock().unwrap() = 1;
    let queued = f.engine.send_draft(f.draft()).unwrap();
    assert!(f.engine.run_next_operation().unwrap()); // submit
    assert!(f.engine.run_next_operation().unwrap()); // sent copy fails
    let item = f.item();
    assert!(matches!(item.send, SendStatus::Accepted { .. }));
    assert!(matches!(
        item.sent_copy,
        SentCopyStatus::Waiting { error: Some(_) }
    ));

    f.engine.retry_sent_copy(&queued.id).unwrap();
    f.drain();
    assert_eq!(f.imap.appended.lock().unwrap().len(), 1);
    f.assert_outbox_empty();
    assert_eq!(f.smtp.submissions(), 1);
}

#[test]
fn a_missing_sent_folder_fails_the_copy_and_leaves_the_send_accepted() {
    let f = fixture();
    *f.imap.sent_folder.lock().unwrap() = false;
    f.engine.sync_account(&f.account).unwrap();
    f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    let item = f.item();
    assert!(matches!(item.send, SendStatus::Accepted { .. }));
    assert!(matches!(item.sent_copy, SentCopyStatus::Failed { .. }));
    assert_eq!(f.smtp.submissions(), 1);
}

#[test]
fn an_unknown_outcome_waits_for_a_decision() {
    let f = fixture();
    f.smtp.then(Submission::Unknown("connection lost".into()));
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    assert!(matches!(f.item().send, SendStatus::Unknown { .. }));
    f.drain();
    assert_eq!(f.smtp.submissions(), 1);
    assert!(matches!(
        f.engine.dismiss_outbox(&queued.id),
        Err(MailError::Conflict(_))
    ));
    assert!(matches!(
        f.engine.return_to_drafts(&queued.id),
        Err(MailError::Conflict(_))
    ));

    f.engine
        .resolve_send(&queued.id, SendDecision::MarkSent)
        .unwrap();
    f.drain();
    assert_eq!(f.imap.appended.lock().unwrap().len(), 1);
    f.assert_outbox_empty();
    assert_eq!(f.smtp.submissions(), 1);
}

#[test]
fn send_again_after_unknown_is_explicit() {
    let f = fixture();
    f.smtp.then(Submission::Unknown("timed out".into()));
    let queued = f
        .engine
        .send_draft(DraftInput {
            attachments: vec![
                pigeon_core::mail::DraftAttachment::from_bytes("receipt.bin", b"receipt\0\xff")
                    .unwrap(),
            ],
            ..f.draft()
        })
        .unwrap();
    f.drain();
    f.engine
        .resolve_send(&queued.id, SendDecision::SendAgain)
        .unwrap();
    f.drain();
    assert_eq!(f.smtp.submissions(), 2);
    let submissions = f.smtp.sent.lock().unwrap();
    assert_eq!(submissions[0].1, submissions[1].1);
    drop(submissions);
    f.assert_outbox_empty();
}

#[test]
fn offline_sends_wait_and_permanent_refusals_go_back_to_drafts() {
    let f = fixture();
    f.smtp.then(Submission::NotSent {
        retry: true,
        reason: "the mail server could not be reached".into(),
    });
    f.smtp.then(Submission::NotSent {
        retry: false,
        reason: "the server refused nobody@pigeon.test (550 5.1.1)".into(),
    });
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    let SendStatus::Queued { error, .. } = f.item().send else {
        panic!("expected a queued retry");
    };
    assert!(error.unwrap().contains("could not be reached"));

    f.engine.retry_send(&queued.id).unwrap();
    f.drain();
    let SendStatus::Failed { error } = f.item().send else {
        panic!("expected failed");
    };
    assert!(error.contains("550 5.1.1"));
    assert!(f.engine.send_state().unwrap().recipients.is_empty());
    let draft = f.engine.return_to_drafts(&queued.id).unwrap();
    assert_eq!(draft.to, f.draft().to);
    assert_eq!(f.smtp.submissions(), 2);
    let submissions = f.smtp.sent.lock().unwrap();
    assert_eq!(submissions[0].1, submissions[1].1);
    drop(submissions);
}

#[test]
fn nothing_is_sent_while_the_keyring_is_locked() {
    let f = fixture();
    f.engine.send_draft(f.draft()).unwrap();
    f.secrets.set_state(KeyringState::Locked);
    assert!(matches!(
        f.engine.run_next_operation(),
        Err(MailError::Locked)
    ));
    assert_eq!(f.smtp.submissions(), 0);
    f.engine.unlock().unwrap();
    f.drain();
    assert_eq!(f.smtp.submissions(), 1);
}

#[test]
fn drafts_autosave_and_survive_a_restart() {
    let f = fixture();
    let saved = f.engine.save_draft(f.draft()).unwrap();
    let updated = f
        .engine
        .save_draft(DraftInput {
            id: Some(saved.id.clone()),
            body: "Second thoughts".into(),
            signature: Some("Personal sign-off".into()),
            ..f.draft()
        })
        .unwrap();
    assert_eq!(updated.id, saved.id);
    let path = f._dir.path().join("mail.db");
    let reopened = MailEngine::with_transports(
        path,
        f.secrets.clone(),
        Arc::new(Connector(f.imap.clone())),
        f.smtp.clone(),
    );
    let drafts = reopened.send_state().unwrap().drafts;
    assert_eq!(drafts.len(), 1);
    assert_eq!(
        reopened.load_draft(&drafts[0].id).unwrap().body,
        "Second thoughts"
    );
    assert_eq!(
        reopened
            .load_draft(&drafts[0].id)
            .unwrap()
            .signature
            .as_deref(),
        Some("Personal sign-off")
    );
    reopened.delete_draft(&saved.id).unwrap();
    assert!(reopened.send_state().unwrap().drafts.is_empty());
}

#[test]
fn invalid_messages_are_refused_and_the_draft_is_kept() {
    let f = fixture();
    for (to, expected) in [
        ("", "recipient"),
        ("not an address", "valid"),
        ("a@b.test\r\nBcc: x@y.test", ""),
    ] {
        let error = f
            .engine
            .send_draft(DraftInput {
                to: to.into(),
                cc: String::new(),
                bcc: String::new(),
                ..f.draft()
            })
            .unwrap_err();
        assert!(
            matches!(error, MailError::InvalidConfig(_)),
            "{to:?}: {error}"
        );
        assert!(error.to_string().contains(expected));
    }
    let state = f.engine.send_state().unwrap();
    assert!(state.outbox.is_empty());
    assert_eq!(state.drafts.len(), 3);
    assert_eq!(f.smtp.submissions(), 0);
}

#[test]
fn accounts_without_smtp_cannot_send_until_it_is_added() {
    let f = fixture();
    f.engine
        .update_sending(&f.account, SendingSettings::default())
        .unwrap();
    assert!(matches!(
        f.engine.send_draft(f.draft()),
        Err(MailError::InvalidConfig(_))
    ));
    f.engine.update_sending(&f.account, sending()).unwrap();
    f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    assert_eq!(f.smtp.submissions(), 1);
}

#[test]
fn an_account_with_unsent_mail_cannot_be_removed() {
    let f = fixture();
    f.smtp.then(Submission::Unknown("connection lost".into()));
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    assert!(matches!(
        f.engine.remove_account(&f.account),
        Err(MailError::Conflict(_))
    ));
    f.engine
        .resolve_send(&queued.id, SendDecision::Discard)
        .unwrap();
    assert!(f.engine.send_state().unwrap().outbox.is_empty());
    f.engine.remove_account(&f.account).unwrap();
}

#[test]
fn sent_items_can_be_dismissed_without_resending() {
    let f = fixture();
    *f.imap.sent_folder.lock().unwrap() = false;
    f.engine.sync_account(&f.account).unwrap();
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    assert!(matches!(f.item().sent_copy, SentCopyStatus::Failed { .. }));
    f.engine.dismiss_outbox(&queued.id).unwrap();
    f.assert_outbox_empty();
    f.drain();
    assert_eq!(f.smtp.submissions(), 1);
}

#[test]
fn a_lock_during_acceptance_keeps_the_send_and_still_saves_one_sent_copy() {
    let f = fixture();
    *f.smtp.lock_on_accept.lock().unwrap() = Some(f.secrets.clone());
    f.engine.send_draft(f.draft()).unwrap();
    assert!(matches!(
        f.engine.run_next_operation(),
        Err(MailError::Locked)
    ));
    assert_eq!(f.smtp.submissions(), 1);
    assert!(f.imap.appended.lock().unwrap().is_empty());

    f.engine.unlock().unwrap();
    f.drain();
    f.assert_outbox_empty();
    assert_eq!(f.smtp.submissions(), 1);
    assert_eq!(f.imap.appended.lock().unwrap().len(), 1);
}

#[test]
fn confirmed_recipients_keep_names_and_survive_dismissal_and_restart() {
    let f = fixture();
    let queued = f.engine.send_draft(f.draft()).unwrap();
    assert!(f.engine.send_state().unwrap().recipients.is_empty());
    f.drain();
    let recipients = f.engine.send_state().unwrap().recipients;
    assert_eq!(recipients.len(), 4);
    let alice = recipients
        .iter()
        .find(|r| r.email == "alice@pigeon.test")
        .unwrap();
    assert_eq!(alice.name.as_deref(), Some("Alice"));
    assert!(recipients.iter().any(|r| r.email == "dave@pigeon.test"));
    f.engine.dismiss_outbox(&queued.id).unwrap();
    f.engine
        .send_draft(DraftInput {
            to: "ALICE@pigeon.test".into(),
            cc: String::new(),
            bcc: String::new(),
            ..f.draft()
        })
        .unwrap();
    f.drain();
    let reopened = MailEngine::with_transports(
        f._dir.path().join("mail.db"),
        f.secrets.clone(),
        Arc::new(Connector(f.imap.clone())),
        f.smtp.clone(),
    );
    let recipients = reopened.send_state().unwrap().recipients;
    assert_eq!(recipients.len(), 4);
    assert_eq!(
        recipients
            .iter()
            .find(|r| r.email.eq_ignore_ascii_case("alice@pigeon.test"))
            .unwrap()
            .name
            .as_deref(),
        Some("Alice")
    );
}

#[test]
fn unsent_and_unknown_recipients_are_not_remembered() {
    let f = fixture();
    f.engine.set_undo_send_seconds(30).unwrap();
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.engine.return_to_drafts(&queued.id).unwrap();
    assert!(f.engine.send_state().unwrap().recipients.is_empty());
    f.engine.set_undo_send_seconds(0).unwrap();
    f.smtp.then(Submission::Unknown("lost confirmation".into()));
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    assert!(f.engine.send_state().unwrap().recipients.is_empty());
    f.engine
        .resolve_send(&queued.id, SendDecision::MarkSent)
        .unwrap();
    assert_eq!(f.engine.send_state().unwrap().recipients.len(), 4);
}

#[test]
fn upgrading_imports_older_confirmed_recipients_even_after_dismissal() {
    let f = fixture();
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    f.engine.dismiss_outbox(&queued.id).unwrap();
    let path = f._dir.path().join("mail.db");
    // Reconstruct the previous schema with its existing confirmed outbox row.
    let pigeon_core::unlock::UnlockOutcome::Opened { store, .. } =
        pigeon_core::unlock::open_store(f.secrets.as_ref(), &path)
    else {
        panic!("store should open");
    };
    store.connection().execute_batch(
        "DROP TABLE image_trust_hosts; DROP TABLE image_trust; DROP TABLE sender_images; DROP TABLE folder_changes; DROP TABLE recipient_history; DELETE FROM settings WHERE key='recipient_history_imported'; ALTER TABLE messages DROP COLUMN in_reply_to; ALTER TABLE messages DROP COLUMN threading_version; DROP INDEX drafts_listing; ALTER TABLE drafts DROP COLUMN draft_to; ALTER TABLE drafts DROP COLUMN draft_cc; ALTER TABLE drafts DROP COLUMN draft_subject; PRAGMA user_version=11;",
    ).unwrap();
    drop(store);
    let reopened = MailEngine::with_transports(
        path,
        f.secrets.clone(),
        Arc::new(Connector(f.imap.clone())),
        f.smtp.clone(),
    );
    let state = reopened.send_state().unwrap();
    assert!(state.outbox.is_empty());
    assert_eq!(state.recipients.len(), 4);
    assert_eq!(
        state
            .recipients
            .iter()
            .find(|r| r.email == "alice@pigeon.test")
            .unwrap()
            .name
            .as_deref(),
        Some("Alice")
    );
}

#[test]
fn attachment_bytes_survive_restart_undo_and_smtp_and_sent_copy() {
    use pigeon_core::mail::DraftAttachment;
    let f = fixture();
    let bytes = b"receipt\0\xff\r\n";
    let attachment = DraftAttachment::from_bytes("caf\u{e9}.bin", bytes).unwrap();
    let saved = f
        .engine
        .save_draft(DraftInput {
            attachments: vec![attachment.clone()],
            ..f.draft()
        })
        .unwrap();
    let reopened = MailEngine::with_transports(
        f._dir.path().join("mail.db"),
        f.secrets.clone(),
        Arc::new(Connector(f.imap.clone())),
        f.smtp.clone(),
    );
    assert_eq!(
        reopened.load_draft(&saved.id).unwrap().attachments,
        vec![attachment.clone()]
    );
    reopened.set_undo_send_seconds(60).unwrap();
    let input = DraftInput {
        id: Some(saved.id),
        attachments: vec![attachment.clone()],
        ..f.draft()
    };
    let queued = reopened.send_draft(input.clone()).unwrap();
    drop(reopened);
    let reopened = MailEngine::with_transports(
        f._dir.path().join("mail.db"),
        f.secrets.clone(),
        Arc::new(Connector(f.imap.clone())),
        f.smtp.clone(),
    );
    let restored = reopened.return_to_drafts(&queued.id).unwrap();
    assert_eq!(restored.attachments, vec![attachment]);
    reopened.set_undo_send_seconds(0).unwrap();
    reopened
        .send_draft(DraftInput {
            id: Some(restored.id),
            ..input
        })
        .unwrap();
    while reopened.run_next_operation().unwrap() {}
    let assert_mime = |message: &str| {
        let parsed = mailparse::parse_mail(message.as_bytes()).unwrap();
        assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
        assert_eq!(parsed.subparts.len(), 2);
        assert_eq!(parsed.subparts[1].get_body_raw().unwrap(), bytes);
        assert_eq!(
            parsed.subparts[1]
                .get_content_disposition()
                .params
                .get("filename")
                .unwrap(),
            "caf\u{e9}.bin"
        );
    };
    assert_eq!(f.smtp.submissions(), 1);
    assert_mime(&f.smtp.sent.lock().unwrap()[0].1);
    assert_mime(&f.imap.appended.lock().unwrap()[0].1);
}

#[test]
fn attachments_reject_header_injection_bad_encoding_and_excess_size() {
    use pigeon_core::mail::{DraftAttachment, MAX_OUTGOING_ATTACHMENT_BYTES};
    let f = fixture();
    for filename in ["../secret", "a\r\nBcc: injected", "..", "a\\b"] {
        assert!(DraftAttachment::from_bytes(filename, b"test").is_err());
    }
    let invalid = DraftAttachment {
        filename: "test.bin".into(),
        data: "!invalid!".into(),
    };
    assert!(
        f.engine
            .save_draft(DraftInput {
                attachments: vec![invalid],
                ..f.draft()
            })
            .is_err()
    );
    let large =
        DraftAttachment::from_bytes("large.bin", &vec![0; MAX_OUTGOING_ATTACHMENT_BYTES / 2 + 1])
            .unwrap();
    assert!(
        f.engine
            .save_draft(DraftInput {
                attachments: vec![large.clone(), large],
                ..f.draft()
            })
            .is_err()
    );
    assert!(f.engine.send_state().unwrap().drafts.is_empty());
}

#[test]
fn named_signatures_survive_restart_and_draft_choice_controls_sent_text() {
    let f = fixture();
    let mut settings = sending();
    settings.signatures = vec![SavedSignature {
        id: "work".into(),
        name: "Work".into(),
        body: "Work sign-off".into(),
    }];
    f.engine.update_sending(&f.account, settings).unwrap();
    let reopened = MailEngine::with_transports(
        f._dir.path().join("mail.db"),
        f.secrets.clone(),
        Arc::new(Connector(f.imap.clone())),
        f.smtp.clone(),
    );
    let accounts = reopened.accounts().unwrap();
    assert_eq!(accounts[0].config.sending.signatures[0].name, "Work");
    assert_eq!(
        accounts[0].config.sending.signatures[0].body,
        "Work sign-off"
    );
    for signature in [Some("Work sign-off".to_owned()), Some(String::new())] {
        reopened
            .send_draft(DraftInput {
                signature,
                ..f.draft()
            })
            .unwrap();
        while reopened.run_next_operation().unwrap() {}
    }
    let sent = f.smtp.sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent[0].1.contains("Work sign-off"));
    assert!(!sent[0].1.contains("Sent from a test"));
    assert!(!sent[1].1.contains("Work sign-off"));
    assert!(!sent[1].1.contains("Sent from a test"));
}

#[test]
fn rich_draft_recovers_and_sends_safe_alternatives_with_signatures() {
    let f = fixture();
    let mut draft = f.draft();
    draft.body_html = Some("<p>Hello <b>café</b></p><ul><li>Budget</li></ul><img src='https://tracker.test/pixel'><script>bad()</script>".into());
    let saved = f.engine.save_draft(draft).unwrap();
    let reopened = MailEngine::with_transports(
        f._dir.path().join("mail.db"),
        f.secrets.clone(),
        Arc::new(Connector(f.imap.clone())),
        f.smtp.clone(),
    );
    let restored = reopened.load_draft(&saved.id).unwrap();
    assert_eq!(restored.id, saved.id);
    assert!(restored.body_html.as_ref().unwrap().contains("<b>café</b>"));
    assert!(!restored.body_html.as_ref().unwrap().contains("tracker"));
    assert!(!restored.body_html.as_ref().unwrap().contains("script"));
    assert!(restored.body.contains("Budget"));
    let queued = reopened
        .send_draft(DraftInput {
            id: Some(restored.id),
            body: restored.body,
            body_html: restored.body_html,
            ..f.draft()
        })
        .unwrap();
    let editable = reopened.return_to_drafts(&queued.id).unwrap();
    assert!(editable.body_html.unwrap().contains("<b>café</b>"));
    reopened
        .send_draft(DraftInput {
            id: Some(editable.id),
            body_html: saved.body_html,
            ..f.draft()
        })
        .unwrap();
    while reopened.run_next_operation().unwrap() {}
    let sent = f.smtp.sent.lock().unwrap();
    let parsed = mailparse::parse_mail(sent[0].1.as_bytes()).unwrap();
    assert_eq!(parsed.ctype.mimetype, "multipart/alternative");
    assert_eq!(parsed.subparts.len(), 2);
    assert_eq!(parsed.subparts[0].ctype.mimetype, "text/plain");
    assert_eq!(parsed.subparts[1].ctype.mimetype, "text/html");
    for part in &parsed.subparts {
        let body = part.get_body().unwrap();
        assert!(
            body.contains("café") && body.contains("Budget") && body.contains("Sent from a test")
        );
        assert!(!body.contains("tracker") && !body.contains("bad()"));
    }
    assert!(!sent[0].1.contains("dave@pigeon.test"));
    let appended = f.imap.appended.lock().unwrap();
    let copy = mailparse::parse_mail(appended[0].1.as_bytes()).unwrap();
    for i in 0..2 {
        assert_eq!(
            parsed.subparts[i].get_body().unwrap(),
            copy.subparts[i].get_body().unwrap()
        );
    }
}

#[test]
fn encoded_whitespace_survives_in_both_alternatives() {
    let f = fixture();
    let mut draft = f.draft();
    draft.body_html = Some("Total:\u{a0} \u{a0}42<br>\u{a0} \u{a0} indented".into());
    let saved = f.engine.save_draft(draft.clone()).unwrap();
    assert!(saved.body.contains("Total:   42"));
    assert!(saved.body.contains("    indented"));
    assert!(!saved.body.contains('\u{a0}'));
    f.engine.send_draft(draft).unwrap();
    while f.engine.run_next_operation().unwrap() {}
    let sent = f.smtp.sent.lock().unwrap();
    let parsed = mailparse::parse_mail(sent[0].1.as_bytes()).unwrap();
    let text = parsed.subparts[0].get_body().unwrap();
    assert!(text.contains("Total:   42") && text.contains("    indented"));
    assert!(!text.contains('\u{a0}'));
    let html = parsed.subparts[1].get_body().unwrap();
    assert!(html.contains("&nbsp; &nbsp;") || html.contains("\u{a0} \u{a0}"));
}

#[test]
fn old_draft_payloads_default_to_plain_text() {
    let input: DraftInput = serde_json::from_str(r#"{"body":"Old text"}"#).unwrap();
    assert!(input.body_html.is_none());
}

#[test]
fn rich_formatting_keeps_composer_styles_and_links_but_nothing_active_or_remote() {
    let f = fixture();
    let mut draft = f.draft();
    draft.body_html = Some(concat!(
        r#"<h1 style="text-align: center; position: fixed">Plan</h1>"#,
        r#"<p><u>Under</u> <s>gone</s> x<sup>2</sup> H<sub>2</sub>O "#,
        r#"<span style="color: #d93025; background-color: rgb(255, 242, 168); font-size: 90px">red</span> "#,
        r#"<span style="background-image: url(https://tracker.test/a)">plain</span> "#,
        r#"<span style="color: expression(alert(1))">expr</span> "#,
        r#"<span style="background-color: initial; font-weight: inherit">junk</span></p>"#,
        r#"<blockquote>Quoted</blockquote>"#,
        r#"<blockquote style="margin: 0 0 0 40px; border: none; padding: 0px">Indented</blockquote>"#,
        r#"<pre><code>let x = 1;</code></pre><hr>"#,
        r#"<a href="https://example.test/docs" onclick="x()" target="_blank">Docs</a> "#,
        r#"<a href="mailto:alice@pigeon.test">Mail</a> "#,
        r#"<a href="javascript:alert(1)">Bad</a> <a href="/relative">Rel</a> "#,
        r#"<img src="https://tracker.test/pixel"><iframe src="https://evil.test"></iframe>"#,
    ).into());
    let html = f.engine.save_draft(draft).unwrap().body_html.unwrap();
    for kept in [
        r#"<h1 style="text-align: center">Plan</h1>"#,
        "<u>Under</u>",
        "<s>gone</s>",
        "<sup>2</sup>",
        "<sub>2</sub>",
        r#"<span style="color: #d93025; background-color: rgb(255, 242, 168)">red</span>"#,
        "<blockquote>Quoted</blockquote>",
        r#"<blockquote style="margin: 0 0 0 40px; border: none; padding: 0px">Indented</blockquote>"#,
        "<pre><code>let x = 1;</code></pre><hr>",
        r#"<a href="https://example.test/docs">Docs</a>"#,
        r#"<a href="mailto:alice@pigeon.test">Mail</a>"#,
    ] {
        assert!(html.contains(kept), "{kept} missing from {html}");
    }
    for dropped in [
        "position",
        "font-size",
        "tracker",
        "expression",
        "javascript",
        "relative",
        "onclick",
        "target",
        "iframe",
        "evil",
    ] {
        assert!(!html.contains(dropped), "{dropped} survived in {html}");
    }
    f.engine
        .send_draft(DraftInput {
            body_html: Some(html),
            ..f.draft()
        })
        .unwrap();
    f.drain();
    let sent = f.smtp.sent.lock().unwrap();
    let parsed = mailparse::parse_mail(sent[0].1.as_bytes()).unwrap();
    let text = parsed.subparts[0].get_body().unwrap();
    for visible in [
        "Plan",
        "Quoted",
        "Indented",
        "let x = 1;",
        "Docs",
        "https://example.test/docs",
    ] {
        assert!(text.contains(visible), "{visible} missing from {text}");
    }
    assert!(
        parsed.subparts[1]
            .get_body()
            .unwrap()
            .contains(r#"<a href="https://example.test/docs">Docs</a>"#)
    );
}

#[test]
fn a_rich_draft_uses_its_chosen_signature_in_both_alternatives() {
    for (choice, expected) in [
        (Some("Work sign-off"), Some("Work sign-off")),
        (Some(""), None),
    ] {
        let f = fixture();
        let mut draft = f.draft();
        draft.body_html = Some("<p>Hello</p>".into());
        draft.signature = choice.map(Into::into);
        f.engine.send_draft(draft).unwrap();
        f.drain();
        let sent = f.smtp.sent.lock().unwrap();
        let parsed = mailparse::parse_mail(sent[0].1.as_bytes()).unwrap();
        for part in &parsed.subparts {
            let body = part.get_body().unwrap();
            assert!(!body.contains("Sent from a test"));
            match expected {
                Some(signature) => assert!(body.contains("-- ") && body.contains(signature)),
                None => assert!(!body.contains("-- ")),
            }
        }
    }
}

#[test]
fn rich_drafts_keep_attachments_and_signatures_in_both_mime_copies() {
    use pigeon_core::mail::DraftAttachment;
    let f = fixture();
    let attachment = DraftAttachment::from_bytes("receipt.bin", b"receipt\0\xff").unwrap();
    let queued = f
        .engine
        .send_draft(DraftInput {
            body_html: Some("<p>Hello <b>café</b></p>".into()),
            signature: Some("Chosen signature".into()),
            attachments: vec![attachment],
            ..f.draft()
        })
        .unwrap();
    assert!(matches!(queued.send, SendStatus::Queued { .. }));
    f.drain();
    let assert_message = |message: &str| {
        let parsed = mailparse::parse_mail(message.as_bytes()).unwrap();
        assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
        assert_eq!(parsed.subparts.len(), 2);
        let alternatives = &parsed.subparts[0];
        assert_eq!(alternatives.ctype.mimetype, "multipart/alternative");
        assert_eq!(alternatives.subparts[0].ctype.mimetype, "text/plain");
        assert_eq!(alternatives.subparts[1].ctype.mimetype, "text/html");
        assert!(
            alternatives.subparts[1]
                .get_body()
                .unwrap()
                .contains("<b>café</b>")
        );
        for part in &alternatives.subparts {
            let body = part.get_body().unwrap();
            assert!(body.contains("café") && body.contains("Chosen signature"));
            assert!(!body.contains("Sent from a test"));
        }
        assert_eq!(parsed.subparts[1].get_body_raw().unwrap(), b"receipt\0\xff");
    };
    assert_message(&f.smtp.sent.lock().unwrap()[0].1);
    assert_message(&f.imap.appended.lock().unwrap()[0].1);
    f.assert_outbox_empty();
}

#[test]
fn polling_drafts_does_not_return_attachment_payloads_or_message_bodies() {
    let f = fixture();
    let saved = f
        .engine
        .save_draft(DraftInput {
            body: "Private draft body".into(),
            attachments: vec![
                pigeon_core::mail::DraftAttachment::from_bytes(
                    "receipt.bin",
                    &vec![42; 1024 * 1024],
                )
                .unwrap(),
            ],
            ..f.draft()
        })
        .unwrap();
    let state = serde_json::to_value(f.engine.send_state().unwrap()).unwrap();
    let draft = &state["drafts"][0];
    assert_eq!(draft["subject"], "Quarterly receipts");
    assert!(draft.get("attachments").is_none());
    assert!(draft.get("body").is_none());
    assert!(serde_json::to_vec(&state).unwrap().len() < 4096);
    let loaded = f.engine.load_draft(&saved.id).unwrap();
    assert_eq!(loaded.body, "Private draft body");
    assert_eq!(loaded.attachments, saved.attachments);
    assert!(matches!(
        f.engine.load_draft("missing"),
        Err(MailError::NotFound)
    ));
    f.engine
        .save_draft(DraftInput {
            id: Some(saved.id.clone()),
            subject: "Revised receipt".into(),
            attachments: loaded.attachments,
            ..f.draft()
        })
        .unwrap();
    assert_eq!(
        f.engine.send_state().unwrap().drafts[0].subject,
        "Revised receipt"
    );
    f.secrets.set_state(KeyringState::Locked);
    assert!(matches!(
        f.engine.load_draft(&saved.id),
        Err(MailError::Locked)
    ));
}
