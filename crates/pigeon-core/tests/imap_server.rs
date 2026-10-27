//! Real Dovecot integration. Run through tools/mail-test/run.sh, which supplies
//! an isolated TLS server and synthetic credentials. No production mail is used.
use pigeon_core::mail::{AccountConfig, MailEngine, MailError, SearchQuery, TransportSecurity};
use pigeon_core::secrets::{KeyringState, MemoryStore};
use std::sync::Arc;
use zeroize::Zeroizing;

fn config() -> AccountConfig {
    AccountConfig {
        name: "Test IMAP".into(),
        address: "mail@pigeon.test".into(),
        host: "localhost".into(),
        port: 993,
        username: "mail@pigeon.test".into(),
        security: TransportSecurity::Tls,
        ca_certificate: Some(std::fs::read_to_string("/pki/ca.pem").unwrap()),
    }
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server"]
fn real_tls_account_can_authenticate_and_discover_folders() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::new(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
    );
    let account = engine
        .add_account(config(), Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
    assert_eq!(engine.accounts().unwrap()[0].id, account.id);
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server"]
fn real_read_only_sync_downloads_text_and_indexes_it_across_folders() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::new(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
    );
    let account = engine
        .add_account(config(), Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    engine.sync_account(&account.id).unwrap();
    let invoice = engine
        .search(&SearchQuery {
            text: "receipt".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(invoice.total, 1);
    assert_eq!(invoice.messages[0].subject, "Invoice 4471");
    assert!(invoice.messages[0].unread);
    let message = engine.message(invoice.messages[0].id).unwrap();
    assert!(message.body_text.contains("building invoice"));
    let archive = engine
        .search(&SearchQuery {
            text: "migration".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(archive.messages[0].folder, "Archive");
    assert_eq!(
        engine
            .search(&SearchQuery {
                text: "invoice".into(),
                include_junk_trash: true,
                ..Default::default()
            })
            .unwrap()
            .total,
        2
    );
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server"]
fn invalid_certificate_and_bad_password_are_rejected_without_saving_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::new(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
    );
    let mut untrusted = config();
    untrusted.ca_certificate = None;
    assert!(matches!(
        engine.add_account(untrusted, Zeroizing::new(b"pigeon-test-only".to_vec())),
        Err(MailError::Tls)
    ));
    assert!(matches!(
        engine.add_account(config(), Zeroizing::new(b"wrong-password".to_vec())),
        Err(MailError::Authentication)
    ));
    assert!(engine.accounts().unwrap().is_empty());
    let mut starttls = config();
    starttls.port = 143;
    starttls.security = TransportSecurity::StartTls;
    engine
        .add_account(starttls, Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server and counting relay"]
fn mime_text_is_decoded_without_downloading_the_nine_megabyte_attachment() {
    let before = std::fs::read_to_string("/pki/server-to-client-bytes")
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::new(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
    );
    let mut through_relay = config();
    through_relay.port = 1993;
    let account = engine
        .add_account(through_relay, Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    let results = engine
        .search(&SearchQuery {
            text: "resume has:attachment".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(results.total, 1);
    let message = engine.message(results.messages[0].id).unwrap();
    assert_eq!(message.summary.subject, "Unicode café attachment");
    assert_eq!(message.attachments[0].filename, "report.pdf");
    assert!(message.attachments[0].size > 8_000_000);
    assert_eq!(
        engine
            .search(&SearchQuery {
                text: "ATTACHMENT_PAYLOAD_CANARY".into(),
                ..Default::default()
            })
            .unwrap()
            .total,
        0
    );
    let html = engine
        .search(&SearchQuery {
            text: "roadrunner".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(html.total, 1);
    assert!(engine.message(html.messages[0].id).unwrap().has_html);
    assert_eq!(
        engine
            .search(&SearchQuery {
                text: "hidden_secret_script".into(),
                ..Default::default()
            })
            .unwrap()
            .total,
        0
    );
    let after = std::fs::read_to_string("/pki/server-to-client-bytes")
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    eprintln!(
        "Encrypted IMAP bytes received: {} (attachment payload >9 MB)",
        after - before
    );
    assert!(
        after - before < 262_144,
        "text-only sync downloaded too much data: {}",
        after - before
    );
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server"]
fn remote_flags_and_deletions_update_cache_after_restart() {
    // This independent mailbox is changed through a second, real IMAP client;
    // Pigeon's connection remains strictly read-only.
    let mut builder = native_tls::TlsConnector::builder();
    builder.add_root_certificate(
        native_tls::Certificate::from_pem(std::fs::read("/pki/ca.pem").unwrap().as_slice())
            .unwrap(),
    );
    let client = imap::connect(("localhost", 993), "localhost", &builder.build().unwrap()).unwrap();
    let mut admin = client
        .login("mail@pigeon.test", "pigeon-test-only")
        .map_err(|(error, _)| error)
        .unwrap();
    admin.create("Recovery").unwrap();
    admin.append("Recovery", b"From: recover@pigeon.test\r\nTo: mail@pigeon.test\r\nSubject: Reconnect canary\r\n\r\nReconnect durability marker\r\n").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let engine = MailEngine::new(path.clone(), secrets.clone());
    let account = engine
        .add_account(config(), Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    let query = SearchQuery {
        text: "Reconnect durability".into(),
        folder: Some("Recovery".into()),
        ..Default::default()
    };
    let results = engine.search(&query).unwrap();
    assert_eq!(results.total, 1);
    assert!(results.messages[0].unread);
    admin.select("Recovery").unwrap();
    admin.uid_store("1:*", "+FLAGS (\\Seen)").unwrap();
    engine.sync_account(&account.id).unwrap();
    assert!(!engine.search(&query).unwrap().messages[0].unread);
    // Model an unavailable transport at the external connector boundary on
    // restart. The preceding and subsequent syncs use the real TLS server.
    drop(engine);
    struct Offline;
    impl pigeon_core::mail::source::MailConnector for Offline {
        fn connect(
            &self,
            _: &AccountConfig,
            _: &pigeon_core::secrets::Secret,
        ) -> Result<Box<dyn pigeon_core::mail::source::MailSession>, MailError> {
            Err(MailError::Connection)
        }
    }
    let offline = MailEngine::with_connector(path.clone(), secrets.clone(), Arc::new(Offline));
    assert!(matches!(
        offline.sync_account(&account.id),
        Err(MailError::Connection)
    ));
    assert_eq!(offline.search(&query).unwrap().total, 1);
    drop(offline);
    let engine = MailEngine::new(path, secrets);
    engine.sync_account(&account.id).unwrap();
    assert_eq!(engine.search(&query).unwrap().total, 1);
    admin.uid_store("1:*", "+FLAGS (\\Deleted)").unwrap();
    admin.expunge().unwrap();
    engine.sync_account(&account.id).unwrap();
    assert_eq!(engine.search(&query).unwrap().total, 0);
    admin.select("INBOX").unwrap();
    admin.delete("Recovery").unwrap();
    // Drop the disposable administrative session; deleting a selected mailbox
    // or LOGOUT can legitimately produce BYE/ConnectionLost in this adapter.
    drop(admin);
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server"]
fn dropped_wire_connection_preserves_offline_mail_and_recovers() {
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    // An opaque TCP relay drops one connection before TLS; other requests reach
    // the actual Dovecot server. No protocol or engine result is mocked here.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let disconnected = Arc::new(AtomicBool::new(false));
    let flag = disconnected.clone();
    let relay = std::thread::spawn(move || {
        for _ in 0..4 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("relay accept: {error}"),
                }
            };
            if flag.load(Ordering::SeqCst) {
                continue;
            }
            let mut remote = TcpStream::connect(("127.0.0.1", 993)).unwrap();
            let mut peer_read = peer.try_clone().unwrap();
            let mut remote_write = remote.try_clone().unwrap();
            let upstream = std::thread::spawn(move || {
                let _ = std::io::copy(&mut peer_read, &mut remote_write);
                let _ = remote_write.shutdown(Shutdown::Both);
            });
            let _ = std::io::copy(&mut remote, &mut peer);
            let _ = peer.shutdown(Shutdown::Both);
            upstream.join().unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::new(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
    );
    let mut relayed = config();
    relayed.port = port;
    let account = engine
        .add_account(relayed, Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    let query = SearchQuery {
        text: "receipt".into(),
        ..Default::default()
    };
    let before = engine.search(&query).unwrap();
    disconnected.store(true, Ordering::SeqCst);
    assert!(matches!(
        engine.sync_account(&account.id),
        Err(MailError::Tls | MailError::Connection)
    ));
    assert_eq!(engine.search(&query).unwrap().total, before.total);
    assert_eq!(
        engine
            .message(before.messages[0].id)
            .unwrap()
            .summary
            .subject,
        "Invoice 4471"
    );
    disconnected.store(false, Ordering::SeqCst);
    engine.sync_account(&account.id).unwrap();
    assert_eq!(engine.search(&query).unwrap().total, before.total);
    assert_eq!(engine.accounts().unwrap()[0].status, "idle");
    relay.join().unwrap();
}
