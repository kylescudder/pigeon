//! Encrypted backup and restore through the mail engine. Synthetic accounts
//! and mail only; the keyring is modelled by `MemoryStore`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pigeon_core::backup::{self, BackupError};
use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, MailEngine, MailError, MessageContent, SearchQuery, TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret, SecretStore};
use pigeon_core::unlock::DB_KEY_NAME;
use zeroize::Zeroizing;

const PASSPHRASE: &str = "correct horse battery staple";
const PASSWORD: &[u8] = b"canary-imap-password";
const CANARY_SUBJECT: &str = "Canary subject 7Q2";

/// A server with `count` messages in one folder. While `gate` is set every
/// fetch waits; while `hold_last` is set only the final batch waits, so a
/// sync stays in progress after writing most of its mail.
#[derive(Clone)]
struct Server {
    count: u32,
    delay: Duration,
    gate: Arc<AtomicBool>,
    hold_last: Arc<AtomicBool>,
}

impl Server {
    fn new(count: u32) -> Self {
        Self {
            count,
            delay: Duration::ZERO,
            gate: Arc::new(AtomicBool::new(false)),
            hold_last: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl MailConnector for Server {
    fn connect(
        &self,
        _: &AccountConfig,
        password: &Secret,
    ) -> Result<Box<dyn MailSession>, MailError> {
        if password.as_slice() != PASSWORD {
            return Err(MailError::Authentication);
        }
        Ok(Box::new(self.clone()))
    }
}

impl MailSession for Server {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(vec![RemoteFolder {
            name: "INBOX".into(),
            kind: "inbox".into(),
            delimiter: Some("/".into()),
        }])
    }
    fn snapshot(&mut self, _: &str) -> Result<FolderSnapshot, MailError> {
        Ok(FolderSnapshot {
            uid_validity: 9,
            messages: (1..=self.count)
                .map(|uid| RemoteFlag { uid, unread: true })
                .collect(),
        })
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        while self.gate.load(Ordering::SeqCst)
            || (self.hold_last.load(Ordering::SeqCst) && uids.contains(&self.count))
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(self.delay);
        Ok(uids
            .iter()
            .map(|uid| RemoteMessage {
                uid: *uid,
                content: MessageContent {
                    sender: "Alice <alice@pigeon.test>".into(),
                    recipients: "mail@pigeon.test".into(),
                    subject: format!("{CANARY_SUBJECT} {uid}"),
                    body_text: "Synthetic body".into(),
                    date: 1793005200,
                    ..Default::default()
                },
            })
            .collect())
    }
}

fn config() -> AccountConfig {
    AccountConfig {
        name: "Synthetic".into(),
        address: "mail@pigeon.test".into(),
        host: "imap.pigeon.test".into(),
        port: 993,
        username: "mail@pigeon.test".into(),
        security: TransportSecurity::Tls,
        ca_certificate: None,
    }
}

struct Device {
    dir: tempfile::TempDir,
    secrets: Arc<MemoryStore>,
    engine: MailEngine,
}

impl Device {
    fn new(server: Server) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
        let engine = MailEngine::with_connector(
            dir.path().join("mail.db"),
            secrets.clone(),
            Arc::new(server),
        );
        Self {
            dir,
            secrets,
            engine,
        }
    }
    fn db(&self) -> PathBuf {
        self.dir.path().join("mail.db")
    }
}

/// A device with one synced account and a saved search.
fn populated() -> (Device, String) {
    let device = Device::new(Server::new(3));
    let account = device
        .engine
        .add_account(config(), Zeroizing::new(PASSWORD.to_vec()))
        .unwrap();
    device.engine.sync_account(&account.id).unwrap();
    device
        .engine
        .save_search(
            "Canary invoices",
            &SearchQuery {
                text: "invoice".into(),
                ..Default::default()
            },
        )
        .unwrap();
    (device, account.id)
}

fn all_mail(engine: &MailEngine) -> u64 {
    engine
        .search(&SearchQuery {
            include_junk_trash: true,
            ..Default::default()
        })
        .unwrap()
        .total
}

fn assert_no_plaintext(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        for canary in [
            PASSWORD,
            CANARY_SUBJECT.as_bytes(),
            b"Canary invoices",
            b"imap.pigeon.test",
        ] {
            assert!(
                !bytes.windows(canary.len()).any(|w| w == canary),
                "{} contains plaintext {:?}",
                path.display(),
                String::from_utf8_lossy(canary)
            );
        }
    }
}

#[test]
fn backup_restores_local_data_and_passwords_on_a_new_device_without_mail() {
    let (source, account_id) = populated();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("pigeon.pigeon-backup");
    let summary = source.engine.create_backup(&file, PASSPHRASE).unwrap();
    assert_eq!(
        (summary.accounts, summary.passwords, summary.saved_searches),
        (1, 1, 1)
    );
    assert_no_plaintext(out.path());
    // A standard age file with a passphrase stanza at the documented cost.
    let header = std::fs::read(&file).unwrap();
    let header = String::from_utf8_lossy(&header[..80]);
    assert!(
        header.starts_with("age-encryption.org/v1\n-> scrypt "),
        "{header}"
    );
    assert!(header.lines().nth(1).unwrap().ends_with(" 18"), "{header}");

    // A fresh device: empty keyring, no store.
    let target = Device::new(Server::new(3));
    let restored = target.engine.restore_backup(&file, PASSPHRASE).unwrap();
    assert_eq!(restored.accounts, 1);
    assert_eq!(restored.previous_store, None);
    let accounts = target.engine.accounts().unwrap();
    assert_eq!(accounts[0].id, account_id);
    assert_eq!(accounts[0].status, "idle");
    assert_eq!(accounts[0].last_synced, None);
    assert_eq!(
        target.engine.saved_searches().unwrap()[0].name,
        "Canary invoices"
    );
    assert_eq!(
        target
            .secrets
            .get(&backup::password_name(&account_id))
            .unwrap()
            .unwrap()
            .as_slice(),
        PASSWORD
    );
    // Mail is not in the backup; the restored password downloads it again.
    assert_eq!(all_mail(&target.engine), 0);
    target.engine.sync_account(&account_id).unwrap();
    assert_eq!(all_mail(&target.engine), 3);
    assert_no_plaintext(target.dir.path());
}

#[test]
fn restore_replaces_local_data_and_keeps_the_previous_store_aside() {
    let (device, _) = populated();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();
    let extra = device
        .engine
        .save_search("Made after the backup", &SearchQuery::default())
        .unwrap();
    assert_eq!(device.engine.saved_searches().unwrap().len(), 2);

    let summary = device.engine.restore_backup(&file, PASSPHRASE).unwrap();
    let previous = summary.previous_store.unwrap();
    assert!(previous.starts_with("mail.db.before-restore-"));
    assert!(device.dir.path().join(&previous).exists());
    let searches = device.engine.saved_searches().unwrap();
    assert_eq!(searches.len(), 1);
    assert_ne!(searches[0].id, extra.id);
    assert_no_plaintext(device.dir.path());
}

#[test]
fn wrong_passphrase_leaves_the_store_untouched() {
    let (device, _) = populated();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();
    drop_store(&device);
    let before = std::fs::read(device.db()).unwrap();
    let error = device
        .engine
        .restore_backup(&file, "a different passphrase")
        .unwrap_err();
    assert!(
        matches!(error, MailError::Backup(BackupError::WrongPassphrase)),
        "{error:?}"
    );
    assert_eq!(std::fs::read(device.db()).unwrap(), before);
    assert_eq!(device.engine.saved_searches().unwrap().len(), 1);
}

#[test]
fn damaged_truncated_and_foreign_files_are_refused_without_changes() {
    let (device, _) = populated();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();
    let good = std::fs::read(&file).unwrap();
    let mut flipped = good.clone();
    let last = flipped.len() - 20;
    flipped[last] ^= 0x01;
    let mut header = good.clone();
    header[60] ^= 0x01;
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("flipped payload byte", flipped),
        ("flipped header byte", header),
        ("truncated", good[..good.len() - 40].to_vec()),
        ("cut at the header", good[..120].to_vec()),
        ("empty", Vec::new()),
        ("plain text", b"not a backup at all\n".to_vec()),
    ];
    drop_store(&device);
    let before = std::fs::read(device.db()).unwrap();
    for (name, bytes) in cases {
        let path = out.path().join(format!("{name}.pigeon-backup"));
        std::fs::write(&path, bytes).unwrap();
        let error = device.engine.restore_backup(&path, PASSPHRASE).unwrap_err();
        assert!(
            matches!(
                error,
                MailError::Backup(BackupError::Damaged | BackupError::WrongPassphrase)
            ),
            "{name}: {error:?}"
        );
        assert_eq!(std::fs::read(device.db()).unwrap(), before, "{name}");
    }
    let missing = device
        .engine
        .restore_backup(&out.path().join("missing"), PASSPHRASE)
        .unwrap_err();
    assert!(matches!(missing, MailError::Backup(BackupError::Io(_))));
    assert_eq!(device.engine.saved_searches().unwrap().len(), 1);
    // Nothing half-built is left beside the store.
    let leftovers: Vec<_> = std::fs::read_dir(device.dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("restore"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn short_passphrases_are_refused_and_write_nothing() {
    let (device, _) = populated();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    let error = device.engine.create_backup(&file, "too short").unwrap_err();
    assert!(matches!(
        error,
        MailError::Backup(BackupError::WeakPassphrase)
    ));
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}

#[test]
fn restore_recovers_a_store_whose_key_was_lost() {
    let (device, account_id) = populated();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();
    // The keyring loses everything; the store file remains.
    let old_key = device.secrets.get(DB_KEY_NAME).unwrap().unwrap();
    device.secrets.delete(DB_KEY_NAME).unwrap();
    device
        .secrets
        .delete(&backup::password_name(&account_id))
        .unwrap();
    let reopened = MailEngine::with_connector(
        device.db(),
        device.secrets.clone(),
        Arc::new(Server::new(3)),
    );
    assert!(matches!(reopened.accounts(), Err(MailError::KeyMissing)));

    let summary = reopened.restore_backup(&file, PASSPHRASE).unwrap();
    let new_key = device.secrets.get(DB_KEY_NAME).unwrap().unwrap();
    assert_ne!(new_key.as_slice(), old_key.as_slice());
    assert!(
        device
            .dir
            .path()
            .join(summary.previous_store.unwrap())
            .exists()
    );
    assert_eq!(reopened.accounts().unwrap()[0].id, account_id);
    reopened.sync_account(&account_id).unwrap();
    assert_eq!(all_mail(&reopened), 3);
}

#[test]
fn backups_taken_during_sync_are_consistent_and_do_not_stall_it() {
    let mut server = Server::new(1500);
    server.delay = Duration::from_millis(20);
    let hold_last = server.hold_last.clone();
    hold_last.store(true, Ordering::SeqCst);
    let device = Arc::new(Device::new(server));
    let account = device
        .engine
        .add_account(config(), Zeroizing::new(PASSWORD.to_vec()))
        .unwrap();
    let syncing = {
        let device = device.clone();
        let id = account.id.clone();
        std::thread::spawn(move || device.engine.sync_account(&id))
    };
    // Saved searches are written one by one while backups run. Each backup
    // must hold a prefix of them: a snapshot, not a mixture.
    let stop = Arc::new(AtomicBool::new(false));
    let saved = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let (device, stop, saved) = (device.clone(), stop.clone(), saved.clone());
        std::thread::spawn(move || {
            let mut n = 0;
            while !stop.load(Ordering::SeqCst) {
                let search = device
                    .engine
                    .save_search(&format!("search {n}"), &SearchQuery::default())
                    .unwrap();
                saved.lock().unwrap().push(search.name);
                n += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let out = tempfile::tempdir().unwrap();
    let mut counts = Vec::new();
    for round in 0..3 {
        std::thread::sleep(Duration::from_millis(150));
        let file = out.path().join(format!("{round}.pigeon-backup"));
        let summary = device.engine.create_backup(&file, PASSPHRASE).unwrap();
        assert_eq!(summary.accounts, 1);
        assert_eq!(summary.passwords, 1);
        counts.push(summary.saved_searches);

        let restored = Device::new(Server::new(0));
        restored.engine.restore_backup(&file, PASSPHRASE).unwrap();
        let names: Vec<String> = restored
            .engine
            .saved_searches()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        let expected: Vec<String> = (0..names.len()).map(|n| format!("search {n}")).collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|n| n[7..].parse::<usize>().unwrap());
        assert_eq!(sorted, expected, "backup {round} is not a snapshot");
    }
    assert!(
        !syncing.is_finished(),
        "every backup ran while the sync was in progress"
    );
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    hold_last.store(false, Ordering::SeqCst);
    syncing.join().unwrap().unwrap();
    assert!(counts.windows(2).all(|w| w[0] < w[1]), "{counts:?}");
    assert_eq!(all_mail(&device.engine), 1500);
}

#[test]
fn restore_is_refused_while_an_account_syncs() {
    let server = Server::new(5);
    let gate = server.gate.clone();
    let device = Arc::new(Device::new(server));
    let account = device
        .engine
        .add_account(config(), Zeroizing::new(PASSWORD.to_vec()))
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();

    gate.store(true, Ordering::SeqCst);
    let syncing = {
        let device = device.clone();
        let id = account.id.clone();
        std::thread::spawn(move || device.engine.sync_account(&id))
    };
    std::thread::sleep(Duration::from_millis(200));
    let error = device.engine.restore_backup(&file, PASSPHRASE).unwrap_err();
    assert!(
        matches!(error, MailError::Backup(BackupError::Busy)),
        "{error:?}"
    );
    gate.store(false, Ordering::SeqCst);
    syncing.join().unwrap().unwrap();
    assert_eq!(all_mail(&device.engine), 5);
    device.engine.restore_backup(&file, PASSPHRASE).unwrap();
    assert_eq!(all_mail(&device.engine), 0);
}

#[test]
fn backups_never_contain_the_store_key() {
    let (device, _) = populated();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();
    let key = device.secrets.get(DB_KEY_NAME).unwrap().unwrap();
    let restored = Device::new(Server::new(0));
    restored.engine.restore_backup(&file, PASSPHRASE).unwrap();
    // The new device has its own key, not the source device's.
    let new_key = restored.secrets.get(DB_KEY_NAME).unwrap().unwrap();
    assert_ne!(new_key.as_slice(), key.as_slice());
    assert!(restored.secrets.get(DB_KEY_NAME).unwrap().is_some());
}

/// Closes the engine's cached connection so the store file is checkpointed
/// and stable for byte comparison.
fn drop_store(device: &Device) {
    device.secrets.set_state(KeyringState::Locked);
    let _ = device.engine.accounts();
    device.secrets.set_state(KeyringState::Unlocked);
}
