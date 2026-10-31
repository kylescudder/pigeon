//! Real SMTP submission and Sent-folder saving against the disposable
//! Dovecot/aiosmtpd server. Run through tools/mail-test/run.sh. Recipient
//! addresses choose the server's behaviour; see tools/mail-test/smtp.py.
use std::sync::Arc;
use std::time::Duration;

use pigeon_core::mail::smtp::{SmtpClient, SmtpConfig};
use pigeon_core::mail::{
    AccountConfig, DraftInput, MailEngine, OutboxItem, SearchQuery, SendDecision, SendStatus,
    SendingSettings, SentCopyStatus, TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore};
use zeroize::Zeroizing;

const PASSWORD: &[u8] = b"pigeon-test-only";

fn smtp(port: u16, security: TransportSecurity) -> SmtpConfig {
    SmtpConfig {
        host: "localhost".into(),
        port,
        security,
        username: "mail@pigeon.test".into(),
    }
}

fn config(outgoing: SmtpConfig) -> AccountConfig {
    AccountConfig {
        managed: false,
        name: "Test account".into(),
        address: "mail@pigeon.test".into(),
        host: "localhost".into(),
        port: 993,
        username: "mail@pigeon.test".into(),
        security: TransportSecurity::Tls,
        ca_certificate: Some(std::fs::read_to_string("/pki/ca.pem").unwrap()),
        sending: SendingSettings {
            smtp: Some(outgoing),
            display_name: "Pigeon Test".into(),
            ..Default::default()
        },
    }
}

struct Harness {
    dir: tempfile::TempDir,
    secrets: Arc<MemoryStore>,
    engine: MailEngine,
    account: String,
}

fn engine_at(dir: &tempfile::TempDir, secrets: Arc<MemoryStore>) -> MailEngine {
    MailEngine::with_smtp(
        dir.path().join("mail.db"),
        secrets,
        Arc::new(SmtpClient::with_final_reply_timeout(Duration::from_secs(3))),
    )
}

fn harness(outgoing: SmtpConfig) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let engine = engine_at(&dir, secrets.clone());
    let account = engine
        .add_account(config(outgoing), Zeroizing::new(PASSWORD.to_vec()))
        .unwrap()
        .id;
    engine.sync_account(&account).unwrap();
    engine.set_undo_send_seconds(0).unwrap();
    Harness {
        dir,
        secrets,
        engine,
        account,
    }
}

fn token() -> String {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).unwrap();
    format!("canary{}", hex::encode(bytes))
}

/// Messages the server spooled (accepted, or received before a fault) that
/// contain `token`.
fn spooled(token: &str) -> usize {
    std::fs::read_dir("/pki/smtp-spool")
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "eml"))
                .filter(|entry| {
                    std::fs::read_to_string(entry.path()).is_ok_and(|text| text.contains(token))
                })
                .count()
        })
        .unwrap_or(0)
}

impl Harness {
    fn send(&self, to: &str, token: &str) -> OutboxItem {
        self.engine
            .send_draft(DraftInput {
                account_id: Some(self.account.clone()),
                to: to.into(),
                subject: format!("Test {token}"),
                body: format!("Synthetic body {token}\n.leading dot line"),
                ..Default::default()
            })
            .unwrap()
    }
    fn drain(&self) {
        while self.engine.run_next_operation().unwrap() {}
    }
    fn item(&self, id: &str) -> OutboxItem {
        self.engine
            .send_state()
            .unwrap()
            .outbox
            .into_iter()
            .find(|item| item.id == id)
            .unwrap()
    }
}

fn admin() -> imap::Session<native_tls::TlsStream<std::net::TcpStream>> {
    let mut builder = native_tls::TlsConnector::builder();
    builder.add_root_certificate(
        native_tls::Certificate::from_pem(std::fs::read("/pki/ca.pem").unwrap().as_slice())
            .unwrap(),
    );
    imap::connect(("localhost", 993), "localhost", &builder.build().unwrap())
        .unwrap()
        .login("mail@pigeon.test", "pigeon-test-only")
        .map_err(|(error, _)| error)
        .unwrap()
}

#[test]
#[ignore = "requires the disposable mail test server"]
fn starttls_send_is_accepted_once_and_saved_to_sent() {
    let h = harness(smtp(587, TransportSecurity::StartTls));
    let token = token();
    let queued = h.send("friend@pigeon.test", &token);
    h.drain();
    let item = h.item(&queued.id);
    assert!(matches!(item.send, SendStatus::Accepted { .. }), "{item:?}");
    assert_eq!(item.sent_copy, SentCopyStatus::Saved);
    assert_eq!(spooled(&token), 1);

    // The Sent copy is a real message the next sync downloads.
    h.engine.sync_account(&h.account).unwrap();
    let results = h
        .engine
        .search(&SearchQuery {
            text: token.clone(),
            folder: Some("Sent".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(results.total, 1);
    assert!(!results.messages[0].unread);
    let message = h.engine.message(results.messages[0].id).unwrap();
    assert!(message.body_text.contains(".leading dot line"));
}

#[test]
#[ignore = "requires the disposable mail test server"]
fn server_failures_map_to_outcomes_without_duplicate_delivery() {
    let h = harness(smtp(465, TransportSecurity::Tls));
    for (recipient, expected, delivered) in [
        ("reject-rcpt@pigeon.test", "failed", 0),
        ("drop-before-data@pigeon.test", "queued", 0),
        ("temp-fail-data@pigeon.test", "queued", 0),
        ("perm-fail-data@pigeon.test", "failed", 0),
        ("drop-after-data@pigeon.test", "unknown", 1),
        ("stall-after-data@pigeon.test", "unknown", 1),
    ] {
        let token = token();
        let queued = h.send(recipient, &token);
        h.drain();
        let state = match h.item(&queued.id).send {
            SendStatus::Failed { .. } => "failed",
            SendStatus::Queued { error: Some(_), .. } => "queued",
            SendStatus::Unknown { .. } => "unknown",
            other => panic!("{recipient}: unexpected {other:?}"),
        };
        assert_eq!(state, expected, "{recipient}");
        // Nothing runs again on its own, so nothing is delivered twice.
        h.drain();
        assert_eq!(spooled(&token), delivered, "{recipient}");
        assert!(matches!(
            h.item(&queued.id).sent_copy,
            SentCopyStatus::NotRequested
        ));
    }
}

#[test]
#[ignore = "requires the disposable mail test server"]
fn an_unknown_outcome_survives_restart_without_resending() {
    let h = harness(smtp(465, TransportSecurity::Tls));
    let token = token();
    let queued = h.send("drop-after-data@pigeon.test", &token);
    h.drain();
    drop(h.engine);
    let engine = engine_at(&h.dir, h.secrets.clone());
    while engine.run_next_operation().unwrap() {}
    let item = engine
        .send_state()
        .unwrap()
        .outbox
        .into_iter()
        .find(|item| item.id == queued.id)
        .unwrap();
    assert!(matches!(item.send, SendStatus::Unknown { .. }));
    assert_eq!(spooled(&token), 1);

    // The user checked and it arrived: mark it sent, which saves a copy
    // without submitting again.
    engine
        .resolve_send(&queued.id, SendDecision::MarkSent)
        .unwrap();
    while engine.run_next_operation().unwrap() {}
    assert_eq!(spooled(&token), 1);
}

#[test]
#[ignore = "requires the disposable mail test server"]
fn a_failed_sent_copy_retries_on_its_own_without_resubmitting() {
    let h = harness(smtp(465, TransportSecurity::Tls));
    let mut admin = admin();
    admin.delete("Sent").unwrap();
    let token = token();
    let queued = h.send("friend@pigeon.test", &token);
    h.drain();
    let item = h.item(&queued.id);
    assert!(matches!(item.send, SendStatus::Accepted { .. }));
    assert!(
        matches!(item.sent_copy, SentCopyStatus::Waiting { error: Some(_) }),
        "{item:?}"
    );

    admin.create("Sent").unwrap();
    h.engine.retry_sent_copy(&queued.id).unwrap();
    h.drain();
    assert_eq!(h.item(&queued.id).sent_copy, SentCopyStatus::Saved);
    assert_eq!(spooled(&token), 1);
    admin.select("Sent").unwrap();
    assert_eq!(admin.search(format!("SUBJECT {token}")).unwrap().len(), 1);
}

/// A TCP relay to the real SMTP server that can go offline. While offline
/// its port is closed, so connections are refused as when the network is down.
fn relay(target: u16, offline: Arc<std::sync::atomic::AtomicBool>) -> u16 {
    use std::sync::atomic::Ordering;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::thread::spawn(move || {
        let mut listener: Option<std::net::TcpListener> = None;
        loop {
            if offline.load(Ordering::SeqCst) {
                listener = None;
            } else if listener.is_none() {
                let bound = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
                bound.set_nonblocking(true).unwrap();
                listener = Some(bound);
            }
            let Some(client) = listener.as_ref().and_then(|l| l.accept().ok()) else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            let client = client.0;
            client.set_nonblocking(false).unwrap();
            let server = std::net::TcpStream::connect(("127.0.0.1", target)).unwrap();
            for (mut from, mut to) in [
                (client.try_clone().unwrap(), server.try_clone().unwrap()),
                (server, client),
            ] {
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut from, &mut to);
                    let _ = to.shutdown(std::net::Shutdown::Both);
                });
            }
        }
    });
    // Wait until the relay is listening before the account is verified.
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        std::thread::sleep(Duration::from_millis(10));
    }
    port
}

#[test]
#[ignore = "requires the disposable mail test server"]
fn offline_send_waits_then_sends_after_reconnecting() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let offline = Arc::new(AtomicBool::new(false));
    let port = relay(465, offline.clone());
    let h = harness(smtp(port, TransportSecurity::Tls));
    offline.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    let token = token();
    let queued = h.send("friend@pigeon.test", &token);
    h.drain();
    let SendStatus::Queued { error, .. } = h.item(&queued.id).send else {
        panic!("expected a queued retry");
    };
    assert!(error.is_some());
    assert_eq!(spooled(&token), 0);

    offline.store(false, Ordering::SeqCst);
    // The relay reopens its port asynchronously; wait until it is back.
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        std::thread::sleep(Duration::from_millis(10));
    }
    h.engine.retry_send(&queued.id).unwrap();
    h.drain();
    assert!(matches!(
        h.item(&queued.id).send,
        SendStatus::Accepted { .. }
    ));
    assert_eq!(spooled(&token), 1);
}

#[test]
#[ignore = "requires the disposable mail test server"]
fn undo_before_the_delay_sends_nothing() {
    let h = harness(smtp(465, TransportSecurity::Tls));
    h.engine.set_undo_send_seconds(30).unwrap();
    let token = token();
    let queued = h.send("friend@pigeon.test", &token);
    h.drain();
    h.engine.return_to_drafts(&queued.id).unwrap();
    h.drain();
    assert_eq!(spooled(&token), 0);
    assert_eq!(h.engine.send_state().unwrap().drafts.len(), 1);
}
