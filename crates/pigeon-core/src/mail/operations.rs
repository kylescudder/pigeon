//! Durable queue of work that changes server state: mailbox changes and
//! outgoing mail. Rows live in the encrypted store and survive restarts.
//!
//! Each kind has one registered handler. A handler that is about to do
//! something that cannot be safely repeated, such as finishing an SMTP
//! submission, first calls [`OperationContext::begin_irreversible`]. Work
//! interrupted after that point becomes [`OperationState::Unknown`] and only
//! runs again after an explicit [`Resolution`]; nothing retries it blindly.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::{MailEngine, MailError, storage_error};

/// The longest a failed handler can postpone itself; keeps queued work from
/// silently stalling for hours.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Waiting until `not_before` and any parent operation is done.
    Pending,
    Running,
    Done,
    /// Did not complete and is safe to retry; waits for the user.
    Failed,
    /// May or may not have taken effect on the server. Never retried
    /// automatically.
    Unknown,
    /// Never run automatically, for example outgoing mail restored from a
    /// backup that the user has not reviewed.
    Held,
    Cancelled,
}

impl OperationState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Held => "held",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "unknown" => Self::Unknown,
            "held" => Self::Held,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    pub id: String,
    pub account: Option<String>,
    pub kind: String,
    /// JSON chosen by the handler's kind.
    pub payload: String,
    pub state: OperationState,
    /// Milliseconds since the Unix epoch.
    pub not_before: i64,
    pub attempts: u32,
    /// The handler started a step that cannot be safely repeated.
    pub irreversible: bool,
    pub last_error: Option<String>,
    pub depends_on: Option<String>,
    pub created: i64,
    pub updated: i64,
}

/// A request to queue work. Built with [`NewOperation::new`] and the builder
/// methods.
#[derive(Debug, Clone)]
pub struct NewOperation {
    pub kind: String,
    pub account: Option<String>,
    pub payload: serde_json::Value,
    pub delay: Duration,
    pub depends_on: Option<String>,
    pub held: bool,
}

impl NewOperation {
    pub fn new(kind: &str, payload: serde_json::Value) -> Self {
        Self {
            kind: kind.to_owned(),
            account: None,
            payload,
            delay: Duration::ZERO,
            depends_on: None,
            held: false,
        }
    }
    pub fn account(mut self, account: &str) -> Self {
        self.account = Some(account.to_owned());
        self
    }
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
    /// Runs only after `parent` is done.
    pub fn after(mut self, parent: &str) -> Self {
        self.depends_on = Some(parent.to_owned());
        self
    }
    pub fn held(mut self) -> Self {
        self.held = true;
        self
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Done,
    /// Did not take effect and is safe to run again after `after`.
    Retry {
        after: Duration,
        error: String,
    },
    /// Did not take effect; retrying needs the user.
    Failed(String),
    /// May have taken effect. Waits for an explicit [`Resolution`].
    Unknown(String),
}

/// The user's decision about an operation with an unknown outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// Run it again, accepting that it may take effect twice.
    RunAgain,
    /// Treat it as having taken effect.
    MarkDone,
    /// Stop tracking it.
    Discard,
}

pub trait OperationHandler: Send + Sync {
    fn kind(&self) -> &'static str;
    /// Runs outside the store lock. Must not panic across the point of no
    /// return without having called `begin_irreversible`.
    fn run(&self, ctx: &OperationContext<'_>, op: &Operation) -> Outcome;
}

pub struct OperationContext<'a> {
    engine: &'a MailEngine,
    id: &'a str,
    irreversible: std::cell::Cell<bool>,
}

impl OperationContext<'_> {
    pub fn engine(&self) -> &MailEngine {
        self.engine
    }

    /// Durably records that the handler is about to do something that cannot
    /// be safely repeated. If this returns an error the handler must stop
    /// before that step and return [`Outcome::Retry`] or [`Outcome::Failed`].
    pub fn begin_irreversible(&self) -> Result<(), MailError> {
        self.engine.with_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE operations SET irreversible=1,updated=?2 WHERE id=?1 AND state='running'",
                    params![self.id, now_ms()],
                )
                .map_err(storage_error)?;
            Ok(())
        })?;
        self.irreversible.set(true);
        Ok(())
    }
}

const COLUMNS: &str = "id,account,kind,payload,state,not_before,attempts,irreversible,last_error,depends_on,created,updated";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Operation> {
    let state: String = row.get(4)?;
    Ok(Operation {
        id: row.get(0)?,
        account: row.get(1)?,
        kind: row.get(2)?,
        payload: row.get(3)?,
        state: OperationState::parse(&state).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("unknown operation state {state}").into(),
            )
        })?,
        not_before: row.get(5)?,
        attempts: row.get::<_, i64>(6)? as u32,
        irreversible: row.get(7)?,
        last_error: row.get(8)?,
        depends_on: row.get(9)?,
        created: row.get(10)?,
        updated: row.get(11)?,
    })
}

/// Inserts a new operation using the caller's connection, so callers can
/// queue work in the same transaction as their own rows.
pub(crate) fn insert(conn: &Connection, new: &NewOperation) -> Result<Operation, MailError> {
    if new.kind.is_empty() || new.kind.len() > 64 {
        return Err(MailError::InvalidConfig("invalid operation kind".into()));
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MailError::Storage("could not create operation identifier".into()))?;
    let id = hex::encode(bytes);
    let now = now_ms();
    let not_before = now.saturating_add(new.delay.as_millis().min(i64::MAX as u128) as i64);
    let state = if new.held {
        OperationState::Held
    } else {
        OperationState::Pending
    };
    let payload = serde_json::to_string(&new.payload)
        .map_err(|_| MailError::Storage("invalid operation payload".into()))?;
    conn.execute(
        "INSERT INTO operations(id,account,kind,payload,state,not_before,depends_on,created,updated) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
        params![id, new.account, new.kind, payload, state.as_str(), not_before, new.depends_on, now],
    )
    .map_err(storage_error)?;
    get(conn, &id)
}

pub(crate) fn get(conn: &Connection, id: &str) -> Result<Operation, MailError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM operations WHERE id=?1"),
        [id],
        read_row,
    )
    .optional()
    .map_err(storage_error)?
    .ok_or(MailError::NotFound)
}

/// Moves an operation between states only if it is currently in one of
/// `from`. Returns a conflict naming the current state otherwise.
pub(crate) fn transition(
    conn: &Connection,
    id: &str,
    from: &[OperationState],
    to: OperationState,
    extra: &str,
) -> Result<(), MailError> {
    let allowed = from
        .iter()
        .map(|state| format!("'{}'", state.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let changed = conn
        .execute(
            &format!(
                "UPDATE operations SET state=?2,updated=?3{extra} WHERE id=?1 AND state IN ({allowed})"
            ),
            params![id, to.as_str(), now_ms()],
        )
        .map_err(storage_error)?;
    if changed == 1 {
        return Ok(());
    }
    let current = get(conn, id)?;
    Err(MailError::Conflict(format!(
        "this action is not available while the operation is {}",
        current.state.as_str()
    )))
}

/// Settles work a previous process left running. Work that never reached
/// its point of no return is queued again; anything else is unknown.
pub(crate) fn recover(conn: &Connection, executing: &[String]) -> Result<(), MailError> {
    let mut statement = conn
        .prepare("SELECT id,irreversible FROM operations WHERE state='running'")
        .map_err(storage_error)?;
    let running: Vec<(String, bool)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(storage_error)?
        .collect::<Result<_, _>>()
        .map_err(storage_error)?;
    let now = now_ms();
    for (id, irreversible) in running {
        if executing.contains(&id) {
            continue;
        }
        if irreversible {
            conn.execute(
                "UPDATE operations SET state='unknown',last_error=?2,updated=?3 WHERE id=?1",
                params![
                    id,
                    "Pigeon was interrupted while this was in progress; it may already have taken effect",
                    now
                ],
            )
        } else {
            conn.execute(
                "UPDATE operations SET state='pending',not_before=?2,updated=?2 WHERE id=?1",
                params![id, now],
            )
        }
        .map_err(storage_error)?;
    }
    Ok(())
}

/// Writes a handler's outcome for a running operation.
pub(crate) fn record(conn: &Connection, id: &str, outcome: &Outcome) -> Result<(), MailError> {
    let now = now_ms();
    match outcome {
        Outcome::Done => conn.execute(
            "UPDATE operations SET state='done',last_error=NULL,updated=?2 WHERE id=?1 AND state='running'",
            params![id, now],
        ),
        Outcome::Retry { after, error } => conn.execute(
            "UPDATE operations SET state='pending',irreversible=0,last_error=?2,not_before=?3,updated=?4 WHERE id=?1 AND state='running'",
            params![
                id,
                error,
                now.saturating_add((*after).min(MAX_RETRY_DELAY).as_millis() as i64),
                now
            ],
        ),
        Outcome::Failed(error) => conn.execute(
            "UPDATE operations SET state='failed',irreversible=0,last_error=?2,updated=?3 WHERE id=?1 AND state='running'",
            params![id, error, now],
        ),
        Outcome::Unknown(error) => conn.execute(
            "UPDATE operations SET state='unknown',last_error=?2,updated=?3 WHERE id=?1 AND state='running'",
            params![id, error, now],
        ),
    }
    .map_err(storage_error)?;
    Ok(())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// In-process bookkeeping for the worker.
#[derive(Default)]
pub(crate) struct Runtime {
    pub(crate) handlers: HashMap<&'static str, std::sync::Arc<dyn OperationHandler>>,
    /// Operations this process is running. Store recovery leaves them alone.
    pub(crate) executing: Vec<String>,
    /// Outcomes reached while the store was unavailable, written on reopen.
    pub(crate) unrecorded: Vec<(String, Outcome)>,
}

impl MailEngine {
    pub fn register_operation_handler(&self, handler: std::sync::Arc<dyn OperationHandler>) {
        if let Ok(mut runtime) = self.operations.lock() {
            runtime.handlers.insert(handler.kind(), handler);
        }
    }

    pub fn enqueue_operation(&self, new: NewOperation) -> Result<Operation, MailError> {
        let op = self.with_store(|store| insert(store.connection(), &new))?;
        self.wake_operations();
        Ok(op)
    }

    pub fn operation(&self, id: &str) -> Result<Operation, MailError> {
        self.with_store(|store| get(store.connection(), id))
    }

    /// All operations, oldest first, optionally for one account.
    pub fn operations(&self, account: Option<&str>) -> Result<Vec<Operation>, MailError> {
        self.with_store(|store| {
            let mut statement = store
                .connection()
                .prepare(&format!(
                    "SELECT {COLUMNS} FROM operations WHERE ?1 IS NULL OR account=?1 ORDER BY rowid"
                ))
                .map_err(storage_error)?;
            let rows = statement
                .query_map([account], read_row)
                .map_err(storage_error)?;
            rows.map(|row| row.map_err(storage_error)).collect()
        })
    }

    /// Cancels work that has not started. Running or unknown work cannot be
    /// cancelled; an unknown outcome needs [`MailEngine::resolve_operation`].
    pub fn cancel_operation(&self, id: &str) -> Result<(), MailError> {
        self.with_store(|store| {
            transition(
                store.connection(),
                id,
                &[
                    OperationState::Pending,
                    OperationState::Held,
                    OperationState::Failed,
                ],
                OperationState::Cancelled,
                "",
            )
        })
    }

    /// Makes failed or waiting work due now.
    pub fn retry_operation(&self, id: &str) -> Result<(), MailError> {
        self.with_store(|store| {
            transition(
                store.connection(),
                id,
                &[OperationState::Pending, OperationState::Failed],
                OperationState::Pending,
                ",not_before=?3",
            )
        })?;
        self.wake_operations();
        Ok(())
    }

    pub fn resolve_operation(&self, id: &str, resolution: Resolution) -> Result<(), MailError> {
        let (to, extra) = match resolution {
            Resolution::RunAgain => (
                OperationState::Pending,
                ",irreversible=0,last_error=NULL,not_before=?3",
            ),
            Resolution::MarkDone => (OperationState::Done, ",last_error=NULL"),
            Resolution::Discard => (OperationState::Cancelled, ""),
        };
        self.with_store(|store| {
            transition(
                store.connection(),
                id,
                &[OperationState::Unknown],
                to,
                extra,
            )
        })?;
        self.wake_operations();
        Ok(())
    }

    pub fn hold_operation(&self, id: &str) -> Result<(), MailError> {
        self.with_store(|store| {
            transition(
                store.connection(),
                id,
                &[OperationState::Pending, OperationState::Failed],
                OperationState::Held,
                "",
            )
        })
    }

    pub fn release_operation(&self, id: &str) -> Result<(), MailError> {
        self.with_store(|store| {
            transition(
                store.connection(),
                id,
                &[OperationState::Held],
                OperationState::Pending,
                ",not_before=?3",
            )
        })?;
        self.wake_operations();
        Ok(())
    }

    /// Runs the next due operation, if any. Returns whether one ran. The
    /// handler runs without holding the store, so reading and searching stay
    /// available.
    pub fn run_next_operation(&self) -> Result<bool, MailError> {
        let kinds: Vec<&'static str> = {
            let runtime = self.runtime()?;
            runtime.handlers.keys().copied().collect()
        };
        if kinds.is_empty() {
            return Ok(false);
        }
        let claimed = self.with_store(|store| {
            let conn = store.connection();
            self.write_unrecorded(conn)?;
            let placeholders = (0..kinds.len())
                .map(|index| format!("?{}", index + 2))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "UPDATE operations SET state='running',attempts=attempts+1,updated=?1
                 WHERE id=(SELECT o.id FROM operations o
                   WHERE o.state='pending' AND o.not_before<=?1 AND o.kind IN ({placeholders})
                   AND (o.depends_on IS NULL OR EXISTS (SELECT 1 FROM operations p WHERE p.id=o.depends_on AND p.state='done'))
                   ORDER BY o.not_before,o.rowid LIMIT 1)
                 RETURNING {COLUMNS}"
            );
            let mut values: Vec<rusqlite::types::Value> = vec![now_ms().into()];
            values.extend(kinds.iter().map(|kind| (*kind).to_owned().into()));
            let op = conn
                .query_row(&sql, rusqlite::params_from_iter(values), read_row)
                .optional()
                .map_err(storage_error)?;
            if let Some(op) = &op {
                // Registered under the store lock so a concurrent reopen
                // cannot mistake this claim for interrupted work.
                self.runtime()?.executing.push(op.id.clone());
            }
            Ok(op)
        })?;
        let Some(op) = claimed else {
            return Ok(false);
        };
        let handler = self.runtime()?.handlers.get(op.kind.as_str()).cloned();
        let ctx = OperationContext {
            engine: self,
            id: &op.id,
            irreversible: std::cell::Cell::new(false),
        };
        let outcome = match handler {
            None => Outcome::Retry {
                after: Duration::from_secs(60),
                error: "no handler is available for this operation".into(),
            },
            Some(handler) => catch_unwind(AssertUnwindSafe(|| handler.run(&ctx, &op)))
                .unwrap_or_else(|_| {
                    if ctx.irreversible.get() {
                        Outcome::Unknown(
                            "the operation stopped unexpectedly after it may have taken effect"
                                .into(),
                        )
                    } else {
                        Outcome::Failed("the operation stopped unexpectedly".into())
                    }
                }),
        };
        let recorded = self.with_store(|store| record(store.connection(), &op.id, &outcome));
        let mut runtime = self.runtime()?;
        if let Err(error) = recorded {
            // Keep the claim so recovery cannot requeue it; the outcome is
            // written when the store reopens.
            runtime.unrecorded.push((op.id, outcome));
            return Err(error);
        }
        runtime.executing.retain(|id| *id != op.id);
        Ok(true)
    }

    /// Blocks until queued work may be due or `timeout` passes.
    pub fn wait_for_operations(&self, timeout: Duration) {
        let (lock, signal) = &self.operations_signal;
        if let Ok(mut woken) = lock.lock() {
            if !*woken {
                woken = match signal.wait_timeout(woken, timeout) {
                    Ok((guard, _)) => guard,
                    Err(_) => return,
                };
            }
            *woken = false;
        }
    }

    pub(crate) fn wake_operations(&self) {
        let (lock, signal) = &self.operations_signal;
        if let Ok(mut woken) = lock.lock() {
            *woken = true;
            signal.notify_all();
        }
    }

    fn runtime(&self) -> Result<std::sync::MutexGuard<'_, Runtime>, MailError> {
        self.operations
            .lock()
            .map_err(|_| MailError::Storage("operation queue unavailable".into()))
    }

    /// Called with a freshly opened store, before anything else uses it.
    pub(crate) fn reconcile_operations(&self, conn: &Connection) -> Result<(), MailError> {
        self.write_unrecorded(conn)?;
        recover(conn, &self.runtime()?.executing)
    }

    fn write_unrecorded(&self, conn: &Connection) -> Result<(), MailError> {
        let mut runtime = self.runtime()?;
        while let Some((id, outcome)) = runtime.unrecorded.first().cloned() {
            record(conn, &id, &outcome)?;
            runtime.unrecorded.remove(0);
            runtime.executing.retain(|running| *running != id);
        }
        Ok(())
    }
}
