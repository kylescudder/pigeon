//! Accounts declared outside the app, read from `$XDG_CONFIG_HOME/pigeon/accounts.json`.
//!
//! The Home Manager module writes the file from `accounts.email.accounts`. It
//! holds server settings only; passwords stay in the keyring and are entered in
//! the app. A declared account gets a stable identity from its key, so its
//! password and cached mail survive a rebuild. An account that leaves the file
//! becomes an ordinary account again: a configuration slip never deletes mail.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::params;
use serde::Deserialize;

use super::smtp::SmtpConfig;
use super::{
    Account, AccountConfig, MailEngine, MailError, SendingSettings, TransportSecurity,
    storage_error, validate_config,
};

pub const FILE_NAME: &str = "accounts.json";
const VERSION: u32 = 1;
const MAX_NAME: usize = 64;

pub fn accounts_path(config_dir: &Path) -> PathBuf {
    config_dir.join("pigeon").join(FILE_NAME)
}

/// The stored identity of a declared account.
pub fn managed_id(name: &str) -> String {
    format!("managed-{name}")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    version: u32,
    accounts: BTreeMap<String, Declared>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Declared {
    address: String,
    #[serde(default)]
    real_name: String,
    user_name: String,
    imap: Server,
    smtp: Option<Server>,
    /// Path to a PEM file trusted in addition to the system roots.
    ca_certificate: Option<PathBuf>,
    #[serde(default)]
    signature: String,
}

#[derive(Debug, Deserialize)]
struct Server {
    host: String,
    port: u16,
    security: TransportSecurity,
}

/// An account as declared, ready to compare with the stored one.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedAccount {
    pub id: String,
    pub config: AccountConfig,
}

#[derive(Debug, Default)]
pub struct Declaration {
    pub accounts: Vec<ManagedAccount>,
    /// Entries or the whole file that could not be used, in words for the user.
    pub warnings: Vec<String>,
    /// The file could not be read as a whole, so nothing may be unmanaged.
    pub unreadable: bool,
}

/// Reads the declaration. A missing file declares no accounts; an unreadable
/// one declares nothing and changes nothing.
pub fn load(path: &Path) -> Declaration {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Declaration::default();
        }
        Err(error) => return unreadable(path, &error.to_string()),
    };
    let file: File = match serde_json::from_str(&text) {
        Ok(file) => file,
        Err(error) => return unreadable(path, &error.to_string()),
    };
    if file.version != VERSION {
        return unreadable(
            path,
            &format!(
                "it is version {}, but this Pigeon reads version {VERSION}",
                file.version
            ),
        );
    }
    let mut declaration = Declaration::default();
    for (name, declared) in file.accounts {
        match convert(&name, declared) {
            Ok(account) => declaration.accounts.push(account),
            Err(reason) => declaration
                .warnings
                .push(format!("Account “{name}” in {FILE_NAME}: {reason}")),
        }
    }
    declaration
}

fn unreadable(path: &Path, reason: &str) -> Declaration {
    Declaration {
        warnings: vec![format!(
            "{} could not be used ({reason}); managed accounts are unchanged",
            path.display()
        )],
        unreadable: true,
        ..Declaration::default()
    }
}

fn convert(name: &str, declared: Declared) -> Result<ManagedAccount, String> {
    if name.is_empty()
        || name.len() > MAX_NAME
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "use a name of up to {MAX_NAME} letters, digits, - or _"
        ));
    }
    let ca_certificate = declared
        .ca_certificate
        .map(|path| {
            std::fs::read_to_string(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))
        })
        .transpose()?;
    let smtp = declared.smtp.map(|server| SmtpConfig {
        host: server.host,
        port: server.port,
        security: server.security,
        username: declared.user_name.clone(),
    });
    let config = AccountConfig {
        name: name.to_owned(),
        address: declared.address,
        host: declared.imap.host,
        port: declared.imap.port,
        username: declared.user_name,
        security: declared.imap.security,
        ca_certificate,
        sending: SendingSettings {
            smtp,
            display_name: declared.real_name,
            signature: declared.signature,
            ..SendingSettings::default()
        },
        managed: true,
    };
    validate_config(&config).map_err(|error| error.to_string())?;
    super::sending::validate_sending(&config.sending).map_err(|error| error.to_string())?;
    Ok(ManagedAccount {
        id: managed_id(name),
        config,
    })
}

/// What the declaration changes about an existing account. The Sent-copy
/// preference is not declared, so it stays whatever the user chose.
fn merged(declared: &AccountConfig, existing: &AccountConfig) -> AccountConfig {
    let mut config = declared.clone();
    config.sending.save_sent_copy = existing.sending.save_sent_copy;
    config
}

const PASSWORD_NEEDED: &str = "enter this account’s password to start syncing";

impl MailEngine {
    /// Brings stored accounts in line with the declaration: declared accounts
    /// are created without a password or updated in place, and managed
    /// accounts no longer declared become ordinary accounts.
    pub fn reconcile_managed(&self, declaration: &Declaration) -> Result<(), MailError> {
        let _lease = self.access_lease("managed-accounts")?;
        let existing = self.accounts()?;
        for declared in &declaration.accounts {
            match existing.iter().find(|account| account.id == declared.id) {
                None => self.insert_managed(declared)?,
                Some(current) => {
                    let config = merged(&declared.config, &current.config);
                    if config != current.config {
                        self.store_config(&current.id, &config)?;
                    }
                }
            }
        }
        if declaration.unreadable {
            return Ok(());
        }
        for account in existing.iter().filter(|account| account.config.managed) {
            if !declaration
                .accounts
                .iter()
                .any(|declared| declared.id == account.id)
            {
                let mut config = account.config.clone();
                config.managed = false;
                self.store_config(&account.id, &config)?;
            }
        }
        Ok(())
    }

    fn insert_managed(&self, declared: &ManagedAccount) -> Result<(), MailError> {
        let config = serde_json::to_string(&declared.config)
            .map_err(|_| MailError::Storage("invalid account settings".into()))?;
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO mail_accounts(id,config,sync_status,last_error) VALUES (?1,?2,'error',?3)",
                    params![declared.id, config, PASSWORD_NEEDED],
                )
                .map_err(storage_error)?;
            Ok(())
        })
    }

    fn store_config(&self, id: &str, config: &AccountConfig) -> Result<(), MailError> {
        let config = serde_json::to_string(config)
            .map_err(|_| MailError::Storage("invalid account settings".into()))?;
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE mail_accounts SET config=?2 WHERE id=?1",
                    params![id, config],
                )
                .map_err(storage_error)?;
            Ok(())
        })
    }

    /// Refuses a change to an account whose settings are declared elsewhere.
    pub(super) fn refuse_if_managed(&self, id: &str) -> Result<(), MailError> {
        let account: Option<Account> = self.accounts()?.into_iter().find(|a| a.id == id);
        match account {
            Some(account) if account.config.managed => Err(MailError::Conflict(
                "this account is managed by Home Manager; change it in your Nix configuration"
                    .into(),
            )),
            _ => Ok(()),
        }
    }
}
