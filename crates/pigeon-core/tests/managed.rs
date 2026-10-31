//! Accounts declared in accounts.json (Home Manager): creation without a
//! password, in-place updates, and what happens when they leave the file.

use std::path::Path;
use std::sync::Arc;

use pigeon_core::mail::managed::{self, Declaration};
use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{AccountConfig, MailEngine, MailError, SendingSettings, TransportSecurity};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use zeroize::Zeroizing;

struct Server;
impl MailConnector for Server {
    fn connect(
        &self,
        _: &AccountConfig,
        password: &Secret,
    ) -> Result<Box<dyn MailSession>, MailError> {
        if password.as_slice() != b"app-password" {
            return Err(MailError::Authentication);
        }
        Ok(Box::new(Session))
    }
}
struct Session;
impl MailSession for Session {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(vec![])
    }
    fn snapshot(&mut self, _: &str) -> Result<FolderSnapshot, MailError> {
        unreachable!("no folders")
    }
    fn fetch_messages(&mut self, _: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        Ok(vec![])
    }
}

fn engine(dir: &Path) -> MailEngine {
    MailEngine::with_connector(
        dir.join("mail.db"),
        Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        Arc::new(Server),
    )
}

fn write(dir: &Path, json: &str) -> Declaration {
    let path = managed::accounts_path(dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, json).unwrap();
    managed::load(&path)
}

fn declaration(host: &str) -> String {
    format!(
        r#"{{"version":1,"accounts":{{"personal":{{
            "address":"me@example.test","realName":"Me","userName":"me","primary":true,
            "imap":{{"host":"{host}","port":993,"security":"tls"}},
            "smtp":{{"host":"smtp.example.test","port":587,"security":"start_tls"}},
            "caCertificate":null,"signature":"Me"}}}}}}"#
    )
}

#[test]
fn declared_account_waits_for_a_password_then_syncs() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let declared = write(dir.path(), &declaration("imap.example.test"));
    assert!(declared.warnings.is_empty());
    engine.reconcile_managed(&declared).unwrap();

    let accounts = engine.accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    let account = &accounts[0];
    assert_eq!(account.id, "managed-personal");
    assert!(account.config.managed);
    assert_eq!(account.config.name, "personal");
    assert_eq!(account.config.host, "imap.example.test");
    let sending = &account.config.sending;
    assert_eq!(sending.display_name, "Me");
    assert_eq!(sending.signature, "Me");
    let smtp = sending.smtp.as_ref().unwrap();
    assert_eq!((smtp.host.as_str(), smtp.port), ("smtp.example.test", 587));
    assert_eq!(smtp.security, TransportSecurity::StartTls);
    assert_eq!(smtp.username, "me");
    assert!(account.error.is_some());

    assert!(matches!(
        engine.sync_account(&account.id),
        Err(MailError::PasswordNeeded)
    ));
    assert!(matches!(
        engine.update_password(&account.id, Zeroizing::new(b"wrong".to_vec())),
        Err(MailError::Authentication)
    ));
    engine
        .update_password(&account.id, Zeroizing::new(b"app-password".to_vec()))
        .unwrap();
    engine.sync_account(&account.id).unwrap();
    assert_eq!(engine.accounts().unwrap()[0].status, "idle");
}

#[test]
fn changes_apply_in_place_and_keep_the_password() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    engine
        .reconcile_managed(&write(dir.path(), &declaration("imap.example.test")))
        .unwrap();
    engine
        .update_password("managed-personal", Zeroizing::new(b"app-password".to_vec()))
        .unwrap();

    engine
        .reconcile_managed(&write(dir.path(), &declaration("imap2.example.test")))
        .unwrap();
    let accounts = engine.accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].config.host, "imap2.example.test");
    // The stored password still works: nothing asks for it again.
    engine.sync_account("managed-personal").unwrap();
}

#[test]
fn managed_accounts_cannot_be_removed_or_edited_in_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    engine
        .reconcile_managed(&write(dir.path(), &declaration("imap.example.test")))
        .unwrap();
    assert!(matches!(
        engine.remove_account("managed-personal"),
        Err(MailError::Conflict(_))
    ));
    assert!(matches!(
        engine.update_sending("managed-personal", SendingSettings::default()),
        Err(MailError::Conflict(_))
    ));
    assert_eq!(engine.accounts().unwrap().len(), 1);
}

#[test]
fn an_account_that_leaves_the_file_becomes_ordinary_and_keeps_its_mail() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    engine
        .reconcile_managed(&write(dir.path(), &declaration("imap.example.test")))
        .unwrap();
    engine
        .reconcile_managed(&write(dir.path(), r#"{"version":1,"accounts":{}}"#))
        .unwrap();
    let accounts = engine.accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert!(!accounts[0].config.managed);
    engine.remove_account("managed-personal").unwrap();
    assert!(engine.accounts().unwrap().is_empty());
}

#[test]
fn an_unreadable_file_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    engine
        .reconcile_managed(&write(dir.path(), &declaration("imap.example.test")))
        .unwrap();
    for broken in ["{ not json", r#"{"version":2,"accounts":{}}"#] {
        let declared = write(dir.path(), broken);
        assert!(declared.unreadable);
        assert_eq!(declared.warnings.len(), 1);
        engine.reconcile_managed(&declared).unwrap();
        assert!(engine.accounts().unwrap()[0].config.managed);
    }
}

#[test]
fn missing_file_declares_nothing_and_bad_entries_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let missing = managed::load(&managed::accounts_path(dir.path()));
    assert!(missing.accounts.is_empty() && missing.warnings.is_empty() && !missing.unreadable);

    let declared = write(
        dir.path(),
        r#"{"version":1,"accounts":{
            "bad name!":{"address":"a@example.test","userName":"a",
              "imap":{"host":"imap.example.test","port":993,"security":"tls"},"smtp":null},
            "good":{"address":"a@example.test","userName":"a",
              "imap":{"host":"imap.example.test","port":993,"security":"tls"},"smtp":null}}}"#,
    );
    assert_eq!(declared.accounts.len(), 1);
    assert_eq!(declared.accounts[0].id, "managed-good");
    assert_eq!(declared.warnings.len(), 1);
    assert!(declared.warnings[0].contains("bad name!"));
}

#[test]
fn a_certificate_file_is_read_and_the_app_cannot_claim_managed() {
    let dir = tempfile::tempdir().unwrap();
    let pem = dir.path().join("ca.pem");
    std::fs::write(
        &pem,
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let declared = write(
        dir.path(),
        &format!(
            r#"{{"version":1,"accounts":{{"lab":{{"address":"a@example.test","userName":"a",
              "imap":{{"host":"imap.lab.test","port":993,"security":"tls"}},"smtp":null,
              "caCertificate":"{}"}}}}}}"#,
            pem.display()
        ),
    );
    assert!(declared.warnings.is_empty(), "{:?}", declared.warnings);
    assert!(
        declared.accounts[0]
            .config
            .ca_certificate
            .as_deref()
            .unwrap()
            .contains("BEGIN CERTIFICATE")
    );

    let engine = engine(dir.path());
    let account = engine
        .add_account(
            AccountConfig {
                managed: true,
                name: "Mine".into(),
                address: "me@example.test".into(),
                host: "imap.example.test".into(),
                port: 993,
                username: "me".into(),
                security: TransportSecurity::Tls,
                ca_certificate: None,
                sending: Default::default(),
            },
            Zeroizing::new(b"app-password".to_vec()),
        )
        .unwrap();
    assert!(!account.config.managed);
    engine.remove_account(&account.id).unwrap();
}
