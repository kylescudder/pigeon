//! Local mail engine. Callers do not manage IMAP sessions, credentials, SQL,
//! synchronisation cursors, or search indexes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::secrets::{KeyringState, Secret, SecretStore};
use crate::store::Store;
use crate::unlock::{UnlockOutcome, open_store};

mod cache;
mod diagnostics;
mod imap;
mod query;
pub mod source;
static NEXT_SEARCH: AtomicU64 = AtomicU64::new(0);
const MAX_PARSE_FAILURES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportSecurity {
    Tls,
    StartTls,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    pub name: String,
    pub address: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub security: TransportSecurity,
    /// Optional additional trust root. Certificate validation remains enabled.
    pub ca_certificate: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub config: AccountConfig,
    pub status: String,
    pub error: Option<String>,
    pub last_synced: Option<i64>,
    pub sync_folder: Option<String>,
    pub sync_cached: u64,
    pub sync_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Attachment {
    pub part: String,
    pub filename: String,
    pub content_type: String,
    pub size: u32,
    pub content_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageContent {
    pub sender: String,
    pub recipients: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub date: i64,
    pub message_id: String,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSummary {
    pub id: i64,
    pub account_id: String,
    pub folder: String,
    pub sender: String,
    pub recipients: String,
    pub subject: String,
    pub preview: String,
    pub date: i64,
    pub unread: bool,
    pub has_attachments: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub summary: MessageSummary,
    pub body_text: String,
    #[serde(skip_serializing)]
    pub body_html: Option<String>,
    pub has_html: bool,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchQuery {
    #[serde(default)]
    pub text: String,
    pub account_id: Option<String>,
    pub folder: Option<String>,
    #[serde(default)]
    pub unread_only: bool,
    #[serde(default)]
    pub has_attachments: bool,
    #[serde(default)]
    pub include_junk_trash: bool,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResults {
    pub messages: Vec<MessageSummary>,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSearch {
    pub id: String,
    pub name: String,
    pub query: SearchQuery,
}

#[derive(Debug, Clone, Serialize)]
pub struct Folder {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub kind: String,
    pub total: u64,
    pub cached: u64,
    pub unread: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("account settings are invalid: {0}")]
    InvalidConfig(String),
    #[error("the mail server rejected your sign-in; check the username and app password")]
    Authentication,
    #[error("could not establish a verified TLS connection to the mail server")]
    Tls,
    #[error("the mail server could not be reached or the connection was interrupted")]
    Connection,
    #[error("the mail server returned an incomplete or unsupported response")]
    Protocol,
    #[error("could not parse {0} message(s); readable mail was downloaded, but sync is incomplete")]
    IncompleteSync(usize),
    #[error(
        "the server repeatedly returned unparseable message responses; sync paused to limit retries"
    )]
    ParseRetryLimit,
    #[error("while {operation}: the server response could not be parsed by this IMAP client")]
    ResponseParse {
        operation: &'static str,
        /// Redacted protocol shape; deliberately not part of the display text.
        diagnostic: Option<String>,
    },
    #[error("while {operation}: {reason}")]
    Imap {
        operation: &'static str,
        reason: &'static str,
    },
    #[error("the desktop keyring is locked or unavailable; unlock it before continuing")]
    Locked,
    #[error("encrypted mail storage is unavailable: {0}")]
    Storage(String),
    #[error("the requested account or message is no longer available")]
    NotFound,
    #[error("this account is already synchronising")]
    Busy,
    #[error("the operation was cancelled; cached mail is still available")]
    Cancelled,
    #[error("invalid search: {0}")]
    InvalidSearch(String),
}

struct FetchContext<'a> {
    account: &'a Account,
    folder: &'a str,
    folder_kind: &'a str,
    validity: u32,
    password: &'a Secret,
    flags: &'a HashMap<u32, bool>,
    cancel: &'a AtomicBool,
}

pub struct MailEngine {
    path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    connector: Arc<dyn source::MailConnector>,
    store: Mutex<Option<Store>>,
    syncing: Mutex<HashMap<String, Arc<AtomicBool>>>,
    searches: Mutex<HashMap<String, Arc<AtomicBool>>>,
    parse_failures: Mutex<Vec<ParseFailure>>,
}

/// One message this client could not parse. Holds no folder name, UID, or
/// server text, so the formatted report is safe for the user to share.
#[derive(Debug, Clone)]
pub struct ParseFailure {
    pub folder_kind: String,
    pub operation: &'static str,
    pub diagnostic: Option<String>,
}

impl MailEngine {
    pub fn cancel_search(&self, request_id: &str) {
        if request_id.len() > 128 {
            return;
        }
        if let Ok(mut searches) = self.searches.lock() {
            // Retain early cancellation until a queued request starts. Bound
            // late-cancellation tombstones without discarding running requests.
            if searches.len() >= 2048 {
                searches.retain(|_, flag| Arc::strong_count(flag) > 1);
            }
            searches
                .entry(request_id.to_owned())
                .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                .store(true, Ordering::SeqCst);
        }
    }
    pub fn unlock(&self) -> Result<(), MailError> {
        self.secrets
            .unlock()
            .map_err(|error| MailError::Storage(error.to_string()))?;
        self.with_store(|_| Ok(()))
    }

    pub fn update_password(&self, id: &str, password: Secret) -> Result<(), MailError> {
        if password.is_empty() {
            return Err(MailError::InvalidConfig("enter an account password".into()));
        }
        let account = self
            .accounts()?
            .into_iter()
            .find(|account| account.id == id)
            .ok_or(MailError::NotFound)?;
        self.connector.connect(&account.config, &password)?;
        self.secrets
            .put(
                &format!("account:{id}:password"),
                "Pigeon mail account password",
                &password,
            )
            .map_err(|_| MailError::Locked)?;
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE mail_accounts SET last_error=NULL,sync_status='idle' WHERE id=?1",
                    [id],
                )
                .map_err(storage_error)?;
            Ok(())
        })
    }

    pub fn remove_account(&self, id: &str) -> Result<(), MailError> {
        let active = self.syncing.lock().map_err(|_| MailError::Busy)?;
        if active.contains_key(id) {
            return Err(MailError::Busy);
        }
        self.with_store(|store| {
            self.secrets
                .delete(&format!("account:{id}:password"))
                .map_err(|_| MailError::Locked)?;
            let tx = store
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            tx.execute("DELETE FROM messages WHERE account=?1", [id])
                .map_err(storage_error)?;
            tx.execute("DELETE FROM mail_accounts WHERE id=?1", [id])
                .map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        })
    }
    pub fn new(path: PathBuf, secrets: Arc<dyn SecretStore>) -> Self {
        Self::with_connector(path, secrets, Arc::new(imap::ImapConnector))
    }
    pub fn with_connector(
        path: PathBuf,
        secrets: Arc<dyn SecretStore>,
        connector: Arc<dyn source::MailConnector>,
    ) -> Self {
        Self {
            path,
            secrets,
            connector,
            store: Mutex::new(None),
            syncing: Mutex::new(HashMap::new()),
            searches: Mutex::new(HashMap::new()),
            parse_failures: Mutex::new(Vec::new()),
        }
    }

    pub fn add_account(
        &self,
        config: AccountConfig,
        password: Secret,
    ) -> Result<Account, MailError> {
        validate_config(&config)?;
        if password.is_empty() {
            return Err(MailError::InvalidConfig("enter an account password".into()));
        }
        self.with_store(|_| Ok(()))?;
        // Authentication happens outside the SQLite mutex. Browsing cached mail
        // remains available while the network is slow or unavailable.
        let mut session = self.connector.connect(&config, &password)?;
        session.folders()?;
        drop(session);
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|_| MailError::Storage("could not create account identifier".into()))?;
        let account = Account {
            id: hex::encode(bytes),
            config,
            status: "idle".into(),
            error: None,
            last_synced: None,
            sync_folder: None,
            sync_cached: 0,
            sync_total: 0,
        };
        let secret_name = format!("account:{}:password", account.id);
        self.secrets
            .put(&secret_name, "Pigeon mail account password", &password)
            .map_err(|_| MailError::Locked)?;
        if let Err(error) = self.with_store(|store| {
            let config = serde_json::to_string(&account.config)
                .map_err(|_| MailError::Storage("invalid account settings".into()))?;
            store
                .connection()
                .execute(
                    "INSERT INTO mail_accounts(id,config) VALUES (?1,?2)",
                    (&account.id, config),
                )
                .map_err(storage_error)?;
            Ok(())
        }) {
            let _ = self.secrets.delete(&secret_name);
            return Err(error);
        }
        Ok(account)
    }

    pub fn accounts(&self) -> Result<Vec<Account>, MailError> {
        self.with_store(|store| {
            let mut statement = store.connection().prepare("SELECT id,config,sync_status,last_error,last_synced,sync_folder,sync_cached,sync_total FROM mail_accounts ORDER BY rowid").map_err(storage_error)?;
            let rows = statement.query_map([], |row| {
                let json: String = row.get(1)?;
                let config: AccountConfig = serde_json::from_str(&json).map_err(|error| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error)))?;
                Ok(Account { id: row.get(0)?, config, status: row.get(2)?, error: row.get(3)?, last_synced: row.get(4)?, sync_folder: row.get(5)?, sync_cached: row.get::<_,i64>(6)? as u64, sync_total: row.get::<_,i64>(7)? as u64 })
            }).map_err(storage_error)?;
            rows.map(|row| row.map_err(storage_error)).collect()
        })
    }

    pub fn sync_account(&self, id: &str) -> Result<(), MailError> {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.syncing.lock().map_err(|_| MailError::Busy)?;
            if active.contains_key(id) {
                return Err(MailError::Busy);
            }
            active.insert(id.to_owned(), cancel.clone());
        }
        let _lease = SyncLease {
            active: &self.syncing,
            id: id.to_owned(),
        };
        let result = self.sync_inner(id, &cancel);
        let now = now();
        let error = result.as_ref().err().map(ToString::to_string);
        let status = match &result {
            Ok(_) => "idle",
            Err(MailError::Cancelled) => "cancelled",
            Err(_) => "error",
        };
        let persisted = self.with_store(|store| {
            store.connection().execute("UPDATE mail_accounts SET sync_status=?2,last_error=?3,last_synced=CASE WHEN ?2='idle' THEN ?4 ELSE last_synced END WHERE id=?1", rusqlite::params![id,status,error,now]).map_err(storage_error)?;
            Ok(())
        });
        result.and(persisted)
    }

    pub fn cancel_sync(&self, id: &str) {
        if let Ok(active) = self.syncing.lock()
            && let Some(cancel) = active.get(id)
        {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    fn sync_inner(&self, id: &str, cancel: &AtomicBool) -> Result<(), MailError> {
        if let Ok(mut failures) = self.parse_failures.lock() {
            failures.clear();
        }
        let account = self
            .accounts()?
            .into_iter()
            .find(|account| account.id == id)
            .ok_or(MailError::NotFound)?;
        self.with_store(|store| { store.connection().execute("UPDATE mail_accounts SET sync_status='syncing',last_error=NULL,sync_folder=NULL WHERE id=?1", [id]).map_err(storage_error)?; Ok(()) })?;
        let password = self
            .secrets
            .get(&format!("account:{id}:password"))
            .map_err(|_| MailError::Locked)?
            .ok_or(MailError::Authentication)?;
        let mut session = self.connector.connect(&account.config, &password)?;
        let folders = session.folders()?;
        let mut failed_messages = 0;
        for folder in &folders {
            let failures_before = failed_messages;
            check_cancel(cancel)?;
            let snapshot = session.snapshot(&folder.name)?;
            if snapshot.uid_validity == 0 {
                return Err(MailError::Protocol);
            }
            let mut unique = HashSet::new();
            if snapshot
                .messages
                .iter()
                .any(|message| message.uid == 0 || !unique.insert(message.uid))
            {
                return Err(MailError::Protocol);
            }
            let known = self.prepare_folder(id, folder, &snapshot)?;
            let needed: Vec<u32> = snapshot
                .messages
                .iter()
                .filter(|message| !known.contains(&message.uid))
                .map(|message| message.uid)
                .collect();
            let flags: HashMap<u32, bool> = snapshot
                .messages
                .iter()
                .map(|message| (message.uid, message.unread))
                .collect();
            let context = FetchContext {
                account: &account,
                folder: &folder.name,
                folder_kind: &folder.kind,
                validity: snapshot.uid_validity,
                password: &password,
                flags: &flags,
                cancel,
            };
            for batch in needed.chunks(100) {
                let mut attempts = 0;
                self.fetch_batch(
                    &context,
                    &mut session,
                    batch,
                    &mut failed_messages,
                    &mut attempts,
                )?;
            }
            check_cancel(cancel)?;
            // A partial folder must not prune old data or promote a partially
            // downloaded UIDVALIDITY epoch over the previous readable cache.
            if failed_messages == failures_before {
                self.complete_folder(id, &folder.name, &snapshot)?;
            }
        }
        if failed_messages > 0 {
            return Err(MailError::IncompleteSync(failed_messages));
        }
        check_cancel(cancel)?;
        self.remove_absent_folders(id, &folders)?;
        Ok(())
    }

    fn record_parse_failure(&self, failure: ParseFailure) {
        if let Ok(mut failures) = self.parse_failures.lock()
            && failures.len() < MAX_PARSE_FAILURES
        {
            failures.push(failure);
        }
    }

    /// Syncs every account and returns a report safe to share: failure
    /// categories and redacted protocol shape, never mail, addresses, folder
    /// names, or credentials. Only uncached messages are fetched.
    pub fn diagnose_sync(&self) -> Result<String, MailError> {
        let mut report = String::from(
            "Pigeon sync diagnostic. Contains no message text, addresses, folder names, or credentials.\n",
        );
        for (index, account) in self.accounts()?.iter().enumerate() {
            let outcome = self.sync_account(&account.id);
            let outcome = outcome.map_or_else(|error| error.to_string(), |()| "complete".into());
            report.push_str(&format!("\nAccount {}: {outcome}\n", index + 1));
            let failures = self
                .parse_failures
                .lock()
                .map_err(|_| MailError::Busy)?
                .clone();
            for (index, failure) in failures.iter().enumerate() {
                report.push_str(&format!(
                    "Failure {} in a folder of type {}, while {}:\n{}\n",
                    index + 1,
                    failure.folder_kind,
                    failure.operation,
                    failure
                        .diagnostic
                        .as_deref()
                        .unwrap_or("no detail available"),
                ));
            }
        }
        Ok(report)
    }

    fn fetch_batch(
        &self,
        context: &FetchContext<'_>,
        session: &mut Box<dyn source::MailSession>,
        batch: &[u32],
        failed: &mut usize,
        attempts: &mut usize,
    ) -> Result<(), MailError> {
        check_cancel(context.cancel)?;
        // Binary splitting is exceptional recovery, never an unbounded retry
        // loop. Typical single-message failures need ~15 requests for 100 UIDs.
        if *attempts >= 31 {
            return Err(MailError::ParseRetryLimit);
        }
        *attempts += 1;
        match session.fetch_messages(batch) {
            Err(MailError::ResponseParse {
                operation,
                diagnostic,
            }) => {
                if batch.len() == 1 {
                    self.record_parse_failure(ParseFailure {
                        folder_kind: context.folder_kind.to_owned(),
                        operation,
                        diagnostic,
                    });
                }
                check_cancel(context.cancel)?;
                // A parser failure may leave unread bytes in the stream.
                // Reconnect and verify the folder epoch before any further fetch.
                *session = self
                    .connector
                    .connect(&context.account.config, context.password)?;
                session.reselect(context.folder, context.validity)?;
                if batch.len() == 1 {
                    *failed += 1;
                    return Ok(());
                }
                let middle = batch.len() / 2;
                self.fetch_batch(context, session, &batch[..middle], failed, attempts)?;
                self.fetch_batch(context, session, &batch[middle..], failed, attempts)
            }
            Err(error) => Err(error),
            Ok(messages) => {
                let mut received = HashSet::new();
                if messages.len() != batch.len()
                    || messages.iter().any(|message| {
                        !batch.contains(&message.uid) || !received.insert(message.uid)
                    })
                {
                    return Err(MailError::Protocol);
                }
                check_cancel(context.cancel)?;
                self.persist_batch(
                    &context.account.id,
                    context.folder,
                    context.validity,
                    &messages,
                    context.flags,
                )
            }
        }
    }

    fn with_store<T>(
        &self,
        f: impl FnOnce(&Store) -> Result<T, MailError>,
    ) -> Result<T, MailError> {
        let mut guard = self
            .store
            .lock()
            .map_err(|_| MailError::Storage("mail store lock unavailable".into()))?;
        if self.secrets.state() != KeyringState::Unlocked {
            *guard = None;
            return Err(MailError::Locked);
        }
        if guard.is_none() {
            match open_store(self.secrets.as_ref(), &self.path) {
                UnlockOutcome::Opened { store, .. } => {
                    store.connection().execute("UPDATE mail_accounts SET sync_status='interrupted', last_error='Sync was interrupted; reconnect to resume' WHERE sync_status='syncing'", []).map_err(storage_error)?;
                    *guard = Some(store);
                }
                UnlockOutcome::Blocked(reason) => {
                    return Err(MailError::Storage(format!("{reason:?}")));
                }
            }
        }
        f(guard
            .as_ref()
            .ok_or_else(|| MailError::Storage("mail store unavailable".into()))?)
    }
}

struct SyncLease<'a> {
    active: &'a Mutex<HashMap<String, Arc<AtomicBool>>>,
    id: String,
}
impl Drop for SyncLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.id);
        }
    }
}
fn check_cancel(cancel: &AtomicBool) -> Result<(), MailError> {
    if cancel.load(Ordering::SeqCst) {
        Err(MailError::Cancelled)
    } else {
        Ok(())
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn storage_error(error: rusqlite::Error) -> MailError {
    MailError::Storage(error.to_string())
}

fn validate_config(config: &AccountConfig) -> Result<(), MailError> {
    if config.name.trim().is_empty() || config.name.len() > 120 {
        return Err(MailError::InvalidConfig(
            "enter an account name of up to 120 characters".into(),
        ));
    }
    if config.address.trim().is_empty()
        || !config.address.contains('@')
        || config.address.contains(['\r', '\n', '\0'])
    {
        return Err(MailError::InvalidConfig("enter an email address".into()));
    }
    if config.host.trim().is_empty()
        || config.host.contains("://")
        || config.host.chars().any(char::is_whitespace)
        || config.host.contains(['\0', '/'])
        || config.port == 0
    {
        return Err(MailError::InvalidConfig(
            "enter a server hostname and valid port".into(),
        ));
    }
    if config.username.trim().is_empty() || config.username.contains(['\r', '\n', '\0']) {
        return Err(MailError::InvalidConfig("enter a username".into()));
    }
    Ok(())
}
