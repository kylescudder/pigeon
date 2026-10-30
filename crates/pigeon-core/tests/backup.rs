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
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret, SecretError, SecretStore};
use pigeon_core::unlock::DB_KEY_NAME;
use zeroize::Zeroizing;

const PASSPHRASE: &str = "correct horse battery staple";
const PASSWORD: &[u8] = b"canary-imap-password";
const CANARY_SUBJECT: &str = "Canary subject 7Q2";

/// A server with `count` messages in one folder. While `gate` is set every
/// connection and fetch waits; while `hold_last` is set only the final batch waits, so a
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
        while self.gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
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

type PutHook = Box<dyn FnMut(&str) -> Result<(), SecretError> + Send>;

/// The in-memory keyring, with an optional hook that runs before each write
/// and can fail it, block it, or change the filesystem underneath a restore.
struct Keyring {
    inner: MemoryStore,
    on_put: Mutex<Option<PutHook>>,
}

impl Keyring {
    fn hook(&self, hook: impl FnMut(&str) -> Result<(), SecretError> + Send + 'static) {
        *self.on_put.lock().unwrap() = Some(Box::new(hook));
    }
    fn unhook(&self) {
        *self.on_put.lock().unwrap() = None;
    }
}

impl std::ops::Deref for Keyring {
    type Target = MemoryStore;
    fn deref(&self) -> &MemoryStore {
        &self.inner
    }
}

impl SecretStore for Keyring {
    fn state(&self) -> KeyringState {
        self.inner.state()
    }
    fn unlock(&self) -> Result<(), SecretError> {
        self.inner.unlock()
    }
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        self.inner.get(name)
    }
    fn put(&self, name: &str, label: &str, secret: &[u8]) -> Result<(), SecretError> {
        if let Some(hook) = self.on_put.lock().unwrap().as_mut() {
            hook(name)?;
        }
        self.inner.put(name, label, secret)
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.inner.delete(name)
    }
}

struct Device {
    dir: tempfile::TempDir,
    secrets: Arc<Keyring>,
    engine: MailEngine,
}

impl Device {
    fn new(server: Server) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(Keyring {
            inner: MemoryStore::new(KeyringState::Unlocked),
            on_put: Mutex::new(None),
        });
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

const CURRENT: &[u8] = b"current-password-after-backup";

/// Two accounts backed up with `PASSWORD`. Afterwards the first account's
/// password changes to `CURRENT` and the second's leaves the keyring, so a
/// restore would overwrite one value and create the other.
fn two_accounts_changed_since_backup() -> (Device, PathBuf, tempfile::TempDir, [String; 2]) {
    let device = Device::new(Server::new(1));
    let ids = [0, 1].map(|_| {
        device
            .engine
            .add_account(config(), Zeroizing::new(PASSWORD.to_vec()))
            .unwrap()
            .id
    });
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();
    device
        .secrets
        .put(&backup::password_name(&ids[0]), "test", CURRENT)
        .unwrap();
    device
        .secrets
        .delete(&backup::password_name(&ids[1]))
        .unwrap();
    device
        .engine
        .save_search("Made after the backup", &SearchQuery::default())
        .unwrap();
    (device, file, out, ids)
}

fn password(device: &Device, id: &str) -> Option<Vec<u8>> {
    device
        .secrets
        .get(&backup::password_name(id))
        .unwrap()
        .map(|p| p.to_vec())
}

fn assert_nothing_changed(device: &Device, ids: &[String; 2], store_before: &[u8]) {
    assert_eq!(password(device, &ids[0]).as_deref(), Some(CURRENT));
    assert_eq!(password(device, &ids[1]), None);
    assert_eq!(std::fs::read(device.db()).unwrap(), store_before);
    let names: Vec<String> = std::fs::read_dir(device.dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("restore"))
        .collect();
    assert!(names.is_empty(), "left behind: {names:?}");
}

#[test]
fn keyring_failure_part_way_through_passwords_puts_back_earlier_ones() {
    let (device, file, _out, ids) = two_accounts_changed_since_backup();
    drop_store(&device);
    let before = std::fs::read(device.db()).unwrap();
    let mut writes = 0;
    device.secrets.hook(move |_| {
        writes += 1;
        if writes == 2 {
            return Err(SecretError::Backend("injected keyring failure".into()));
        }
        Ok(())
    });
    let error = device.engine.restore_backup(&file, PASSPHRASE).unwrap_err();
    device.secrets.unhook();
    assert!(matches!(error, MailError::Locked), "{error:?}");
    assert_nothing_changed(&device, &ids, &before);
    assert_eq!(device.engine.saved_searches().unwrap().len(), 1);
    assert_eq!(device.engine.accounts().unwrap().len(), 2);
}

#[test]
fn failing_to_move_the_live_store_aside_puts_back_passwords() {
    let (device, file, _out, ids) = two_accounts_changed_since_backup();
    drop_store(&device);
    let before = std::fs::read(device.db()).unwrap();
    // Occupy every name the live store could be moved to with a directory,
    // which a file can never be renamed over.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let blockers: Vec<PathBuf> = (now - 5..now + 120)
        .map(|t| {
            device
                .dir
                .path()
                .join(format!("mail.db.before-restore-{t}"))
        })
        .collect();
    for blocker in &blockers {
        std::fs::create_dir(blocker).unwrap();
        std::fs::write(blocker.join("occupied"), b"x").unwrap();
    }
    let error = device.engine.restore_backup(&file, PASSPHRASE).unwrap_err();
    for blocker in &blockers {
        std::fs::remove_dir_all(blocker).unwrap();
    }
    assert!(
        matches!(error, MailError::Backup(BackupError::Io(_))),
        "{error:?}"
    );
    assert_nothing_changed(&device, &ids, &before);
    assert_eq!(device.engine.saved_searches().unwrap().len(), 1);
}

/// For the lost-key cases: the store stays unreadable and keyless, exactly
/// as before the restore, and the passwords are put back.
fn without_store_key() -> (Device, PathBuf, tempfile::TempDir, [String; 2], Vec<u8>) {
    let (device, file, out, ids) = two_accounts_changed_since_backup();
    drop_store(&device);
    device.secrets.delete(DB_KEY_NAME).unwrap();
    let before = std::fs::read(device.db()).unwrap();
    (device, file, out, ids, before)
}

#[test]
fn failing_to_save_a_new_store_key_puts_back_the_store_and_passwords() {
    let (device, file, _out, ids, before) = without_store_key();
    device.secrets.hook(|name| {
        if name == DB_KEY_NAME {
            return Err(SecretError::Backend("injected keyring failure".into()));
        }
        Ok(())
    });
    let error = device.engine.restore_backup(&file, PASSPHRASE).unwrap_err();
    device.secrets.unhook();
    assert!(matches!(error, MailError::Storage(_)), "{error:?}");
    assert_nothing_changed(&device, &ids, &before);
    assert!(device.secrets.get(DB_KEY_NAME).unwrap().is_none());
    assert!(matches!(
        device.engine.accounts(),
        Err(MailError::KeyMissing)
    ));
}

#[test]
fn failing_to_rename_the_new_store_into_place_puts_everything_back() {
    let (device, file, _out, ids, before) = without_store_key();
    // Saving the new key is the last step before the rename. Remove the
    // staged store then, so the rename itself fails.
    let dir = device.dir.path().to_owned();
    device.secrets.hook(move |name| {
        if name == DB_KEY_NAME {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".mail.db.restore-")
                {
                    std::fs::remove_file(path).unwrap();
                }
            }
        }
        Ok(())
    });
    let error = device.engine.restore_backup(&file, PASSPHRASE).unwrap_err();
    device.secrets.unhook();
    assert!(
        matches!(error, MailError::Backup(BackupError::Io(_))),
        "{error:?}"
    );
    assert_nothing_changed(&device, &ids, &before);
    assert!(device.secrets.get(DB_KEY_NAME).unwrap().is_none());
    assert!(matches!(
        device.engine.accounts(),
        Err(MailError::KeyMissing)
    ));
}

#[test]
fn successful_restore_writes_every_backed_up_password() {
    let (device, file, _out, ids) = two_accounts_changed_since_backup();
    let summary = device.engine.restore_backup(&file, PASSPHRASE).unwrap();
    assert_eq!((summary.accounts, summary.passwords), (2, 2));
    assert_eq!(password(&device, &ids[0]).as_deref(), Some(PASSWORD));
    assert_eq!(password(&device, &ids[1]).as_deref(), Some(PASSWORD));
    assert_eq!(device.engine.saved_searches().unwrap().len(), 0);
    for id in &ids {
        device.engine.sync_account(id).unwrap();
    }
}

#[test]
fn account_changes_are_refused_while_a_restore_runs() {
    let (device, account_id) = populated();
    let device = Arc::new(device);
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();
    // Pause the restore at its first keyring write.
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel::<()>();
    let mut first = true;
    device.secrets.hook(move |_| {
        if std::mem::take(&mut first) {
            entered.send(()).unwrap();
            wait_release.recv().unwrap();
        }
        Ok(())
    });
    let restoring = {
        let (device, file) = (device.clone(), file.clone());
        std::thread::spawn(move || device.engine.restore_backup(&file, PASSPHRASE))
    };
    wait_entered.recv().unwrap();
    let added = device
        .engine
        .add_account(config(), Zeroizing::new(PASSWORD.to_vec()));
    assert!(matches!(added, Err(MailError::Restoring)), "{added:?}");
    let updated = device
        .engine
        .update_password(&account_id, Zeroizing::new(PASSWORD.to_vec()));
    assert!(matches!(updated, Err(MailError::Restoring)), "{updated:?}");
    assert!(matches!(
        device.engine.sync_account(&account_id),
        Err(MailError::Restoring)
    ));
    assert!(matches!(
        device.engine.remove_account(&account_id),
        Err(MailError::Restoring)
    ));
    release.send(()).unwrap();
    restoring.join().unwrap().unwrap();
    device.secrets.unhook();
    assert_eq!(device.engine.accounts().unwrap().len(), 1);
    device
        .engine
        .add_account(config(), Zeroizing::new(PASSWORD.to_vec()))
        .unwrap();
    assert_eq!(device.engine.accounts().unwrap().len(), 2);
}

#[test]
fn restore_is_refused_while_an_account_is_added_or_its_password_changes() {
    let server = Server::new(1);
    let gate = server.gate.clone();
    let device = Arc::new(Device::new(server));
    let account = device
        .engine
        .add_account(config(), Zeroizing::new(PASSWORD.to_vec()))
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("b.pigeon-backup");
    device.engine.create_backup(&file, PASSPHRASE).unwrap();

    type Change = fn(&MailEngine, &str) -> Result<(), MailError>;
    let changes: [(&str, Change); 2] = [
        ("add", |engine, _| {
            engine
                .add_account(config(), Zeroizing::new(PASSWORD.to_vec()))
                .map(|_| ())
        }),
        ("password", |engine, id| {
            engine.update_password(id, Zeroizing::new(PASSWORD.to_vec()))
        }),
    ];
    for (name, change) in changes {
        // The change waits on the server while holding its place.
        gate.store(true, Ordering::SeqCst);
        let running = {
            let (device, id) = (device.clone(), account.id.clone());
            std::thread::spawn(move || change(&device.engine, &id))
        };
        std::thread::sleep(Duration::from_millis(200));
        let error = device.engine.restore_backup(&file, PASSPHRASE).unwrap_err();
        assert!(
            matches!(error, MailError::Backup(BackupError::Busy)),
            "{name}: {error:?}"
        );
        gate.store(false, Ordering::SeqCst);
        running.join().unwrap().unwrap();
    }
    // The account added meanwhile is in the live store, not set aside.
    assert_eq!(device.engine.accounts().unwrap().len(), 2);
}
