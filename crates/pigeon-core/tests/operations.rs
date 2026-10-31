//! The durable operations queue: pending work survives restarts, an operation
//! that may have reached the server is never retried without a decision, and
//! nothing runs while the store is locked.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pigeon_core::mail::operations::{
    NewOperation, Operation, OperationContext, OperationHandler, OperationState, Outcome,
    Resolution,
};
use pigeon_core::mail::{MailEngine, MailError};
use pigeon_core::secrets::{KeyringState, MemoryStore};
use pigeon_core::unlock::{UnlockOutcome, open_store};

type Script = Box<dyn Fn(&OperationContext<'_>, &Operation) -> Outcome + Send + Sync>;

struct Scripted {
    kind: &'static str,
    calls: AtomicUsize,
    seen: Mutex<Vec<String>>,
    script: Script,
}

impl Scripted {
    fn new(
        kind: &'static str,
        script: impl Fn(&OperationContext<'_>, &Operation) -> Outcome + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            kind,
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            script: Box::new(script),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl OperationHandler for Scripted {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn run(&self, ctx: &OperationContext<'_>, op: &Operation) -> Outcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(op.payload.clone());
        (self.script)(ctx, op)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    secrets: Arc<MemoryStore>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            path: dir.path().join("mail.db"),
            _dir: dir,
            secrets: Arc::new(MemoryStore::new(KeyringState::Unlocked)),
        }
    }
    fn engine(&self) -> MailEngine {
        MailEngine::new(self.path.clone(), self.secrets.clone())
    }
}

fn job(kind: &str) -> NewOperation {
    NewOperation::new(kind, serde_json::json!({ "note": kind }))
}

fn drain(engine: &MailEngine) -> usize {
    let mut ran = 0;
    while engine.run_next_operation().unwrap() {
        ran += 1;
    }
    ran
}

fn state(engine: &MailEngine, id: &str) -> OperationState {
    engine.operation(id).unwrap().state
}

#[test]
fn queued_operation_runs_once_and_is_done() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let handler = Scripted::new("echo", |_, _| Outcome::Done);
    engine.register_operation_handler(handler.clone());

    let op = engine.enqueue_operation(job("echo")).unwrap();
    assert_eq!(op.state, OperationState::Pending);
    assert_eq!(drain(&engine), 1);
    assert_eq!(drain(&engine), 0);

    let done = engine.operation(&op.id).unwrap();
    assert_eq!(done.state, OperationState::Done);
    assert_eq!(done.attempts, 1);
    assert_eq!(handler.calls(), 1);
    assert_eq!(handler.seen.lock().unwrap()[0], r#"{"note":"echo"}"#);
}

#[test]
fn delayed_operation_waits_and_can_be_cancelled_before_it_starts() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let handler = Scripted::new("send", |_, _| Outcome::Done);
    engine.register_operation_handler(handler.clone());

    let op = engine
        .enqueue_operation(job("send").delay(Duration::from_secs(60)))
        .unwrap();
    assert_eq!(drain(&engine), 0);
    engine.cancel_operation(&op.id).unwrap();
    assert_eq!(state(&engine, &op.id), OperationState::Cancelled);
    assert_eq!(drain(&engine), 0);
    assert_eq!(handler.calls(), 0);
    // A cancelled operation cannot be cancelled or retried back to life.
    assert!(matches!(
        engine.retry_operation(&op.id),
        Err(MailError::Conflict(_))
    ));
}

#[test]
fn a_running_operation_cannot_be_cancelled() {
    let fixture = Fixture::new();
    let engine = Arc::new(fixture.engine());
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel::<()>();
    let finish_rx = Mutex::new(finish_rx);
    engine.register_operation_handler(Scripted::new("slow", move |_, _| {
        started_tx.send(()).unwrap();
        finish_rx.lock().unwrap().recv().unwrap();
        Outcome::Done
    }));
    let op = engine.enqueue_operation(job("slow")).unwrap();
    let worker = {
        let engine = engine.clone();
        std::thread::spawn(move || engine.run_next_operation().unwrap())
    };
    started_rx.recv().unwrap();
    // The store is not held while the handler runs, so reads stay available.
    assert_eq!(state(&engine, &op.id), OperationState::Running);
    assert!(matches!(
        engine.cancel_operation(&op.id),
        Err(MailError::Conflict(_))
    ));
    finish_tx.send(()).unwrap();
    assert!(worker.join().unwrap());
    assert_eq!(state(&engine, &op.id), OperationState::Done);
}

#[test]
fn transient_failure_backs_off_then_a_permanent_failure_waits_for_the_user() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    engine.register_operation_handler(Scripted::new("flaky", move |_, _| {
        match counter.fetch_add(1, Ordering::SeqCst) {
            0 => Outcome::Retry {
                after: Duration::ZERO,
                error: "offline".into(),
            },
            1 => Outcome::Retry {
                after: Duration::from_secs(300),
                error: "still offline".into(),
            },
            _ => Outcome::Failed("rejected".into()),
        }
    }));
    let op = engine.enqueue_operation(job("flaky")).unwrap();

    assert_eq!(drain(&engine), 2);
    let waiting = engine.operation(&op.id).unwrap();
    assert_eq!(waiting.state, OperationState::Pending);
    assert_eq!(waiting.last_error.as_deref(), Some("still offline"));
    assert!(waiting.not_before > waiting.updated + 299_000);

    // Explicit retry makes it due now; the next failure is permanent.
    engine.retry_operation(&op.id).unwrap();
    assert_eq!(drain(&engine), 1);
    let failed = engine.operation(&op.id).unwrap();
    assert_eq!(failed.state, OperationState::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("rejected"));
    assert_eq!(failed.attempts, 3);
    assert_eq!(drain(&engine), 0);

    engine.retry_operation(&op.id).unwrap();
    assert_eq!(drain(&engine), 1);
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
}

#[test]
fn unknown_outcome_is_never_retried_without_an_explicit_decision() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let handler = Scripted::new("submit", |ctx, _| {
        ctx.begin_irreversible().unwrap();
        Outcome::Unknown("connection lost after the message was sent".into())
    });
    engine.register_operation_handler(handler.clone());
    let op = engine.enqueue_operation(job("submit")).unwrap();

    assert_eq!(drain(&engine), 1);
    let unknown = engine.operation(&op.id).unwrap();
    assert_eq!(unknown.state, OperationState::Unknown);
    assert!(unknown.irreversible);
    assert_eq!(drain(&engine), 0);
    // Ordinary retry and cancel are not decisions about an unknown outcome.
    assert!(matches!(
        engine.retry_operation(&op.id),
        Err(MailError::Conflict(_))
    ));
    assert!(matches!(
        engine.cancel_operation(&op.id),
        Err(MailError::Conflict(_))
    ));
    // A restart does not retry it either.
    drop(engine);
    let engine = fixture.engine();
    engine.register_operation_handler(handler.clone());
    assert_eq!(drain(&engine), 0);
    assert_eq!(handler.calls(), 1);

    engine
        .resolve_operation(&op.id, Resolution::RunAgain)
        .unwrap();
    assert_eq!(drain(&engine), 1);
    assert_eq!(handler.calls(), 2);

    engine
        .resolve_operation(&op.id, Resolution::MarkDone)
        .unwrap();
    assert_eq!(state(&engine, &op.id), OperationState::Done);
    assert!(matches!(
        engine.resolve_operation(&op.id, Resolution::Discard),
        Err(MailError::Conflict(_))
    ));
}

#[test]
fn restart_recovers_interrupted_work_by_whether_it_passed_the_point_of_no_return() {
    let fixture = Fixture::new();
    let (safe, risky) = {
        let engine = fixture.engine();
        (
            engine.enqueue_operation(job("move")).unwrap(),
            engine.enqueue_operation(job("submit")).unwrap(),
        )
    };
    // Model a crash: both were claimed, one had started its irreversible step.
    {
        let UnlockOutcome::Opened { store, .. } =
            open_store(fixture.secrets.as_ref(), &fixture.path)
        else {
            panic!("store should open");
        };
        let conn = store.connection();
        conn.execute("UPDATE operations SET state='running'", [])
            .unwrap();
        conn.execute(
            "UPDATE operations SET irreversible=1 WHERE id=?1",
            [&risky.id],
        )
        .unwrap();
    }
    let engine = fixture.engine();
    let moves = Scripted::new("move", |_, _| Outcome::Done);
    let submits = Scripted::new("submit", |_, _| Outcome::Done);
    engine.register_operation_handler(moves.clone());
    engine.register_operation_handler(submits.clone());

    assert_eq!(state(&engine, &safe.id), OperationState::Pending);
    let interrupted = engine.operation(&risky.id).unwrap();
    assert_eq!(interrupted.state, OperationState::Unknown);
    assert!(interrupted.last_error.unwrap().contains("interrupted"));
    assert_eq!(drain(&engine), 1);
    assert_eq!((moves.calls(), submits.calls()), (1, 0));
}

#[test]
fn a_panicking_handler_fails_safely() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    engine.register_operation_handler(Scripted::new("early", |_, _| panic!("before sending")));
    engine.register_operation_handler(Scripted::new("late", |ctx, _| {
        ctx.begin_irreversible().unwrap();
        panic!("after sending")
    }));
    let early = engine.enqueue_operation(job("early")).unwrap();
    let late = engine.enqueue_operation(job("late")).unwrap();
    assert_eq!(drain(&engine), 2);
    assert_eq!(state(&engine, &early.id), OperationState::Failed);
    assert_eq!(state(&engine, &late.id), OperationState::Unknown);
}

#[test]
fn held_operations_wait_for_release() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let handler = Scripted::new("send", |_, _| Outcome::Done);
    engine.register_operation_handler(handler.clone());
    let held = engine.enqueue_operation(job("send").held()).unwrap();
    let pending = engine.enqueue_operation(job("send")).unwrap();
    engine.hold_operation(&pending.id).unwrap();

    assert_eq!(drain(&engine), 0);
    assert_eq!(state(&engine, &held.id), OperationState::Held);
    engine.release_operation(&held.id).unwrap();
    assert_eq!(drain(&engine), 1);
    assert_eq!(state(&engine, &held.id), OperationState::Done);
    assert_eq!(state(&engine, &pending.id), OperationState::Held);
}

#[test]
fn dependent_operation_waits_for_its_parent() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let order = Arc::new(Mutex::new(Vec::new()));
    let log = order.clone();
    engine.register_operation_handler(Scripted::new("step", move |_, op| {
        log.lock().unwrap().push(op.payload.clone());
        Outcome::Done
    }));
    let parent = engine
        .enqueue_operation(
            NewOperation::new("step", serde_json::json!("parent")).delay(Duration::from_secs(60)),
        )
        .unwrap();
    engine
        .enqueue_operation(NewOperation::new("step", serde_json::json!("child")).after(&parent.id))
        .unwrap();
    assert_eq!(drain(&engine), 0);
    engine.retry_operation(&parent.id).unwrap();
    assert_eq!(drain(&engine), 2);
    assert_eq!(*order.lock().unwrap(), vec![r#""parent""#, r#""child""#]);
}

#[test]
fn unregistered_kinds_stay_queued() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let op = engine.enqueue_operation(job("future-kind")).unwrap();
    assert_eq!(drain(&engine), 0);
    assert_eq!(state(&engine, &op.id), OperationState::Pending);
}

#[test]
fn nothing_runs_while_the_keyring_is_locked() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let handler = Scripted::new("send", |_, _| Outcome::Done);
    engine.register_operation_handler(handler.clone());
    engine.enqueue_operation(job("send")).unwrap();

    fixture.secrets.set_state(KeyringState::Locked);
    assert!(matches!(
        engine.run_next_operation(),
        Err(MailError::Locked)
    ));
    assert_eq!(handler.calls(), 0);
    engine.unlock().unwrap();
    assert_eq!(drain(&engine), 1);
}

#[test]
fn an_outcome_reached_while_locked_is_recorded_after_unlock_without_rerunning() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    let secrets = fixture.secrets.clone();
    let handler = Scripted::new("submit", move |ctx, _| {
        ctx.begin_irreversible().unwrap();
        secrets.set_state(KeyringState::Locked);
        Outcome::Done
    });
    engine.register_operation_handler(handler.clone());
    let op = engine.enqueue_operation(job("submit")).unwrap();

    assert!(matches!(
        engine.run_next_operation(),
        Err(MailError::Locked)
    ));
    engine.unlock().unwrap();
    assert_eq!(state(&engine, &op.id), OperationState::Done);
    assert_eq!(drain(&engine), 0);
    assert_eq!(handler.calls(), 1);
}

#[test]
fn operations_list_newest_last_with_account_filter() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    engine.enqueue_operation(job("a").account("one")).unwrap();
    engine.enqueue_operation(job("b").account("two")).unwrap();
    engine.enqueue_operation(job("c").account("one")).unwrap();
    let kinds: Vec<String> = engine
        .operations(Some("one"))
        .unwrap()
        .into_iter()
        .map(|op| op.kind)
        .collect();
    assert_eq!(kinds, vec!["a", "c"]);
    assert_eq!(engine.operations(None).unwrap().len(), 3);
}

#[test]
fn restore_refuses_to_replace_the_store_while_an_operation_is_on_the_wire() {
    let fixture = Fixture::new();
    let engine = Arc::new(fixture.engine());
    let backup = fixture._dir.path().join("before-send.pigeon-backup");
    engine
        .create_backup(&backup, "correct horse battery staple")
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let finish_rx = Mutex::new(finish_rx);
    let handler = Scripted::new("send", move |ctx, _| {
        ctx.begin_irreversible().unwrap();
        entered_tx.send(()).unwrap();
        finish_rx.lock().unwrap().recv().unwrap();
        Outcome::Done
    });
    engine.register_operation_handler(handler.clone());
    engine.enqueue_operation(job("send")).unwrap();
    let worker_engine = engine.clone();
    let worker = std::thread::spawn(move || worker_engine.run_next_operation());
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let result = engine.restore_backup(&backup, "correct horse battery staple");
    finish_tx.send(()).unwrap();
    assert!(worker.join().unwrap().unwrap());
    assert!(matches!(
        result,
        Err(MailError::Backup(pigeon_core::backup::BackupError::Busy))
    ));
    assert_eq!(handler.calls(), 1);
}
