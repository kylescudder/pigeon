//! Drafts, the outbox, and send outcomes against scripted SMTP and IMAP
//! servers. Real-server coverage is in tests/smtp_server.rs.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use pigeon_core::mail::smtp::{Envelope, SmtpConfig, SmtpServer, SmtpTransport, Submission};
use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, DraftInput, MailEngine, MailError, SendDecision, SendStatus, SendingSettings,
    SentCopyStatus, TransportSecurity,
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
}

#[test]
fn sending_submits_once_then_saves_a_sent_copy_with_bcc() {
    let f = fixture();
    let queued = f.engine.send_draft(f.draft()).unwrap();
    assert!(matches!(queued.send, SendStatus::Queued { .. }));
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
    let item = f.item();
    assert!(matches!(item.send, SendStatus::Accepted { .. }));
    assert_eq!(item.sent_copy, SentCopyStatus::Saved);
    assert_eq!(item.recipients, "Alice, bob@pigeon.test, carol@pigeon.test");
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
    assert_eq!(f.item().sent_copy, SentCopyStatus::Saved);
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
    let item = f.item();
    assert!(matches!(item.send, SendStatus::Accepted { .. }));
    assert_eq!(item.sent_copy, SentCopyStatus::Saved);
    assert_eq!(f.smtp.submissions(), 1);
}

#[test]
fn send_again_after_unknown_is_explicit() {
    let f = fixture();
    f.smtp.then(Submission::Unknown("timed out".into()));
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    f.engine
        .resolve_send(&queued.id, SendDecision::SendAgain)
        .unwrap();
    f.drain();
    assert_eq!(f.smtp.submissions(), 2);
    assert!(matches!(f.item().send, SendStatus::Accepted { .. }));
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
    assert_eq!(drafts[0].body, "Second thoughts");
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
    let queued = f.engine.send_draft(f.draft()).unwrap();
    f.drain();
    f.engine.dismiss_outbox(&queued.id).unwrap();
    assert!(f.engine.send_state().unwrap().outbox.is_empty());
    f.drain();
    assert_eq!(f.smtp.submissions(), 1);
}

#[test]
fn a_lock_during_acceptance_keeps_the_send_and_still_saves_one_sent_copy() {
    let f = fixture();
    *f.smtp.lock_on_accept.lock().unwrap() = Some(f.secrets.clone());
    let queued = f.engine.send_draft(f.draft()).unwrap();
    assert!(matches!(
        f.engine.run_next_operation(),
        Err(MailError::Locked)
    ));
    assert_eq!(f.smtp.submissions(), 1);
    assert!(f.imap.appended.lock().unwrap().is_empty());

    f.engine.unlock().unwrap();
    f.drain();
    let item = f
        .engine
        .send_state()
        .unwrap()
        .outbox
        .into_iter()
        .find(|item| item.id == queued.id)
        .unwrap();
    assert!(matches!(item.send, SendStatus::Accepted { .. }), "{item:?}");
    assert_eq!(item.sent_copy, SentCopyStatus::Saved);
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
        "DROP TABLE folder_changes; DROP TABLE recipient_history; DELETE FROM settings WHERE key='recipient_history_imported'; PRAGMA user_version=11;",
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
