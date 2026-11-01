//! Real Dovecot integration. Run through tools/mail-test/run.sh, which supplies
//! an isolated TLS server and synthetic credentials. No production mail is used.
use pigeon_core::mail::{AccountConfig, MailEngine, MailError, SearchQuery, TransportSecurity};
use pigeon_core::secrets::{KeyringState, MemoryStore};
use std::sync::Arc;
use zeroize::Zeroizing;

fn config() -> AccountConfig {
    AccountConfig {
        managed: false,
        name: "Test IMAP".into(),
        address: "mail@pigeon.test".into(),
        host: "localhost".into(),
        port: 993,
        username: "mail@pigeon.test".into(),
        security: TransportSecurity::Tls,
        ca_certificate: Some(std::fs::read_to_string("/pki/ca.pem").unwrap()),
        sending: Default::default(),
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
    // The real server reports its hierarchy separator, which drives nesting.
    assert!(
        engine
            .folders()
            .unwrap()
            .iter()
            .all(|folder| folder.delimiter.as_deref().is_some_and(|d| d.len() == 1))
    );
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

fn relay_bytes() -> u64 {
    std::fs::read_to_string("/pki/server-to-client-bytes")
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server and counting relay"]
fn parts_download_on_request_byte_exact_without_marking_mail_read() {
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
    let find = |text: &str| {
        engine
            .search(&SearchQuery {
                text: text.into(),
                ..Default::default()
            })
            .unwrap()
            .messages[0]
            .id
    };

    // The embedded logo is fetched and inlined; the 2 MB manual beside it is not.
    let newsletter = find("pelican");
    let message = engine.message(newsletter).unwrap();
    let logo = message
        .attachments
        .iter()
        .find(|a| a.content_id.as_deref() == Some("logo@pigeon.test"))
        .unwrap();
    assert_eq!(logo.content_type, "image/png");
    assert_eq!(logo.part, "1.2.2");
    let before = relay_bytes();
    let document = engine.viewer_document(newsletter).unwrap();
    let used = relay_bytes() - before;
    eprintln!("Encrypted IMAP bytes for the embedded logo: {used}");
    assert!(used < 65_536, "embedded image fetch pulled {used} bytes");
    assert_eq!(document.missing_inline, 0);
    assert!(
        document
            .html
            .contains("src=\"data:image/png;base64,iVBORw0KGgo"),
        "{}",
        document.html
    );
    assert!(!document.html.contains("cid:"));
    assert!(!document.html.contains("tracker.example"));

    // A base64 attachment decodes to exactly the original bytes.
    let manual = message
        .attachments
        .iter()
        .find(|a| a.filename == "manual.pdf")
        .unwrap();
    let data = engine.attachment(newsletter, &manual.part, false).unwrap();
    assert_eq!(
        data.data.as_slice(),
        b"MANUAL_PAYLOAD_CANARY-".repeat(100_000)
    );
    let report = find("resume");
    let pdf = engine.attachment(report, "2", false).unwrap();
    assert_eq!(pdf.filename, "report.pdf");
    assert_eq!(
        pdf.data.as_slice(),
        b"ATTACHMENT_PAYLOAD_CANARY-".repeat(360_000)
    );

    // BODY.PEEK keeps both messages unread on the server.
    engine.sync_account(&account.id).unwrap();
    for id in [newsletter, report] {
        assert!(engine.message(id).unwrap().summary.unread);
    }
}

type Admin = imap::Session<native_tls::TlsStream<std::net::TcpStream>>;

/// An independent, real IMAP client standing in for webmail or another device.
fn second_client() -> Admin {
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

fn append(admin: &mut Admin, folder: &str, subject: &str) {
    admin
        .append(
            folder,
            format!("From: queue@pigeon.test\r\nTo: mail@pigeon.test\r\nSubject: {subject}\r\n\r\nQueue fixture {subject}\r\n"),
        )
        .unwrap();
}

/// UIDs of messages with this subject, as the second client sees them.
fn server_uids(admin: &mut Admin, folder: &str, subject: &str) -> Vec<u32> {
    admin.select(folder).unwrap();
    let mut uids: Vec<u32> = admin
        .uid_search(format!("SUBJECT \"{subject}\""))
        .unwrap()
        .into_iter()
        .collect();
    uids.sort_unstable();
    uids
}

fn server_seen(admin: &mut Admin, folder: &str, subject: &str) -> bool {
    let uid = server_uids(admin, folder, subject)[0];
    let fetched = admin.uid_fetch(uid.to_string(), "FLAGS").unwrap();
    fetched
        .iter()
        .any(|message| message.flags().contains(&imap::types::Flag::Seen))
}

/// Removes this test's mail from shared folders by UID, leaving fixtures intact.
fn remove_by_subject(admin: &mut Admin, folder: &str, subject: &str) {
    let uids = server_uids(admin, folder, subject);
    if !uids.is_empty() {
        let set = uids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        admin.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").unwrap();
        admin.uid_expunge(&set).unwrap();
    }
}

fn local_id(engine: &MailEngine, folder: &str, subject: &str) -> Option<i64> {
    engine
        .search(&SearchQuery {
            folder: Some(folder.into()),
            ..Default::default()
        })
        .unwrap()
        .messages
        .into_iter()
        .find(|message| message.subject == subject)
        .map(|message| message.id)
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server"]
fn real_queued_changes_are_observed_by_a_second_client() {
    use pigeon_core::mail::ActionKind;
    let mut admin = second_client();
    let capabilities = admin.capabilities().unwrap();
    assert!(capabilities.has_str("MOVE") && capabilities.has_str("UIDPLUS"));
    admin.create("Actions").unwrap();
    for subject in [
        "Queue read",
        "Queue archive",
        "Queue trash",
        "Queue purge",
        "Queue bystander",
    ] {
        append(&mut admin, "Actions", subject);
    }
    // Another client has flagged this for deletion but not expunged it.
    let bystander = server_uids(&mut admin, "Actions", "Queue bystander")[0];
    admin
        .uid_store(bystander.to_string(), "+FLAGS.SILENT (\\Deleted)")
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::new(
        dir.path().join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
    );
    let account = engine
        .add_account(config(), Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    assert!(
        local_id(&engine, "Actions", "Queue bystander").is_none(),
        "\\Deleted mail is not shown"
    );
    let id = |subject| local_id(&engine, "Actions", subject).unwrap();

    engine
        .queue_actions(&[id("Queue read")], ActionKind::MarkRead)
        .unwrap();
    engine.apply_actions(&account.id).unwrap();
    assert!(server_seen(&mut admin, "Actions", "Queue read"));
    engine
        .queue_actions(&[id("Queue read")], ActionKind::MarkUnread)
        .unwrap();
    engine.apply_actions(&account.id).unwrap();
    assert!(!server_seen(&mut admin, "Actions", "Queue read"));

    let (archive, trash, purge) = (id("Queue archive"), id("Queue trash"), id("Queue purge"));
    engine
        .queue_actions(&[archive], ActionKind::Archive)
        .unwrap();
    engine.queue_actions(&[trash], ActionKind::Trash).unwrap();
    engine
        .queue_actions(&[purge], ActionKind::DeletePermanently)
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    assert!(engine.pending_actions().unwrap().is_empty());

    assert!(server_uids(&mut admin, "Actions", "Queue archive").is_empty());
    assert_eq!(server_uids(&mut admin, "Archive", "Queue archive").len(), 1);
    assert_eq!(server_uids(&mut admin, "Trash", "Queue trash").len(), 1);
    assert!(server_uids(&mut admin, "Actions", "Queue purge").is_empty());
    for folder in ["Archive", "Trash"] {
        assert!(server_uids(&mut admin, folder, "Queue purge").is_empty());
    }
    assert_eq!(
        server_uids(&mut admin, "Actions", "Queue bystander"),
        [bystander],
        "UID EXPUNGE left the other client's \\Deleted message alone"
    );
    assert!(local_id(&engine, "Archive", "Queue archive").is_some());
    assert!(local_id(&engine, "Trash", "Queue trash").is_some());
    assert!(local_id(&engine, "Actions", "Queue archive").is_none());

    // Permanently delete from Trash, as the interface offers there.
    let trashed = local_id(&engine, "Trash", "Queue trash").unwrap();
    engine
        .queue_actions(&[trashed], ActionKind::DeletePermanently)
        .unwrap();
    engine.apply_actions(&account.id).unwrap();
    assert!(server_uids(&mut admin, "Trash", "Queue trash").is_empty());

    remove_by_subject(&mut admin, "Archive", "Queue archive");
    admin.select("INBOX").unwrap();
    admin.delete("Actions").unwrap();
    drop(admin);
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server"]
fn real_queue_survives_restart_and_never_resurrects_remotely_deleted_mail() {
    use pigeon_core::mail::ActionKind;
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
    let mut admin = second_client();
    admin.create("Offline").unwrap();
    append(&mut admin, "Offline", "Queue offline read");
    append(&mut admin, "Offline", "Queue vanishing");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let account = {
        let engine = MailEngine::new(path.clone(), secrets.clone());
        let account = engine
            .add_account(config(), Zeroizing::new(b"pigeon-test-only".to_vec()))
            .unwrap();
        engine.sync_account(&account.id).unwrap();
        account
    };
    {
        let offline = MailEngine::with_connector(path.clone(), secrets.clone(), Arc::new(Offline));
        let read = local_id(&offline, "Offline", "Queue offline read").unwrap();
        let vanishing = local_id(&offline, "Offline", "Queue vanishing").unwrap();
        offline
            .queue_actions(&[read], ActionKind::MarkRead)
            .unwrap();
        offline
            .queue_actions(&[vanishing], ActionKind::Archive)
            .unwrap();
        assert!(offline.sync_account(&account.id).is_err());
        assert_eq!(offline.pending_actions().unwrap().len(), 2);
    }
    // While Pigeon is offline, the second client deletes the queued message.
    remove_by_subject(&mut admin, "Offline", "Queue vanishing");

    let engine = MailEngine::new(path, secrets);
    engine.sync_account(&account.id).unwrap();
    assert!(server_seen(&mut admin, "Offline", "Queue offline read"));
    let pending = engine.pending_actions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "conflict");
    for folder in ["Offline", "Archive", "INBOX", "Trash"] {
        assert!(
            server_uids(&mut admin, folder, "Queue vanishing").is_empty(),
            "resurrected in {folder}"
        );
    }
    assert!(local_id(&engine, "Offline", "Queue vanishing").is_none());
    engine.discard_action(pending[0].id).unwrap();

    admin.select("INBOX").unwrap();
    admin.delete("Offline").unwrap();
    drop(admin);
}

#[test]
#[ignore = "requires the disposable Dovecot TLS server"]
fn real_folder_management_is_visible_to_a_second_client() {
    use pigeon_core::mail::FolderChange;
    let dir = tempfile::tempdir().unwrap();
    let engine = MailEngine::new(
        dir.path().join("folders.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
    );
    let a = engine
        .add_account(config(), Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
    engine.sync_account(&a.id).unwrap();
    engine
        .change_folder(
            &a.id,
            FolderChange::Create {
                name: "PigeonFolderTest".into(),
            },
        )
        .unwrap();
    let folder = engine
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.name == "PigeonFolderTest")
        .unwrap();
    engine
        .change_folder(
            &a.id,
            FolderChange::Rename {
                folder_id: folder.id.clone(),
                name: "PigeonRenamedTest".into(),
            },
        )
        .unwrap();
    let second = MailEngine::new(
        dir.path().join("second.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
    );
    let b = second
        .add_account(config(), Zeroizing::new(b"pigeon-test-only".to_vec()))
        .unwrap();
    second.sync_account(&b.id).unwrap();
    assert!(
        second
            .folders()
            .unwrap()
            .iter()
            .any(|f| f.name == "PigeonRenamedTest")
    );
    engine
        .change_folder(
            &a.id,
            FolderChange::Delete {
                folder_id: folder.id,
                confirmed: true,
            },
        )
        .unwrap();
    second.sync_account(&b.id).unwrap();
    assert!(
        !second
            .folders()
            .unwrap()
            .iter()
            .any(|f| f.name == "PigeonRenamedTest" || f.name == "PigeonFolderTest")
    );
}
