//! Local mail engine. Callers do not manage IMAP sessions, credentials, SQL,
//! synchronisation cursors, or search indexes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::backup::{BackupError, password_name};
use crate::secrets::{KeyringState, Secret, SecretStore};
use crate::store::Store;
use crate::unlock::{StoreBlocked, UnlockOutcome, open_store};

pub use attachments::{AttachmentData, MAX_ATTACHMENT_BYTES, ViewerDocument};

mod actions;
mod attachments;
mod cache;
mod diagnostics;
mod imap;
pub mod managed;
mod net;
pub mod operations;
mod query;
mod recovery;
pub mod reply;
mod sending;
pub mod smtp;
pub mod source;
pub use actions::{ActionCounts, ActionKind, PendingAction};
pub use sending::{
    Draft, DraftInput, OutboxItem, SendDecision, SendState, SendStatus, SendingSettings,
    SentCopyStatus,
};
static NEXT_ACCESS: AtomicU64 = AtomicU64::new(0);
static NEXT_SEARCH: AtomicU64 = AtomicU64::new(0);
const MAX_PARSE_FAILURES: usize = 20;
const REMOVAL_BATCH: i64 = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportSecurity {
    Tls,
    StartTls,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountConfig {
    pub name: String,
    pub address: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub security: TransportSecurity,
    /// Optional additional trust root. Certificate validation remains enabled.
    pub ca_certificate: Option<String>,
    /// Outgoing server and identity. Absent in accounts added before sending.
    #[serde(default)]
    pub sending: SendingSettings,
    /// Declared in `accounts.json` (Home Manager): the app cannot edit or
    /// remove it, and its password is still entered here.
    #[serde(default)]
    pub managed: bool,
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
    /// The Reply-To header, when the message has one.
    pub reply_to: String,
    /// The References header: Message-IDs of the conversation so far.
    pub references: String,
    /// The To and Cc headers alone. `recipients` also holds Bcc.
    pub to_list: String,
    pub cc_list: String,
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
    pub delimiter: Option<String>,
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
    #[error("enter this account’s password to start syncing")]
    PasswordNeeded,
    #[error("the desktop keyring is locked or unavailable; unlock it before continuing")]
    Locked,
    #[error("encrypted mail storage is unavailable: {0}")]
    Storage(String),
    #[error("the requested account or message is no longer available")]
    NotFound,
    #[error("this account is already synchronising")]
    Busy,
    #[error("a backup is being restored; try again when it finishes")]
    Restoring,
    #[error("the operation was cancelled; cached mail is still available")]
    Cancelled,
    #[error("invalid search: {0}")]
    InvalidSearch(String),
    #[error(
        "this device's mail store key is missing from the keyring; restore a backup to recover local data"
    )]
    KeyMissing,
    #[error(transparent)]
    Backup(#[from] BackupError),
    #[error("the mail server does not support {0} safely")]
    Unsupported(&'static str),
    #[error("this account has no Trash folder; delete permanently instead")]
    NoTrashFolder,
    #[error("this account has no Archive folder")]
    NoArchiveFolder,
    #[error("{0}")]
    InvalidAction(&'static str),
    #[error("{0}")]
    Conflict(String),
    #[error("while {operation}: {reason}")]
    Smtp {
        operation: &'static str,
        /// A 4xx reply: the same request may succeed later.
        temporary: bool,
        /// Our description and the reply code; never the server's text.
        reason: String,
    },
}

impl MailError {
    /// The server answered the command with NO or BAD: retrying unchanged
    /// will not help, unlike a dropped connection.
    fn is_rejection(&self) -> bool {
        matches!(self, MailError::Imap { reason, .. } if reason.contains("IMAP NO") || reason.contains("IMAP BAD"))
            || matches!(self, MailError::Unsupported(_))
    }
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
    smtp: Arc<dyn smtp::SmtpTransport>,
    store: Mutex<Option<Store>>,
    syncing: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Serialises backups and restores with one another.
    recovery: Mutex<()>,
    searches: Mutex<HashMap<String, Arc<AtomicBool>>>,
    parse_failures: Mutex<Vec<ParseFailure>>,
    /// Downloaded but not kept attachment payloads, in memory for this
    /// session only. Cleared whenever the store becomes unavailable.
    parts: Mutex<attachments::PartCache>,
    operations: Mutex<operations::Runtime>,
    operations_signal: (Mutex<bool>, Condvar),
    /// Unread Inbox mail stored since the last `take_arrivals`, per account.
    arrivals: Mutex<HashMap<String, cache::Arrived>>,
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
        // Excludes a restore, which would replace this account's password and
        // settings underneath the update. Syncing the account may continue.
        let _lease = self.lease(&format!("\0password:{id}"))?;
        let account = self
            .accounts()?
            .into_iter()
            .find(|account| account.id == id)
            .ok_or(MailError::NotFound)?;
        self.connector.connect(&account.config, &password)?;
        self.secrets
            .put(
                &password_name(id),
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
        let _lease = self.lease(id)?;
        self.refuse_if_managed(id)?;
        self.check_no_unsent(id)?;
        // Mail goes first, in batches: each row also rewrites the search index,
        // so one transaction would hold the store and freeze every other view.
        // The account and password outlast it, so any failure can be retried.
        loop {
            let removed = self.with_store(|store| {
                store
                    .connection()
                    .execute(
                        "DELETE FROM messages WHERE id IN (SELECT id FROM messages WHERE account=?1 LIMIT ?2)",
                        rusqlite::params![id, REMOVAL_BATCH],
                    )
                    .map_err(storage_error)
            })?;
            if removed == 0 {
                break;
            }
            // The store mutex is not fair; give waiting readers a turn.
            std::thread::sleep(Duration::from_millis(1));
        }
        self.forget_session_parts(Some(id));
        self.secrets
            .delete(&password_name(id))
            .map_err(|_| MailError::Locked)?;
        self.with_store(|store| {
            store
                .connection()
                .execute("DELETE FROM mail_accounts WHERE id=?1", [id])
                .map_err(storage_error)?;
            // Only settled messages remain; unsent ones blocked removal above.
            store
                .connection()
                .execute("DELETE FROM outbox WHERE account=?1", [id])
                .map_err(storage_error)?;
            Ok(())
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
        Self::with_transports(
            path,
            secrets,
            connector,
            Arc::new(smtp::SmtpClient::default()),
        )
    }
    /// The real IMAP connector with a chosen SMTP transport.
    pub fn with_smtp(
        path: PathBuf,
        secrets: Arc<dyn SecretStore>,
        smtp: Arc<dyn smtp::SmtpTransport>,
    ) -> Self {
        Self::with_transports(path, secrets, Arc::new(imap::ImapConnector), smtp)
    }
    pub fn with_transports(
        path: PathBuf,
        secrets: Arc<dyn SecretStore>,
        connector: Arc<dyn source::MailConnector>,
        smtp: Arc<dyn smtp::SmtpTransport>,
    ) -> Self {
        let mut runtime = operations::Runtime::default();
        for handler in [
            Arc::new(sending::SubmitHandler) as Arc<dyn operations::OperationHandler>,
            Arc::new(sending::SentCopyHandler),
        ] {
            runtime.handlers.insert(handler.kind(), handler);
        }
        Self {
            path,
            secrets,
            connector,
            smtp,
            store: Mutex::new(None),
            syncing: Mutex::new(HashMap::new()),
            recovery: Mutex::new(()),
            searches: Mutex::new(HashMap::new()),
            parse_failures: Mutex::new(Vec::new()),
            parts: Mutex::new(attachments::PartCache::default()),
            operations: Mutex::new(runtime),
            operations_signal: (Mutex::new(false), Condvar::new()),
            arrivals: Mutex::new(HashMap::new()),
        }
    }

    pub fn add_account(
        &self,
        mut config: AccountConfig,
        password: Secret,
    ) -> Result<Account, MailError> {
        // Only the declaration file can make an account managed.
        config.managed = false;
        validate_config(&config)?;
        sending::validate_sending(&config.sending)?;
        if password.is_empty() {
            return Err(MailError::InvalidConfig("enter an account password".into()));
        }
        self.with_store(|_| Ok(()))?;
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|_| MailError::Storage("could not create account identifier".into()))?;
        let id = hex::encode(bytes);
        // Held until the account row is written, so a restore cannot move the
        // store aside between saving the password and saving the account.
        let _lease = self.lease(&format!("\0add:{id}"))?;
        // Authentication happens outside the SQLite mutex. Browsing cached mail
        // remains available while the network is slow or unavailable.
        let mut session = self.connector.connect(&config, &password)?;
        session.folders()?;
        drop(session);
        if let Some(outgoing) = &config.sending.smtp {
            self.smtp.verify(
                &smtp::SmtpServer {
                    config: outgoing,
                    ca_certificate: config.ca_certificate.as_deref(),
                },
                &password,
            )?;
        }
        let account = Account {
            id,
            config,
            status: "idle".into(),
            error: None,
            last_synced: None,
            sync_folder: None,
            sync_cached: 0,
            sync_total: 0,
        };
        let secret_name = password_name(&account.id);
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
        let lease = self.lease(id)?;
        let result = self.sync_inner(id, &lease.cancel);
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
            .get(&password_name(id))
            .map_err(|_| MailError::Locked)?
            .ok_or(if account.config.managed {
                MailError::PasswordNeeded
            } else {
                MailError::Authentication
            })?;
        // Queued changes go first, so the snapshots below already reflect them.
        self.apply_queue(&account, &password)?;
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
        // Changes queued while this sync ran.
        self.apply_queue(&account, &password)
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
                    context.folder_kind == "inbox",
                )
            }
        }
    }

    fn with_store<T>(
        &self,
        f: impl FnOnce(&Store) -> Result<T, MailError>,
    ) -> Result<T, MailError> {
        let _access = self.access_lease("store")?;
        let mut guard = self
            .store
            .lock()
            .map_err(|_| MailError::Storage("mail store lock unavailable".into()))?;
        if self.secrets.state() != KeyringState::Unlocked {
            *guard = None;
            self.forget_session_parts(None);
            return Err(MailError::Locked);
        }
        if guard.is_none() {
            match open_store(self.secrets.as_ref(), &self.path) {
                UnlockOutcome::Opened { store, .. } => {
                    store.connection().execute("UPDATE mail_accounts SET sync_status='interrupted', last_error='Sync was interrupted; reconnect to resume' WHERE sync_status='syncing'", []).map_err(storage_error)?;
                    // A change cut off mid-apply is re-checked against the
                    // server before anything is written again.
                    store
                        .connection()
                        .execute(
                            "UPDATE mail_actions SET state='queued' WHERE state='applying'",
                            [],
                        )
                        .map_err(storage_error)?;
                    self.reconcile_operations(store.connection())?;
                    *guard = Some(store);
                }
                UnlockOutcome::Blocked(StoreBlocked::KeyMissing) => {
                    return Err(MailError::KeyMissing);
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

/// Held for the duration of an account sync or removal, or of a restore
/// (under [`RESTORE_LEASE`]). A restore excludes every other lease.
struct SyncLease<'a> {
    active: &'a Mutex<HashMap<String, Arc<AtomicBool>>>,
    id: String,
    cancel: Arc<AtomicBool>,
}

const RESTORE_LEASE: &str = "\0restore";

impl MailEngine {
    fn access_lease(&self, purpose: &str) -> Result<SyncLease<'_>, MailError> {
        self.lease(&format!(
            "\0{purpose}:{}",
            NEXT_ACCESS.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn lease(&self, id: &str) -> Result<SyncLease<'_>, MailError> {
        let mut active = self.syncing.lock().map_err(|_| MailError::Busy)?;
        if active.contains_key(RESTORE_LEASE) {
            return Err(MailError::Restoring);
        }
        if active.contains_key(id) || (id == RESTORE_LEASE && !active.is_empty()) {
            return Err(MailError::Busy);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        active.insert(id.to_owned(), cancel.clone());
        Ok(SyncLease {
            active: &self.syncing,
            id: id.to_owned(),
            cancel,
        })
    }
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
