//! Durable queue of changes to apply to the server: read state, moves, and
//! deletion. Queued changes show locally straight away, survive restarts, and
//! are applied when the account next connects. A change is complete only once
//! the server has been checked; it is never inferred from the local view.
//!
//! Each change names a folder, its UIDVALIDITY, and UIDs. Before writing, the
//! folder is opened read-write and the epoch and UIDs are confirmed. A message
//! that disappeared before Pigeon acted is a conflict: it is never re-created
//! or searched for elsewhere. Writes are UID-targeted; Pigeon never sends a
//! plain EXPUNGE, which would also remove other clients' \Deleted messages.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::source::{MailSession, WriteCapabilities};
use super::{Account, MailEngine, MailError, now, storage_error};
use crate::secrets::Secret;

const MAX_REQUEST: usize = 5000;
/// Bounds each command line; a UID list is sent verbatim.
const MAX_ACTION_UIDS: usize = 500;
const OUTSTANDING: &str = "('queued','applying')";
const FLAG_KINDS: &str = "('mark_read','mark_unread')";
const HIDING_KINDS: &str = "('archive','trash','move','delete_permanently')";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionKind {
    MarkRead,
    MarkUnread,
    Archive,
    /// Moves to the account's Trash folder. Not permanent.
    Trash,
    /// Flags and expunges exactly these messages. Requires UIDPLUS.
    DeletePermanently,
    Move {
        folder_id: String,
    },
}

impl ActionKind {
    fn stored(&self) -> &'static str {
        match self {
            Self::MarkRead => "mark_read",
            Self::MarkUnread => "mark_unread",
            Self::Archive => "archive",
            Self::Trash => "trash",
            Self::DeletePermanently => "delete_permanently",
            Self::Move { .. } => "move",
        }
    }
}

/// An outstanding change, for display. Completed changes are removed.
#[derive(Debug, Clone, Serialize)]
pub struct PendingAction {
    pub id: i64,
    pub account_id: String,
    pub kind: String,
    pub folder: String,
    pub target: Option<String>,
    /// Subject of one affected message, while it is still cached.
    pub subject: Option<String>,
    pub count: u64,
    pub conflicts: u64,
    /// queued, applying, failed, or conflict.
    pub state: String,
    pub error: Option<String>,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ActionCounts {
    /// Queued or being applied.
    pub pending: u64,
    pub failed: u64,
    pub conflict: u64,
}

struct Queued {
    id: i64,
    kind: String,
    folder: String,
    validity: u32,
    target: Option<String>,
    attempted: bool,
    step: Option<String>,
    uids: Vec<u32>,
}

enum Outcome {
    /// `applied` are complete on the server; `dropped` no longer exist there and
    /// needed nothing. `conflicts` could not be applied safely.
    Settled {
        applied: Vec<u32>,
        dropped: Vec<u32>,
        conflicts: Vec<u32>,
        reason: Option<String>,
    },
    /// The server cannot or did not apply this change; retrying unchanged will
    /// not help.
    Failed(String),
}

impl Outcome {
    fn conflict(uids: &[u32], reason: &str) -> Self {
        Self::Settled {
            applied: Vec::new(),
            dropped: Vec::new(),
            conflicts: uids.to_vec(),
            reason: Some(reason.into()),
        }
    }
}

impl MailEngine {
    /// Queues a change for the given messages and shows it locally. Returns
    /// how many messages it affects; ones already in the requested state are
    /// skipped.
    pub fn queue_actions(&self, ids: &[i64], kind: ActionKind) -> Result<usize, MailError> {
        if ids.is_empty() {
            return Err(MailError::InvalidAction("select at least one message"));
        }
        if ids.len() > MAX_REQUEST {
            return Err(MailError::InvalidAction(
                "select at most 5,000 messages at a time",
            ));
        }
        let stored = kind.stored();
        self.with_store(|store| {
            let tx = store
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            // (account, folder, epoch) -> (folder kind, [(uid, unread)])
            let mut groups = BTreeMap::<(String, String, u32), (String, Vec<(u32, bool)>)>::new();
            {
                let mut select = tx
                    .prepare(&format!(
                        "SELECT m.account,m.folder,m.uid_validity,m.uid,m.unread,f.kind FROM messages m JOIN mail_folders f ON f.account=m.account AND f.name=m.folder WHERE m.id=?1 AND m.uid IS NOT NULL AND m.uid_validity IS NOT NULL AND {}",
                        super::cache::VISIBLE
                    ))
                    .map_err(storage_error)?;
                for id in ids {
                    let row = select
                        .query_row([id], |row| {
                            Ok((
                                (row.get(0)?, row.get(1)?, row.get(2)?),
                                (row.get::<_, u32>(3)?, row.get::<_, bool>(4)?),
                                row.get::<_, String>(5)?,
                            ))
                        })
                        .optional()
                        .map_err(storage_error)?;
                    if let Some((key, item, folder_kind)) = row {
                        groups
                            .entry(key)
                            .or_insert_with(|| (folder_kind, Vec::new()))
                            .1
                            .push(item);
                    }
                }
            }
            if groups.is_empty() {
                return Err(MailError::NotFound);
            }
            let now = now();
            let mut queued = 0;
            for ((account, folder, validity), (folder_kind, mut items)) in groups {
                super::folders::ensure_ready(&tx, &account)?;
                let target = match &kind {
                    ActionKind::MarkRead | ActionKind::MarkUnread => None,
                    ActionKind::DeletePermanently => None,
                    ActionKind::Archive => {
                        if folder_kind == "archive" {
                            continue;
                        }
                        Some(special_folder(&tx, &account, "archive")?.ok_or(MailError::NoArchiveFolder)?)
                    }
                    ActionKind::Trash => {
                        if folder_kind == "trash" {
                            return Err(MailError::InvalidAction(
                                "this mail is already in Trash; delete it permanently instead",
                            ));
                        }
                        Some(special_folder(&tx, &account, "trash")?.ok_or(MailError::NoTrashFolder)?)
                    }
                    ActionKind::Move { folder_id } => {
                        let (owner, name): (String, String) = tx
                            .query_row(
                                "SELECT account,name FROM mail_folders WHERE id=?1",
                                [folder_id],
                                |row| Ok((row.get(0)?, row.get(1)?)),
                            )
                            .optional()
                            .map_err(storage_error)?
                            .ok_or(MailError::NotFound)?;
                        if owner != account {
                            return Err(MailError::InvalidAction(
                                "moving mail between accounts is not supported",
                            ));
                        }
                        if name == folder {
                            continue;
                        }
                        Some(name)
                    }
                };
                match kind {
                    ActionKind::MarkRead => items.retain(|(_, unread)| *unread),
                    ActionKind::MarkUnread => items.retain(|(_, unread)| !*unread),
                    _ => {}
                }
                items.sort_unstable();
                for chunk in items.chunks(MAX_ACTION_UIDS) {
                    tx.execute(
                        "INSERT INTO mail_actions(account,kind,folder,uid_validity,target,state,created,updated) VALUES (?1,?2,?3,?4,?5,'queued',?6,?6)",
                        params![account, stored, folder, validity, target, now],
                    )
                    .map_err(storage_error)?;
                    let action = tx.last_insert_rowid();
                    {
                        let mut insert = tx
                            // With an earlier read change still outstanding, the
                            // local value is that change's, not the server's.
                            .prepare(&format!("INSERT INTO mail_action_items(action,uid,prior_unread,state) VALUES (?1,?2,coalesce((SELECT i.prior_unread FROM mail_action_items i JOIN mail_actions a ON a.id=i.action WHERE a.id!=?1 AND a.account=?4 AND a.folder=?5 AND a.uid_validity=?6 AND a.kind IN {FLAG_KINDS} AND a.state IN {OUTSTANDING} AND i.state='pending' AND i.uid=?2 ORDER BY a.id LIMIT 1),?3),'pending')"))
                            .map_err(storage_error)?;
                        for (uid, unread) in chunk {
                            insert.execute(params![action, uid, unread, account, folder, validity]).map_err(storage_error)?;
                        }
                    }
                    show_locally(&tx, action)?;
                    queued += chunk.len();
                }
            }
            tx.commit().map_err(storage_error)?;
            Ok(queued)
        })
    }

    pub fn pending_actions(&self) -> Result<Vec<PendingAction>, MailError> {
        self.with_store(|store| {
            let mut statement = store.connection().prepare(
                "SELECT a.id,a.account,a.kind,a.folder,a.target,a.state,a.error,a.created,a.updated,
                    (SELECT count(*) FROM mail_action_items i WHERE i.action=a.id),
                    (SELECT count(*) FROM mail_action_items i WHERE i.action=a.id AND i.state='conflict'),
                    (SELECT m.subject FROM mail_action_items i JOIN messages m ON m.account=a.account AND m.folder=a.folder AND m.uid_validity=a.uid_validity AND m.uid=i.uid WHERE i.action=a.id LIMIT 1)
                 FROM mail_actions a ORDER BY a.id",
            ).map_err(storage_error)?;
            statement
                .query_map([], |row| {
                    Ok(PendingAction {
                        id: row.get(0)?,
                        account_id: row.get(1)?,
                        kind: row.get(2)?,
                        folder: row.get(3)?,
                        target: row.get(4)?,
                        state: row.get(5)?,
                        error: row.get(6)?,
                        created: row.get(7)?,
                        updated: row.get(8)?,
                        count: row.get::<_, i64>(9)? as u64,
                        conflicts: row.get::<_, i64>(10)? as u64,
                        subject: row.get(11)?,
                    })
                })
                .map_err(storage_error)?
                .collect::<rusqlite::Result<_>>()
                .map_err(storage_error)
        })
    }

    pub fn action_counts(&self) -> Result<ActionCounts, MailError> {
        self.with_store(|store| {
            let mut counts = ActionCounts::default();
            let mut statement = store
                .connection()
                .prepare("SELECT state,count(*) FROM mail_actions GROUP BY state")
                .map_err(storage_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(storage_error)?;
            for row in rows {
                let (state, count) = row.map_err(storage_error)?;
                match state.as_str() {
                    "failed" => counts.failed += count as u64,
                    "conflict" => counts.conflict += count as u64,
                    _ => counts.pending += count as u64,
                }
            }
            Ok(counts)
        })
    }

    /// Queues a failed or conflicting change again. This is the explicit
    /// decision an uncertain copy needs, so unconfirmed attempt history is
    /// cleared. A confirmed copy is kept, so a retry only removes the source.
    pub fn retry_action(&self, id: i64) -> Result<(), MailError> {
        self.with_store(|store| {
            let tx = store
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            let changed = tx
                .execute(
                    "UPDATE mail_actions SET state='queued',attempted=CASE WHEN step='copied' THEN attempted ELSE 0 END,step=CASE WHEN step='copied' THEN step ELSE NULL END,error=NULL,updated=?2 WHERE id=?1 AND state IN ('failed','conflict')",
                    params![id, now()],
                )
                .map_err(storage_error)?;
            if changed == 0 {
                return Err(MailError::NotFound);
            }
            tx.execute(
                "UPDATE mail_action_items SET state='pending' WHERE action=?1",
                [id],
            )
            .map_err(storage_error)?;
            show_locally(&tx, id)?;
            tx.commit().map_err(storage_error)
        })
    }

    /// Abandons a change that is not being applied and restores its local view.
    pub fn discard_action(&self, id: i64) -> Result<(), MailError> {
        self.with_store(|store| {
            let tx = store
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            let state: String = tx
                .query_row("SELECT state FROM mail_actions WHERE id=?1", [id], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(storage_error)?
                .ok_or(MailError::NotFound)?;
            if state == "applying" {
                return Err(MailError::Busy);
            }
            if state == "queued" {
                release_locally(&tx, id)?;
            }
            tx.execute("DELETE FROM mail_actions WHERE id=?1", [id])
                .map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        })
    }

    /// Applies an account's queued changes now. Busy while it is syncing; sync
    /// applies the queue itself.
    pub fn apply_actions(&self, id: &str) -> Result<(), MailError> {
        let _lease = self.lease(id)?;
        let account = self
            .accounts()?
            .into_iter()
            .find(|account| account.id == id)
            .ok_or(MailError::NotFound)?;
        let password = self
            .secrets
            .get(&format!("account:{id}:password"))
            .map_err(|_| MailError::Locked)?
            .ok_or(MailError::Authentication)?;
        self.apply_queue(&account, &password)
    }

    /// Applies queued changes in order. Connects only if there is work. A
    /// connection failure leaves the remaining work queued and is returned.
    pub(super) fn apply_queue(
        &self,
        account: &Account,
        password: &Secret,
    ) -> Result<(), MailError> {
        self.with_store(|store| super::folders::ensure_ready(store.connection(), &account.id))?;
        let mut session: Option<Box<dyn MailSession>> = None;
        let mut capabilities = None;
        let mut last = 0;
        while let Some(action) = self.next_action(&account.id, last)? {
            last = action.id;
            if action.uids.is_empty() {
                self.with_store(|store| {
                    store
                        .connection()
                        .execute("DELETE FROM mail_actions WHERE id=?1", [action.id])
                        .map_err(storage_error)
                })?;
                continue;
            }
            let result = match &mut session {
                Some(session) => self.apply_one(session.as_mut(), &mut capabilities, &action),
                None => match self.connector.connect(&account.config, password) {
                    Ok(connected) => {
                        let session = session.insert(connected);
                        self.apply_one(session.as_mut(), &mut capabilities, &action)
                    }
                    Err(error) => Err(error),
                },
            };
            match result {
                Ok(outcome) => self.settle(&action, outcome)?,
                // A malformed or unparseable response would recur on every
                // attempt; failing this change keeps it from blocking sync.
                Err(
                    error @ (MailError::Protocol
                    | MailError::ResponseParse { .. }
                    | MailError::InvalidAction(_)),
                ) => self.settle(&action, Outcome::Failed(error.to_string()))?,
                Err(error) if error.is_rejection() => {
                    self.settle(&action, Outcome::Failed(error.to_string()))?
                }
                Err(error) => {
                    // Unknown whether a write reached the server; `attempted`
                    // records that, and the next run re-checks before acting.
                    let message = error.to_string();
                    self.with_store(|store| {
                        store
                            .connection()
                            .execute(
                                "UPDATE mail_actions SET state='queued',error=?2,updated=?3 WHERE id=?1",
                                params![action.id, message, now()],
                            )
                            .map_err(storage_error)
                    })?;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn next_action(&self, account: &str, after: i64) -> Result<Option<Queued>, MailError> {
        self.with_store(|store| {
            let db = store.connection();
            let Some(mut action) = db
                .query_row(
                    "SELECT id,kind,folder,uid_validity,target,attempted,step FROM mail_actions WHERE account=?1 AND state='queued' AND id>?2 ORDER BY id LIMIT 1",
                    params![account, after],
                    |row| {
                        Ok(Queued {
                            id: row.get(0)?,
                            kind: row.get(1)?,
                            folder: row.get(2)?,
                            validity: row.get(3)?,
                            target: row.get(4)?,
                            attempted: row.get(5)?,
                            step: row.get(6)?,
                            uids: Vec::new(),
                        })
                    },
                )
                .optional()
                .map_err(storage_error)?
            else {
                return Ok(None);
            };
            action.uids = db
                .prepare("SELECT uid FROM mail_action_items WHERE action=?1 AND state='pending' ORDER BY uid")
                .map_err(storage_error)?
                .query_map([action.id], |row| row.get(0))
                .map_err(storage_error)?
                .collect::<rusqlite::Result<_>>()
                .map_err(storage_error)?;
            db.execute(
                "UPDATE mail_actions SET state='applying',updated=?2 WHERE id=?1",
                params![action.id, now()],
            )
            .map_err(storage_error)?;
            Ok(Some(action))
        })
    }

    fn mark_progress(&self, id: i64, step: Option<&str>) -> Result<(), MailError> {
        // Committed before the write is sent, so a crash or dropped connection
        // afterwards is known to be uncertain.
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE mail_actions SET attempted=1,step=coalesce(?2,step),updated=?3 WHERE id=?1",
                    params![id, step, now()],
                )
                .map_err(storage_error)?;
            Ok(())
        })
    }

    fn apply_one(
        &self,
        session: &mut dyn MailSession,
        capabilities: &mut Option<WriteCapabilities>,
        action: &Queued,
    ) -> Result<Outcome, MailError> {
        let validity = match session.select_writable(&action.folder) {
            Ok(validity) => validity,
            Err(error) if error.is_rejection() => {
                return Ok(Outcome::conflict(
                    &action.uids,
                    "the folder is no longer available on the server",
                ));
            }
            Err(error) => return Err(error),
        };
        if validity != action.validity {
            return Ok(Outcome::conflict(
                &action.uids,
                "the folder was reset on the server, so these messages can no longer be identified safely",
            ));
        }
        let present: HashMap<u32, bool> = session
            .present(&action.uids)?
            .into_iter()
            .filter(|flag| action.uids.contains(&flag.uid))
            .map(|flag| (flag.uid, flag.unread))
            .collect();
        let (targets, missing): (Vec<u32>, Vec<u32>) = action
            .uids
            .iter()
            .partition(|uid| present.contains_key(uid));
        let caps = |session: &mut dyn MailSession, cached: &mut Option<WriteCapabilities>| {
            if cached.is_none() {
                *cached = Some(session.capabilities()?);
            }
            Ok::<_, MailError>(cached.unwrap_or_default())
        };
        match action.kind.as_str() {
            "mark_read" | "mark_unread" => {
                let seen = action.kind == "mark_read";
                if !targets.is_empty() {
                    self.mark_progress(action.id, None)?;
                    session.set_seen(&targets, seen)?;
                    if session
                        .present(&targets)?
                        .iter()
                        .any(|flag| flag.unread == seen)
                    {
                        return Ok(Outcome::Failed(
                            "the server accepted the change but did not apply it".into(),
                        ));
                    }
                }
                // Read state of mail that no longer exists needs no conflict.
                Ok(Outcome::Settled {
                    applied: targets,
                    dropped: missing,
                    conflicts: Vec::new(),
                    reason: None,
                })
            }
            "delete_permanently" => {
                if !targets.is_empty() {
                    if !caps(session, capabilities)?.uidplus {
                        return Err(MailError::Unsupported(
                            "permanent deletion of selected messages (UIDPLUS)",
                        ));
                    }
                    self.mark_progress(action.id, None)?;
                    session.expunge_uids(&targets)?;
                    if !session.present(&targets)?.is_empty() {
                        return Ok(Outcome::Failed(
                            "the server accepted the deletion but the mail is still there".into(),
                        ));
                    }
                }
                // Already gone is what was asked for.
                Ok(Outcome::Settled {
                    applied: targets,
                    dropped: missing,
                    conflicts: Vec::new(),
                    reason: None,
                })
            }
            "archive" | "trash" | "move" => {
                let target = action.target.as_deref().ok_or(MailError::Protocol)?;
                // Missing before any attempt: changed elsewhere, so a conflict.
                // Missing after an attempt: most likely this change moved it.
                let (mut applied, conflicts) = if action.attempted {
                    (missing, Vec::new())
                } else {
                    (Vec::new(), missing)
                };
                if !targets.is_empty() {
                    let caps = caps(session, capabilities)?;
                    if caps.moves {
                        self.mark_progress(action.id, None)?;
                        session.move_to(&targets, target)?;
                    } else if caps.uidplus {
                        if action.step.as_deref() != Some("copied") {
                            if action.attempted {
                                let mut all = conflicts;
                                all.extend(&targets);
                                return Ok(Outcome::Settled {
                                    applied,
                                    dropped: Vec::new(),
                                    conflicts: all,
                                    reason: Some("the connection was lost while copying; the server may already have copied these messages, so retrying could duplicate them".into()),
                                });
                            }
                            self.mark_progress(action.id, None)?;
                            session.copy_to(&targets, target)?;
                            self.mark_progress(action.id, Some("copied"))?;
                        }
                        session.expunge_uids(&targets)?;
                    } else {
                        return Err(MailError::Unsupported("moving mail (MOVE or UIDPLUS)"));
                    }
                    if !session.present(&targets)?.is_empty() {
                        return Ok(Outcome::Failed(
                            "the server accepted the move but the mail is still in its folder"
                                .into(),
                        ));
                    }
                    applied.extend(targets);
                }
                let reason = (!conflicts.is_empty()).then(|| {
                    format!(
                        "{} message(s) were removed or moved on the server before this change was applied",
                        conflicts.len()
                    )
                });
                Ok(Outcome::Settled {
                    applied,
                    dropped: Vec::new(),
                    conflicts,
                    reason,
                })
            }
            _ => Ok(Outcome::Failed("this change is not supported".into())),
        }
    }

    fn settle(&self, action: &Queued, outcome: Outcome) -> Result<(), MailError> {
        self.with_store(|store| {
            let tx = store
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            match outcome {
                Outcome::Failed(reason) => {
                    tx.execute(
                        "UPDATE mail_actions SET state='failed',error=?2,updated=?3 WHERE id=?1",
                        params![action.id, reason, now()],
                    )
                    .map_err(storage_error)?;
                    release_locally(&tx, action.id)?;
                }
                Outcome::Settled {
                    applied,
                    dropped,
                    conflicts,
                    reason,
                } => {
                    let flags = action.kind.starts_with("mark_");
                    for uid in applied.iter().chain(&dropped) {
                        if !flags {
                            // The source copy is gone; its destination arrives
                            // with the next sync of that folder.
                            tx.execute(
                                "DELETE FROM messages WHERE account=(SELECT account FROM mail_actions WHERE id=?1) AND folder=?2 AND uid_validity=?3 AND uid=?4",
                                params![action.id, action.folder, action.validity, uid],
                            )
                            .map_err(storage_error)?;
                        }
                        tx.execute(
                            "DELETE FROM mail_action_items WHERE action=?1 AND uid=?2",
                            params![action.id, uid],
                        )
                        .map_err(storage_error)?;
                        if flags {
                            tx.execute(
                                &format!("UPDATE messages SET flags_pending=0,unread=?5 WHERE account=(SELECT account FROM mail_actions WHERE id=?1) AND folder=?2 AND uid_validity=?3 AND uid=?4 AND {}", no_other(FLAG_KINDS)),
                                params![action.id, action.folder, action.validity, uid, action.kind == "mark_unread"],
                            )
                            .map_err(storage_error)?;
                        }
                    }
                    if conflicts.is_empty() {
                        tx.execute("DELETE FROM mail_actions WHERE id=?1", [action.id])
                            .map_err(storage_error)?;
                    } else {
                        tx.execute(
                            "UPDATE mail_actions SET state='conflict',error=?2,updated=?3 WHERE id=?1",
                            params![action.id, reason, now()],
                        )
                        .map_err(storage_error)?;
                        tx.execute(
                            "UPDATE mail_action_items SET state='conflict' WHERE action=?1",
                            [action.id],
                        )
                        .map_err(storage_error)?;
                        release_locally(&tx, action.id)?;
                    }
                }
            }
            tx.commit().map_err(storage_error)
        })
    }
}

fn special_folder(
    tx: &Transaction<'_>,
    account: &str,
    kind: &str,
) -> Result<Option<String>, MailError> {
    // Prefer the conventional name when several folders share a role.
    tx.query_row(
        "SELECT name FROM mail_folders WHERE account=?1 AND kind=?2 ORDER BY CASE WHEN lower(name) IN ('archive','trash') THEN 0 ELSE 1 END,name LIMIT 1",
        params![account, kind],
        |row| row.get(0),
    )
    .optional()
    .map_err(storage_error)
}

/// Messages addressed by action ?1's pending items.
const TARGETED: &str = "account=(SELECT account FROM mail_actions WHERE id=?1) AND folder=(SELECT folder FROM mail_actions WHERE id=?1) AND uid_validity=(SELECT uid_validity FROM mail_actions WHERE id=?1) AND uid IN (SELECT uid FROM mail_action_items WHERE action=?1)";

/// No other outstanding change of these kinds still covers the message.
fn no_other(kinds: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM mail_action_items i JOIN mail_actions a ON a.id=i.action WHERE a.id!=?1 AND a.state IN {OUTSTANDING} AND a.kind IN {kinds} AND i.state='pending' AND a.account=messages.account AND a.folder=messages.folder AND a.uid_validity=messages.uid_validity AND i.uid=messages.uid)"
    )
}

/// Shows a queued change in the local view.
fn show_locally(tx: &Transaction<'_>, action: i64) -> Result<(), MailError> {
    let kind: String = tx
        .query_row(
            "SELECT kind FROM mail_actions WHERE id=?1",
            [action],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    let sql = match kind.as_str() {
        "mark_read" => format!("UPDATE messages SET unread=0,flags_pending=1 WHERE {TARGETED}"),
        "mark_unread" => format!("UPDATE messages SET unread=1,flags_pending=1 WHERE {TARGETED}"),
        _ => format!("UPDATE messages SET local_hidden=1 WHERE {TARGETED}"),
    };
    tx.execute(&sql, [action]).map_err(storage_error)?;
    Ok(())
}

/// Undoes a change's local effect when it will not be applied. Read state goes
/// back to its value when queued; the next sync corrects it if that changed.
fn release_locally(tx: &Transaction<'_>, action: i64) -> Result<(), MailError> {
    let kind: String = tx
        .query_row(
            "SELECT kind FROM mail_actions WHERE id=?1",
            [action],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if kind.starts_with("mark_") {
        // Messages another read change still covers show the latest one.
        tx.execute(
            &format!(
                "UPDATE messages SET flags_pending=1,unread=(SELECT a.kind='mark_unread' FROM mail_action_items i JOIN mail_actions a ON a.id=i.action WHERE a.id!=?1 AND a.state IN {OUTSTANDING} AND a.kind IN {FLAG_KINDS} AND i.state='pending' AND a.account=messages.account AND a.folder=messages.folder AND a.uid_validity=messages.uid_validity AND i.uid=messages.uid ORDER BY a.id DESC LIMIT 1) WHERE {TARGETED} AND NOT {}",
                no_other(FLAG_KINDS)
            ),
            [action],
        )
        .map_err(storage_error)?;
    }
    let sql = if kind.starts_with("mark_") {
        format!(
            "UPDATE messages SET flags_pending=0,unread=coalesce((SELECT prior_unread FROM mail_action_items WHERE action=?1 AND uid=messages.uid),unread) WHERE {TARGETED} AND {}",
            no_other(FLAG_KINDS)
        )
    } else {
        format!(
            "UPDATE messages SET local_hidden=0 WHERE {TARGETED} AND {}",
            no_other(HIDING_KINDS)
        )
    };
    tx.execute(&sql, [action]).map_err(storage_error)?;
    Ok(())
}
