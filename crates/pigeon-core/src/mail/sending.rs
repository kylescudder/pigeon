//! Local drafts, the outbox, and the two queued steps of sending a message:
//! SMTP submission, then saving a copy to the Sent folder. The second step
//! can fail and be retried on its own; it never submits the message again.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::operations::{self, NewOperation, Operation, OperationContext, OperationState, Outcome};
use super::smtp::{Envelope, SmtpConfig, SmtpServer, Submission};
use super::{Account, MailEngine, MailError, storage_error};

pub(crate) const SUBMIT: &str = "smtp_submit";
pub(crate) const SENT_COPY: &str = "sent_copy";
const DEFAULT_UNDO_SECONDS: u32 = 10;
const MAX_UNDO_SECONDS: u32 = 60;
const MAX_BODY: usize = 5 * 1024 * 1024;
const MAX_FIELD: usize = 64 * 1024;
/// After this many tries a Sent copy stops retrying on its own.
const SENT_COPY_ATTEMPTS: u32 = 6;

/// A named plain-text signature saved with an account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedSignature {
    pub id: String,
    pub name: String,
    pub body: String,
}

/// How an account sends mail. Stored inside the account settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SendingSettings {
    pub smtp: Option<SmtpConfig>,
    /// The name recipients see. Empty means the address alone.
    pub display_name: String,
    /// Plain text appended below a standard "-- " separator.
    pub signature: String,
    /// Alternatives; `signature` remains the default for older accounts.
    pub signatures: Vec<SavedSignature>,
    /// Off for providers that file sent mail themselves.
    pub save_sent_copy: bool,
}

impl Default for SendingSettings {
    fn default() -> Self {
        Self {
            smtp: None,
            display_name: String::new(),
            signature: String::new(),
            signatures: Vec::new(),
            save_sent_copy: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct DraftInput {
    pub id: Option<String>,
    pub account_id: Option<String>,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    /// None uses the account default; Some("") omits the signature.
    pub signature: Option<String>,
    /// Message-ID of the message this replies to, without angle brackets.
    pub in_reply_to: String,
    /// Message-IDs of the conversation so far, space separated.
    pub references: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Draft {
    pub id: String,
    pub account_id: Option<String>,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub signature: Option<String>,
    pub in_reply_to: String,
    pub references: String,
    pub updated: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SendStatus {
    /// Waiting for the undo delay, or to retry after a temporary failure.
    Queued {
        send_at: i64,
        error: Option<String>,
    },
    Sending,
    Accepted {
        at: i64,
    },
    Failed {
        error: String,
    },
    /// The server may have accepted it. Needs a decision.
    Unknown {
        error: String,
    },
    /// Paused, for example after restoring a backup.
    Held,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SentCopyStatus {
    NotRequested,
    /// Queued, or retrying on its own after `error`.
    Waiting {
        error: Option<String>,
    },
    Saving,
    Saved,
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct OutboxItem {
    pub id: String,
    pub account_id: String,
    pub subject: String,
    pub recipients: String,
    pub created: i64,
    pub send: SendStatus,
    pub sent_copy: SentCopyStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct SavedRecipient {
    pub email: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendState {
    pub drafts: Vec<Draft>,
    pub outbox: Vec<OutboxItem>,
    pub undo_send_seconds: u32,
    pub recipients: Vec<SavedRecipient>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendDecision {
    /// Submit again, accepting that recipients may get it twice.
    SendAgain,
    /// Treat it as sent, then save the Sent copy.
    MarkSent,
    /// Stop tracking it.
    Discard,
}

#[derive(Serialize, Deserialize)]
struct OutboxPayload {
    outbox: String,
}

struct OutboxRow {
    account: String,
    draft: String,
    envelope: Envelope,
    message: Vec<u8>,
    sent_copy: Vec<u8>,
    message_id: String,
    submit_op: String,
    sent_copy_op: Option<String>,
}

fn random_id() -> Result<String, MailError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MailError::Storage("could not create an identifier".into()))?;
    Ok(hex::encode(bytes))
}

pub(super) fn invalid(message: &str) -> MailError {
    MailError::InvalidConfig(message.into())
}

fn check_draft(draft: &DraftInput) -> Result<(), MailError> {
    if draft
        .signature
        .as_ref()
        .is_some_and(|text| text.len() > 10_000)
    {
        return Err(invalid("the signature is too long"));
    }
    if draft.body.len() > MAX_BODY {
        return Err(invalid("the message is too long"));
    }
    if [&draft.to, &draft.cc, &draft.bcc, &draft.subject]
        .iter()
        .any(|field| field.len() > MAX_FIELD)
    {
        return Err(invalid("a recipient or subject field is too long"));
    }
    Ok(())
}

/// Parses a comma-separated recipient field into bare addresses and the
/// display form used in headers.
pub(super) fn parse_recipients(
    field: &str,
    label: &str,
) -> Result<Vec<(Option<String>, String)>, MailError> {
    if field.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parsed = mailparse::addrparse(field)
        .map_err(|_| MailError::InvalidConfig(format!("check the {label} addresses")))?;
    let mut out = Vec::new();
    for entry in parsed.iter() {
        let singles = match entry {
            mailparse::MailAddr::Single(single) => vec![single.clone()],
            mailparse::MailAddr::Group(group) => group.addrs.clone(),
        };
        for single in singles {
            let address = single.addr.trim().to_owned();
            let valid = address.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty() && (domain.contains('.') || domain == "localhost")
            }) && !address
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | '"'));
            if !valid {
                return Err(MailError::InvalidConfig(format!(
                    "{address} in {label} is not a valid email address"
                )));
            }
            let name = single
                .display_name
                .map(|name| name.replace(['\r', '\n'], " ").trim().to_owned())
                .filter(|name| !name.is_empty());
            out.push((name, address));
        }
    }
    Ok(out)
}

pub(super) fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_owned()
}

fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

struct Built {
    envelope: Envelope,
    message: Vec<u8>,
    sent_copy: Vec<u8>,
    message_id: String,
    subject: String,
    recipients: String,
}

fn build(account: &Account, draft: &DraftInput, now_secs: i64) -> Result<Built, MailError> {
    let sending = &account.config.sending;
    let to = parse_recipients(&draft.to, "To")?;
    let cc = parse_recipients(&draft.cc, "Cc")?;
    let bcc = parse_recipients(&draft.bcc, "Bcc")?;
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        return Err(invalid("add at least one recipient"));
    }
    let from_address = account.config.address.trim().to_owned();
    let domain = from_address
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_owned())
        .ok_or_else(|| invalid("the account address is not valid"))?;
    let message_id = format!("{}@{domain}", random_id()?);
    let subject = single_line(&draft.subject);
    let mut body = draft.body.clone();
    let signature = draft.signature.as_deref().unwrap_or(&sending.signature);
    if !signature.trim().is_empty() {
        body = format!("{}\n\n-- \n{}", body.trim_end(), signature.trim_end());
    }
    let body = crlf(&body);
    let display_name = single_line(&sending.display_name);
    let in_reply_to = header_id(&draft.in_reply_to);
    let references = header_ids(&draft.references);

    let addresses =
        |list: &[(Option<String>, String)]| -> mail_builder::headers::address::Address<'static> {
            mail_builder::headers::address::Address::new_list(
                list.iter()
                    .map(|(name, address)| {
                        mail_builder::headers::address::Address::new_address(
                            name.clone(),
                            address.clone(),
                        )
                    })
                    .collect(),
            )
        };
    let write = |with_bcc: bool| -> Result<Vec<u8>, MailError> {
        let from = mail_builder::headers::address::Address::new_address(
            (!display_name.is_empty()).then(|| display_name.clone()),
            from_address.clone(),
        );
        let mut builder = mail_builder::MessageBuilder::new()
            .from(from)
            .subject(subject.clone())
            .message_id(message_id.clone())
            .date(now_secs)
            .text_body(body.clone());
        if !in_reply_to.is_empty() {
            builder = builder.in_reply_to(in_reply_to.clone());
        }
        if !references.is_empty() {
            builder = builder.references(references.clone());
        }
        if !to.is_empty() {
            builder = builder.to(addresses(&to));
        }
        if !cc.is_empty() {
            builder = builder.cc(addresses(&cc));
        }
        if with_bcc && !bcc.is_empty() {
            builder = builder.bcc(addresses(&bcc));
        }
        builder
            .write_to_vec()
            .map_err(|_| MailError::Storage("could not build the message".into()))
    };

    let mut recipients: Vec<String> = Vec::new();
    for (_, address) in to.iter().chain(&cc).chain(&bcc) {
        if !recipients
            .iter()
            .any(|known| known.eq_ignore_ascii_case(address))
        {
            recipients.push(address.clone());
        }
    }
    let shown: Vec<String> = to
        .iter()
        .chain(&cc)
        .map(|(name, address)| name.clone().unwrap_or_else(|| address.clone()))
        .collect();
    let message = write(false)?;
    let sent_copy = write(true)?;
    Ok(Built {
        envelope: Envelope {
            from: from_address,
            recipients,
        },
        message,
        sent_copy,
        message_id,
        subject,
        recipients: if shown.is_empty() {
            "Bcc recipients".into()
        } else {
            shown.join(", ")
        },
    })
}

/// One Message-ID for a threading header: no angle brackets, spaces or control
/// characters, so a draft cannot inject headers.
fn header_id(value: &str) -> String {
    value
        .trim()
        .trim_matches(['<', '>'])
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>'))
        .collect()
}

/// Several Message-IDs, separated by spaces, the oldest first.
fn header_ids(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(header_id)
        .filter(|id| !id.is_empty())
        .collect()
}

fn draft_from_payload(id: &str, payload: &str, updated: i64) -> Result<Draft, MailError> {
    let input: DraftInput = serde_json::from_str(payload)
        .map_err(|_| MailError::Storage("a saved draft could not be read".into()))?;
    Ok(Draft {
        id: id.to_owned(),
        account_id: input.account_id,
        to: input.to,
        cc: input.cc,
        bcc: input.bcc,
        subject: input.subject,
        body: input.body,
        signature: input.signature,
        in_reply_to: input.in_reply_to,
        references: input.references,
        updated,
    })
}

pub(super) fn upsert_draft(conn: &Connection, draft: &DraftInput) -> Result<Draft, MailError> {
    check_draft(draft)?;
    let id = match &draft.id {
        Some(id) if !id.is_empty() && id.len() <= 64 => id.clone(),
        _ => random_id()?,
    };
    let stored = DraftInput {
        id: Some(id.clone()),
        ..draft.clone()
    };
    let payload = serde_json::to_string(&stored)
        .map_err(|_| MailError::Storage("could not save the draft".into()))?;
    let now = operations::now_ms();
    conn.execute(
        "INSERT INTO drafts(id,account,payload,created,updated) VALUES (?1,?2,?3,?4,?4)
         ON CONFLICT(id) DO UPDATE SET account=excluded.account,payload=excluded.payload,updated=excluded.updated",
        params![id, stored.account_id, payload, now],
    )
    .map_err(storage_error)?;
    draft_from_payload(&id, &payload, now)
}

fn load_outbox(conn: &Connection, id: &str) -> Result<OutboxRow, MailError> {
    conn.query_row(
        "SELECT account,draft,envelope,message,sent_copy,message_id,submit_op,sent_copy_op FROM outbox WHERE id=?1",
        [id],
        |row| {
            let envelope: String = row.get(2)?;
            let (from, recipients): (String, Vec<String>) =
                serde_json::from_str(&envelope).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
                })?;
            Ok(OutboxRow {
                account: row.get(0)?,
                draft: row.get(1)?,
                envelope: Envelope { from, recipients },
                message: row.get(3)?,
                sent_copy: row.get(4)?,
                message_id: row.get(5)?,
                submit_op: row.get(6)?,
                sent_copy_op: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(storage_error)?
    .ok_or(MailError::NotFound)
}

fn undo_seconds(conn: &Connection) -> Result<u32, MailError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key='undo_send_seconds'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    Ok(value
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_UNDO_SECONDS)
        .min(MAX_UNDO_SECONDS))
}

// Keep the latest known name when a subsequent send uses only the address.
fn remember_recipients(conn: &Connection, payload: &str, at: i64) -> Result<(), MailError> {
    let draft: DraftInput = serde_json::from_str(payload)
        .map_err(|_| MailError::Storage("the queued message could not be read".into()))?;
    for field in [&draft.to, &draft.cc, &draft.bcc] {
        for (name, email) in parse_recipients(field, "recipient")? {
            conn.execute(
                "INSERT INTO recipient_history(email,name,last_sent) VALUES (?1,?2,?3)
                 ON CONFLICT(email) DO UPDATE SET
                 name=CASE WHEN excluded.last_sent >= last_sent THEN coalesce(excluded.name,name) ELSE coalesce(name,excluded.name) END,
                 last_sent=max(last_sent,excluded.last_sent)",
                params![email, name, at],
            ).map_err(storage_error)?;
        }
    }
    Ok(())
}

fn saved_recipients(conn: &Connection) -> Result<Vec<SavedRecipient>, MailError> {
    // Import earlier confirmed sends once, including dismissed outbox entries.
    let imported: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM settings WHERE key='recipient_history_imported')",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if !imported {
        let tx = conn.unchecked_transaction().map_err(storage_error)?;
        let mut statement = tx
            .prepare(
                "SELECT draft,accepted FROM outbox WHERE accepted IS NOT NULL ORDER BY accepted",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        drop(statement);
        for (payload, at) in rows {
            remember_recipients(&tx, &payload, at)?;
        }
        tx.execute(
            "INSERT INTO settings(key,value) VALUES ('recipient_history_imported','true')",
            [],
        )
        .map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
    }
    let mut statement = conn
        .prepare("SELECT email,name FROM recipient_history ORDER BY last_sent DESC,email")
        .map_err(storage_error)?;
    statement
        .query_map([], |row| {
            Ok(SavedRecipient {
                email: row.get(0)?,
                name: row.get(1)?,
            })
        })
        .map_err(storage_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage_error)
}

/// Marks a message accepted and queues its Sent copy if the account wants
/// one. Reads the account from `conn`, so it can run inside the transaction
/// that records the accepted send.
fn queue_sent_copy(conn: &Connection, outbox: &str, account: &str) -> Result<(), MailError> {
    let row = load_outbox(conn, outbox)?;
    remember_recipients(conn, &row.draft, operations::now_ms())?;
    conn.execute(
        "UPDATE outbox SET accepted=coalesce(accepted,?2) WHERE id=?1",
        params![outbox, operations::now_ms()],
    )
    .map_err(storage_error)?;
    let config: Option<String> = conn
        .query_row(
            "SELECT config FROM mail_accounts WHERE id=?1",
            [account],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    let wanted = config
        .and_then(|config| serde_json::from_str::<super::AccountConfig>(&config).ok())
        .is_some_and(|config| config.sending.save_sent_copy);
    let existing: Option<Option<String>> = conn
        .query_row(
            "SELECT sent_copy_op FROM outbox WHERE id=?1",
            [outbox],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    // No row: dismissed or discarded. Some(Some): already queued.
    if !wanted || !matches!(existing, Some(None)) {
        return Ok(());
    }
    let op = operations::insert(
        conn,
        &NewOperation::new(SENT_COPY, serde_json::json!({ "outbox": outbox })).account(account),
    )?;
    conn.execute(
        "UPDATE outbox SET sent_copy_op=?2 WHERE id=?1",
        params![outbox, op.id],
    )
    .map_err(storage_error)?;
    Ok(())
}

fn send_status(op: &Operation, accepted: Option<i64>) -> SendStatus {
    match op.state {
        OperationState::Pending => SendStatus::Queued {
            send_at: op.not_before,
            error: op.last_error.clone(),
        },
        OperationState::Running => SendStatus::Sending,
        OperationState::Done => SendStatus::Accepted {
            at: accepted.unwrap_or(op.updated),
        },
        OperationState::Failed | OperationState::Cancelled => SendStatus::Failed {
            error: op
                .last_error
                .clone()
                .unwrap_or_else(|| "the message was not sent".into()),
        },
        OperationState::Unknown => SendStatus::Unknown {
            error: op
                .last_error
                .clone()
                .unwrap_or_else(|| "the outcome is unknown".into()),
        },
        OperationState::Held => SendStatus::Held,
    }
}

fn sent_copy_status(op: Option<&Operation>) -> SentCopyStatus {
    let Some(op) = op else {
        return SentCopyStatus::NotRequested;
    };
    match op.state {
        OperationState::Held => SentCopyStatus::Failed {
            error: "Restored from backup; review before saving the Sent copy".into(),
        },
        OperationState::Pending => SentCopyStatus::Waiting {
            error: op.last_error.clone(),
        },
        OperationState::Running => SentCopyStatus::Saving,
        OperationState::Done => SentCopyStatus::Saved,
        OperationState::Cancelled => SentCopyStatus::NotRequested,
        OperationState::Failed | OperationState::Unknown => SentCopyStatus::Failed {
            error: op
                .last_error
                .clone()
                .unwrap_or_else(|| "the Sent copy was not saved".into()),
        },
    }
}

fn backoff(attempts: u32) -> Duration {
    Duration::from_secs(15u64.saturating_mul(1 << attempts.saturating_sub(1).min(6)))
}

impl MailEngine {
    pub(super) fn account(&self, id: &str) -> Result<Account, MailError> {
        self.accounts()?
            .into_iter()
            .find(|account| account.id == id)
            .ok_or(MailError::NotFound)
    }

    pub fn send_state(&self) -> Result<SendState, MailError> {
        self.with_store(|store| {
            let conn = store.connection();
            let mut statement = conn
                .prepare("SELECT id,payload,updated FROM drafts ORDER BY updated DESC")
                .map_err(storage_error)?;
            let drafts = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get(2)?))
                })
                .map_err(storage_error)?
                .map(|row| {
                    let (id, payload, updated) = row.map_err(storage_error)?;
                    draft_from_payload(&id, &payload, updated)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut statement = conn
                .prepare("SELECT id,account,subject,recipients,created,submit_op,sent_copy_op,accepted FROM outbox WHERE dismissed=0 ORDER BY created")
                .map_err(storage_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                })
                .map_err(storage_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage_error)?;
            let mut outbox = Vec::new();
            for (id, account_id, subject, recipients, created, submit, copy, accepted) in rows {
                let submit = operations::get(conn, &submit)?;
                let copy = copy.map(|copy| operations::get(conn, &copy)).transpose()?;
                let send = send_status(&submit, accepted);
                let sent_copy = sent_copy_status(copy.as_ref());
                // A finished send leaves the outbox; only a Sent copy still
                // on its way or needing a retry keeps it here.
                if matches!(send, SendStatus::Accepted { .. })
                    && matches!(sent_copy, SentCopyStatus::Saved | SentCopyStatus::NotRequested)
                {
                    continue;
                }
                outbox.push(OutboxItem {
                    id,
                    account_id,
                    subject,
                    recipients,
                    created,
                    send,
                    sent_copy,
                });
            }
            Ok(SendState {
                drafts,
                outbox,
                undo_send_seconds: undo_seconds(conn)?,
                recipients: saved_recipients(conn)?,
            })
        })
    }

    pub fn save_draft(&self, draft: DraftInput) -> Result<Draft, MailError> {
        self.with_store(|store| upsert_draft(store.connection(), &draft))
    }

    pub fn delete_draft(&self, id: &str) -> Result<(), MailError> {
        self.with_store(|store| {
            store
                .connection()
                .execute("DELETE FROM drafts WHERE id=?1", [id])
                .map_err(storage_error)?;
            Ok(())
        })
    }

    pub fn set_undo_send_seconds(&self, seconds: u32) -> Result<(), MailError> {
        if seconds > MAX_UNDO_SECONDS {
            return Err(invalid("choose an undo delay of up to 60 seconds"));
        }
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO settings(key,value) VALUES ('undo_send_seconds',?1)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    [seconds.to_string()],
                )
                .map_err(storage_error)?;
            Ok(())
        })
    }

    /// Saves the draft, then moves it to the outbox behind the undo delay.
    /// The draft is kept if the message cannot be queued.
    pub fn send_draft(&self, draft: DraftInput) -> Result<OutboxItem, MailError> {
        let saved = self.save_draft(draft)?;
        let account_id = saved
            .account_id
            .clone()
            .ok_or_else(|| invalid("choose the account to send from"))?;
        let account = self.account(&account_id)?;
        if account.config.sending.smtp.is_none() {
            return Err(invalid(
                "add outgoing mail (SMTP) settings for this account before sending",
            ));
        }
        let input = DraftInput {
            id: Some(saved.id.clone()),
            account_id: Some(account_id.clone()),
            to: saved.to.clone(),
            cc: saved.cc.clone(),
            bcc: saved.bcc.clone(),
            subject: saved.subject.clone(),
            body: saved.body.clone(),
            signature: saved.signature.clone(),
            in_reply_to: saved.in_reply_to.clone(),
            references: saved.references.clone(),
        };
        let built = build(&account, &input, operations::now_ms() / 1000)?;
        let outbox_id = random_id()?;
        let item = self.with_store(|store| {
            let tx = store
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            let delay = Duration::from_secs(u64::from(undo_seconds(&tx)?));
            let op = operations::insert(
                &tx,
                &NewOperation::new(SUBMIT, serde_json::json!({ "outbox": outbox_id }))
                    .account(&account_id)
                    .delay(delay),
            )?;
            let draft = serde_json::to_string(&input)
                .map_err(|_| MailError::Storage("could not queue the message".into()))?;
            let envelope = serde_json::to_string(&(&built.envelope.from, &built.envelope.recipients))
                .map_err(|_| MailError::Storage("could not queue the message".into()))?;
            let created = operations::now_ms();
            tx.execute(
                "INSERT INTO outbox(id,account,draft,envelope,message,sent_copy,message_id,subject,recipients,submit_op,created)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    outbox_id,
                    account_id,
                    draft,
                    envelope,
                    built.message,
                    built.sent_copy,
                    built.message_id,
                    built.subject,
                    built.recipients,
                    op.id,
                    created
                ],
            )
            .map_err(storage_error)?;
            tx.execute("DELETE FROM drafts WHERE id=?1", [&saved.id])
                .map_err(storage_error)?;
            tx.commit().map_err(storage_error)?;
            // Built here, not read back: with no undo delay the message may
            // be sent and out of the outbox before it could be read.
            Ok(OutboxItem {
                id: outbox_id,
                account_id,
                subject: built.subject,
                recipients: built.recipients,
                created,
                send: send_status(&op, None),
                sent_copy: sent_copy_status(None),
            })
        })?;
        self.wake_operations();
        Ok(item)
    }

    /// Undo before sending starts, or edit a message that failed. Puts it
    /// back in drafts.
    pub fn return_to_drafts(&self, outbox_id: &str) -> Result<Draft, MailError> {
        self.with_store(|store| {
            let tx = store
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            let row = load_outbox(&tx, outbox_id)?;
            operations::transition(
                &tx,
                &row.submit_op,
                &[
                    OperationState::Pending,
                    OperationState::Held,
                    OperationState::Failed,
                ],
                OperationState::Cancelled,
                "",
            )
            .map_err(|_| {
                MailError::Conflict(
                    "sending has already started, so it can no longer be undone".into(),
                )
            })?;
            let input: DraftInput = serde_json::from_str(&row.draft)
                .map_err(|_| MailError::Storage("the queued message could not be read".into()))?;
            let draft = upsert_draft(&tx, &input)?;
            tx.execute("DELETE FROM outbox WHERE id=?1", [outbox_id])
                .map_err(storage_error)?;
            tx.commit().map_err(storage_error)?;
            Ok(draft)
        })
    }

    /// Sends now: skips the rest of the undo delay or retries a failure.
    pub fn retry_send(&self, outbox_id: &str) -> Result<(), MailError> {
        let row = self.with_store(|store| load_outbox(store.connection(), outbox_id))?;
        self.retry_operation(&row.submit_op)
    }

    pub fn retry_sent_copy(&self, outbox_id: &str) -> Result<(), MailError> {
        let row = self.with_store(|store| load_outbox(store.connection(), outbox_id))?;
        let op = row.sent_copy_op.ok_or(MailError::NotFound)?;
        if self.operation(&op)?.state == OperationState::Held {
            self.release_operation(&op)
        } else {
            self.retry_operation(&op)
        }
    }

    /// The user's decision about a message whose outcome is unknown.
    pub fn resolve_send(&self, outbox_id: &str, decision: SendDecision) -> Result<(), MailError> {
        let row = self.with_store(|store| load_outbox(store.connection(), outbox_id))?;
        match decision {
            SendDecision::SendAgain => {
                self.resolve_operation(&row.submit_op, operations::Resolution::RunAgain)
            }
            SendDecision::MarkSent => {
                self.with_store(|store| {
                    let tx = store
                        .connection()
                        .unchecked_transaction()
                        .map_err(storage_error)?;
                    operations::transition(
                        &tx,
                        &row.submit_op,
                        &[OperationState::Unknown, OperationState::Held],
                        OperationState::Done,
                        ",last_error=NULL",
                    )?;
                    queue_sent_copy(&tx, outbox_id, &row.account)?;
                    tx.commit().map_err(storage_error)
                })?;
                self.wake_operations();
                Ok(())
            }
            SendDecision::Discard => self.with_store(|store| {
                let tx = store
                    .connection()
                    .unchecked_transaction()
                    .map_err(storage_error)?;
                operations::transition(
                    &tx,
                    &row.submit_op,
                    &[OperationState::Unknown, OperationState::Held],
                    OperationState::Cancelled,
                    "",
                )?;
                tx.execute("DELETE FROM outbox WHERE id=?1", [outbox_id])
                    .map_err(storage_error)?;
                tx.commit().map_err(storage_error)
            }),
        }
    }

    /// Removes an item from the outbox. A sent message is only hidden; a
    /// message that was not sent is discarded. Unknown outcomes need
    /// [`MailEngine::resolve_send`].
    pub fn dismiss_outbox(&self, outbox_id: &str) -> Result<(), MailError> {
        self.with_store(|store| {
            let tx = store
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            let row = load_outbox(&tx, outbox_id)?;
            let submit = operations::get(&tx, &row.submit_op)?;
            match submit.state {
                OperationState::Done => {
                    if let Some(copy) = &row.sent_copy_op {
                        let _ = operations::transition(
                            &tx,
                            copy,
                            &[
                                OperationState::Pending,
                                OperationState::Failed,
                                OperationState::Held,
                            ],
                            OperationState::Cancelled,
                            "",
                        );
                    }
                    tx.execute("UPDATE outbox SET dismissed=1 WHERE id=?1", [outbox_id])
                        .map_err(storage_error)?;
                }
                OperationState::Pending | OperationState::Failed | OperationState::Held => {
                    operations::transition(
                        &tx,
                        &row.submit_op,
                        &[
                            OperationState::Pending,
                            OperationState::Failed,
                            OperationState::Held,
                        ],
                        OperationState::Cancelled,
                        "",
                    )?;
                    tx.execute("DELETE FROM outbox WHERE id=?1", [outbox_id])
                        .map_err(storage_error)?;
                }
                OperationState::Cancelled => {
                    tx.execute("DELETE FROM outbox WHERE id=?1", [outbox_id])
                        .map_err(storage_error)?;
                }
                OperationState::Running | OperationState::Unknown => {
                    return Err(MailError::Conflict(
                        "this message may already have been sent; choose what to do with it first"
                            .into(),
                    ));
                }
            }
            tx.commit().map_err(storage_error)
        })
    }

    /// Saves new sending settings, first checking that Pigeon can sign in to
    /// the outgoing server when one is configured.
    pub fn update_sending(
        &self,
        id: &str,
        settings: SendingSettings,
    ) -> Result<Account, MailError> {
        let _lease = self.access_lease("sending-settings")?;
        self.refuse_if_managed(id)?;
        let mut account = self.account(id)?;
        validate_sending(&settings)?;
        if let Some(smtp) = &settings.smtp
            && account.config.sending.smtp.as_ref() != Some(smtp)
        {
            let password = self.account_password(id)?;
            self.smtp.verify(
                &SmtpServer {
                    config: smtp,
                    ca_certificate: account.config.ca_certificate.as_deref(),
                },
                &password,
            )?;
        }
        account.config.sending = settings;
        let config = serde_json::to_string(&account.config)
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
        })?;
        Ok(account)
    }

    pub(crate) fn account_password(&self, id: &str) -> Result<crate::secrets::Secret, MailError> {
        self.secrets
            .get(&format!("account:{id}:password"))
            .map_err(|_| MailError::Locked)?
            .ok_or(MailError::Authentication)
    }

    /// Fails while the account still has messages that are not settled.
    pub(crate) fn check_no_unsent(&self, account: &str) -> Result<(), MailError> {
        let unsent: i64 = self.with_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT count(*) FROM outbox o JOIN operations s ON s.id=o.submit_op
                     WHERE o.account=?1 AND o.dismissed=0 AND s.state NOT IN ('done','cancelled')",
                    [account],
                    |row| row.get(0),
                )
                .map_err(storage_error)
        })?;
        if unsent > 0 {
            return Err(MailError::Conflict(
                "this account has messages in the outbox that are not sent; send or discard them first"
                    .into(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_sending(settings: &SendingSettings) -> Result<(), MailError> {
    if settings.display_name.len() > 200 || settings.display_name.contains(['\r', '\n', '\0']) {
        return Err(invalid(
            "enter a display name of up to 200 characters on one line",
        ));
    }
    if settings.signature.len() > 10_000 {
        return Err(invalid("the signature is too long"));
    }
    if settings.signatures.len() > 20 {
        return Err(invalid("save up to 20 signatures per account"));
    }
    let mut ids = std::collections::HashSet::new();
    for signature in &settings.signatures {
        if signature.id.is_empty()
            || signature.id.len() > 64
            || !ids.insert(&signature.id)
            || signature.name.trim().is_empty()
            || signature.name.len() > 100
            || signature.name.contains(['\r', '\n', '\0'])
            || signature.body.len() > 10_000
        {
            return Err(invalid(
                "each signature needs a unique ID, a name up to 100 characters and text up to 10000 bytes",
            ));
        }
    }
    if let Some(smtp) = &settings.smtp {
        if smtp.host.trim().is_empty()
            || smtp.host.contains("://")
            || smtp.host.chars().any(char::is_whitespace)
            || smtp.host.contains(['\0', '/'])
            || smtp.port == 0
        {
            return Err(invalid("enter an outgoing server hostname and valid port"));
        }
        if smtp.username.trim().is_empty() || smtp.username.contains(['\r', '\n', '\0']) {
            return Err(invalid("enter an outgoing server username"));
        }
    }
    Ok(())
}

fn outbox_of(op: &Operation) -> Option<String> {
    serde_json::from_str::<OutboxPayload>(&op.payload)
        .ok()
        .map(|payload| payload.outbox)
}

/// Submits a queued message over SMTP.
pub(crate) struct SubmitHandler;

impl operations::OperationHandler for SubmitHandler {
    fn kind(&self) -> &'static str {
        SUBMIT
    }

    fn run(&self, ctx: &OperationContext<'_>, op: &Operation) -> Outcome {
        let engine = ctx.engine();
        let Some(outbox) = outbox_of(op) else {
            return Outcome::Failed("the queued message could not be read".into());
        };
        let row = match engine.with_store(|store| load_outbox(store.connection(), &outbox)) {
            Ok(row) => row,
            Err(MailError::NotFound) => {
                return Outcome::Failed("the message is no longer in the outbox".into());
            }
            Err(error) => return retry_soon(error),
        };
        let account = match engine.account(&row.account) {
            Ok(account) => account,
            Err(MailError::NotFound) => {
                return Outcome::Failed("the sending account was removed".into());
            }
            Err(error) => return retry_soon(error),
        };
        let Some(smtp) = &account.config.sending.smtp else {
            return Outcome::Failed(
                "add outgoing mail (SMTP) settings for this account, then retry".into(),
            );
        };
        let password = match engine.account_password(&account.id) {
            Ok(password) => password,
            Err(MailError::Authentication) => {
                return Outcome::Failed(
                    "the account password is missing; update it, then retry".into(),
                );
            }
            Err(error) => return retry_soon(error),
        };
        let server = SmtpServer {
            config: smtp,
            ca_certificate: account.config.ca_certificate.as_deref(),
        };
        let mut commit = || ctx.begin_irreversible();
        match engine
            .smtp
            .submit(&server, &password, &row.envelope, &row.message, &mut commit)
        {
            // The Sent copy is queued in `recorded`, together with this
            // outcome, so a lock now cannot lose it or cause a resend.
            Submission::Accepted => Outcome::Done,
            Submission::NotSent {
                retry: true,
                reason,
            } => Outcome::Retry {
                after: backoff(op.attempts),
                error: reason,
            },
            Submission::NotSent {
                retry: false,
                reason,
            } => Outcome::Failed(reason),
            Submission::Unknown(reason) => Outcome::Unknown(reason),
        }
    }

    fn recorded(
        &self,
        conn: &Connection,
        op: &Operation,
        outcome: &Outcome,
    ) -> Result<(), MailError> {
        let (Outcome::Done, Some(outbox), Some(account)) = (outcome, outbox_of(op), &op.account)
        else {
            return Ok(());
        };
        match queue_sent_copy(conn, &outbox, account) {
            // Discarded meanwhile: there is nothing to save a copy of.
            Err(MailError::NotFound) => Ok(()),
            other => other,
        }
    }
}

fn retry_soon(error: MailError) -> Outcome {
    Outcome::Retry {
        after: Duration::from_secs(30),
        error: error.to_string(),
    }
}

/// Appends the sent message to the account's Sent folder.
pub(crate) struct SentCopyHandler;

impl operations::OperationHandler for SentCopyHandler {
    fn kind(&self) -> &'static str {
        SENT_COPY
    }

    fn run(&self, ctx: &OperationContext<'_>, op: &Operation) -> Outcome {
        let engine = ctx.engine();
        let Some(outbox) = outbox_of(op) else {
            return Outcome::Failed("the sent message could not be read".into());
        };
        let row = match engine.with_store(|store| load_outbox(store.connection(), &outbox)) {
            Ok(row) => row,
            Err(MailError::NotFound) => {
                return Outcome::Failed("the message is no longer in the outbox".into());
            }
            Err(error) => return retry_soon(error),
        };
        let account = match engine.account(&row.account) {
            Ok(account) => account,
            Err(MailError::NotFound) => {
                return Outcome::Failed("the sending account was removed".into());
            }
            Err(error) => return retry_soon(error),
        };
        let folder = match engine.with_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT name FROM mail_folders WHERE account=?1 AND kind='sent' ORDER BY name LIMIT 1",
                    [&account.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(storage_error)
        }) {
            Ok(Some(folder)) => folder,
            Ok(None) => {
                return Outcome::Failed(
                    "no Sent folder was found for this account; refresh the account, then retry"
                        .into(),
                );
            }
            Err(error) => return retry_soon(error),
        };
        let password = match engine.account_password(&account.id) {
            Ok(password) => password,
            Err(MailError::Authentication) => {
                return Outcome::Failed(
                    "the account password is missing; update it, then retry".into(),
                );
            }
            Err(error) => return retry_soon(error),
        };
        let give_up = op.attempts >= SENT_COPY_ATTEMPTS;
        let failed = |error: MailError| match error {
            MailError::Authentication | MailError::Tls | MailError::InvalidConfig(_) => {
                Outcome::Failed(error.to_string())
            }
            error if give_up => Outcome::Failed(error.to_string()),
            error => Outcome::Retry {
                after: backoff(op.attempts),
                error: error.to_string(),
            },
        };
        let mut session = match engine.connector.connect(&account.config, &password) {
            Ok(session) => session,
            Err(error) => return failed(error),
        };
        // A previous attempt may have saved it before its reply was lost.
        if op.attempts > 1 {
            match session.has_message_id(&folder, &row.message_id) {
                Ok(true) => return Outcome::Done,
                Ok(false) => {}
                Err(error) => return failed(error),
            }
        }
        match session.append(&folder, &row.sent_copy) {
            Ok(()) => Outcome::Done,
            Err(error) => failed(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::{AccountConfig, TransportSecurity};

    fn account() -> Account {
        Account {
            id: "acc".into(),
            config: AccountConfig {
                managed: false,
                name: "Test".into(),
                address: "me@pigeon.test".into(),
                host: "imap.pigeon.test".into(),
                port: 993,
                username: "me@pigeon.test".into(),
                security: TransportSecurity::Tls,
                ca_certificate: None,
                sending: Default::default(),
            },
            status: "idle".into(),
            error: None,
            last_synced: None,
            sync_folder: None,
            sync_cached: 0,
            sync_total: 0,
        }
    }

    fn text(draft: &DraftInput) -> String {
        String::from_utf8(build(&account(), draft, 0).unwrap().message).unwrap()
    }

    #[test]
    fn replies_carry_threading_headers() {
        let message = text(&DraftInput {
            to: "alice@pigeon.test".into(),
            subject: "Re: receipts".into(),
            body: "Thanks".into(),
            in_reply_to: "abc@pigeon.test".into(),
            references: "<first@pigeon.test> abc@pigeon.test".into(),
            ..Default::default()
        });
        assert!(
            message.contains("In-Reply-To: <abc@pigeon.test>"),
            "{message}"
        );
        assert!(
            message.contains("References: <first@pigeon.test> <abc@pigeon.test>"),
            "{message}"
        );
    }

    #[test]
    fn new_messages_have_no_threading_headers() {
        let message = text(&DraftInput {
            to: "alice@pigeon.test".into(),
            subject: "Hello".into(),
            ..Default::default()
        });
        assert!(!message.contains("In-Reply-To") && !message.contains("References"));
    }

    #[test]
    fn threading_values_cannot_inject_headers() {
        let message = text(&DraftInput {
            to: "alice@pigeon.test".into(),
            subject: "Hello".into(),
            in_reply_to: "x@y.test>\r\nBcc: evil@x.test".into(),
            references: "a@b.test\nBcc: evil@x.test".into(),
            ..Default::default()
        });
        assert!(
            !message.contains("\r\nBcc:") && !message.contains("\nBcc:"),
            "{message}"
        );
    }
}
