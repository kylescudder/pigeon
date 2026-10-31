//! The durable action queue against a deterministic server model. The model
//! keeps per-folder UIDs, flags, and an "other client" \Deleted message, and can
//! drop the connection just before or just after a write reaches it. Real
//! Dovecot coverage of the same paths lives in imap_server.rs.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
    WriteCapabilities,
};
use pigeon_core::mail::{
    AccountConfig, ActionKind, MailEngine, MailError, MessageContent, MessageSummary, SearchQuery,
    TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use pigeon_core::unlock::{UnlockOutcome, open_store};
use zeroize::Zeroizing;

#[derive(Clone)]
struct Mail {
    subject: String,
    seen: bool,
    deleted: bool,
}

struct Folder {
    name: String,
    kind: &'static str,
    validity: u32,
    next_uid: u32,
    mail: BTreeMap<u32, Mail>,
}

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    /// The connection drops before the next write reaches the server.
    DropBefore,
    /// The next write is applied, then the connection drops before the reply.
    DropAfter,
}

struct Server {
    folders: Vec<Folder>,
    caps: WriteCapabilities,
    offline: bool,
    fault: Option<Fault>,
    /// Every pre-write check returns an unusable response.
    malformed: bool,
    /// UID EXPUNGE is refused with NO.
    reject_expunge: bool,
    log: Vec<String>,
}

impl Server {
    fn new(folders: &[(&str, &'static str)]) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            folders: folders
                .iter()
                .map(|(name, kind)| Folder {
                    name: (*name).into(),
                    kind,
                    validity: 1,
                    next_uid: 1,
                    mail: BTreeMap::new(),
                })
                .collect(),
            caps: WriteCapabilities {
                moves: true,
                uidplus: true,
            },
            offline: false,
            fault: None,
            malformed: false,
            reject_expunge: false,
            log: Vec::new(),
        }))
    }
    fn folder(&mut self, name: &str) -> &mut Folder {
        self.folders.iter_mut().find(|f| f.name == name).unwrap()
    }
    fn deliver(&mut self, folder: &str, subject: &str) -> u32 {
        let folder = self.folder(folder);
        let uid = folder.next_uid;
        folder.next_uid += 1;
        folder.mail.insert(
            uid,
            Mail {
                subject: subject.into(),
                seen: false,
                deleted: false,
            },
        );
        uid
    }
    fn subjects(&mut self, folder: &str) -> Vec<String> {
        self.folder(folder)
            .mail
            .values()
            .map(|m| m.subject.clone())
            .collect()
    }
    fn find(&mut self, folder: &str, subject: &str) -> Option<Mail> {
        self.folder(folder)
            .mail
            .values()
            .find(|m| m.subject == subject)
            .cloned()
    }
    fn remove(&mut self, folder: &str, subject: &str) {
        self.folder(folder).mail.retain(|_, m| m.subject != subject);
    }
    /// Consumes a pending fault for a write command.
    fn write(&mut self, command: String, apply: impl FnOnce(&mut Self)) -> Result<(), MailError> {
        match self.fault.take() {
            Some(Fault::DropBefore) => return Err(MailError::Connection),
            Some(Fault::DropAfter) => {
                self.log.push(command);
                apply(self);
                return Err(MailError::Connection);
            }
            None => {}
        }
        self.log.push(command);
        apply(self);
        Ok(())
    }
}

#[derive(Clone)]
struct Connector(Arc<Mutex<Server>>);
struct Session {
    server: Arc<Mutex<Server>>,
    selected: Option<String>,
}
impl MailConnector for Connector {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        if self.0.lock().unwrap().offline {
            return Err(MailError::Connection);
        }
        Ok(Box::new(Session {
            server: self.0.clone(),
            selected: None,
        }))
    }
}
fn rejected() -> MailError {
    MailError::Imap {
        operation: "test",
        reason: "the server rejected the command (IMAP NO)",
    }
}
impl MailSession for Session {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(self
            .server
            .lock()
            .unwrap()
            .folders
            .iter()
            .map(|f| RemoteFolder {
                name: f.name.clone(),
                kind: f.kind.into(),
                delimiter: Some("/".into()),
            })
            .collect())
    }
    fn snapshot(&mut self, name: &str) -> Result<FolderSnapshot, MailError> {
        let mut server = self.server.lock().unwrap();
        let folder = server.folder(name);
        self.selected = Some(name.into());
        Ok(FolderSnapshot {
            uid_validity: folder.validity,
            messages: folder
                .mail
                .iter()
                .filter(|(_, m)| !m.deleted)
                .map(|(uid, m)| RemoteFlag {
                    uid: *uid,
                    unread: !m.seen,
                })
                .collect(),
        })
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        let mut server = self.server.lock().unwrap();
        let folder = server.folder(self.selected.as_ref().unwrap());
        Ok(uids
            .iter()
            .map(|uid| RemoteMessage {
                uid: *uid,
                content: MessageContent {
                    sender: "Alice <alice@pigeon.test>".into(),
                    recipients: "mail@pigeon.test".into(),
                    subject: folder.mail[uid].subject.clone(),
                    body_text: "synthetic body".into(),
                    date: 1793005200 + i64::from(*uid),
                    ..Default::default()
                },
            })
            .collect())
    }
    fn capabilities(&mut self) -> Result<WriteCapabilities, MailError> {
        Ok(self.server.lock().unwrap().caps)
    }
    fn select_writable(&mut self, name: &str) -> Result<u32, MailError> {
        let server = self.server.lock().unwrap();
        let folder = server
            .folders
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(rejected)?;
        self.selected = Some(name.into());
        Ok(folder.validity)
    }
    fn present(&mut self, uids: &[u32]) -> Result<Vec<RemoteFlag>, MailError> {
        let mut server = self.server.lock().unwrap();
        if server.malformed {
            return Err(MailError::Protocol);
        }
        let folder = server.folder(self.selected.as_ref().unwrap());
        Ok(uids
            .iter()
            .filter_map(|uid| {
                folder.mail.get(uid).map(|m| RemoteFlag {
                    uid: *uid,
                    unread: !m.seen,
                })
            })
            .collect())
    }
    fn set_seen(&mut self, uids: &[u32], seen: bool) -> Result<(), MailError> {
        let selected = self.selected.clone().unwrap();
        let uids = uids.to_vec();
        self.server
            .lock()
            .unwrap()
            .write(format!("STORE {selected} {uids:?} seen={seen}"), |s| {
                for uid in &uids {
                    if let Some(m) = s.folder(&selected).mail.get_mut(uid) {
                        m.seen = seen;
                    }
                }
            })
    }
    fn move_to(&mut self, uids: &[u32], target: &str) -> Result<(), MailError> {
        let mut server = self.server.lock().unwrap();
        if !server.caps.moves {
            return Err(rejected());
        }
        if !server.folders.iter().any(|f| f.name == target) {
            return Err(rejected());
        }
        let selected = self.selected.clone().unwrap();
        let uids = uids.to_vec();
        server.write(format!("MOVE {selected} {uids:?} {target}"), |s| {
            for uid in &uids {
                if let Some(m) = s.folder(&selected).mail.remove(uid) {
                    let target = s.folder(target);
                    target.mail.insert(target.next_uid, m);
                    target.next_uid += 1;
                }
            }
        })
    }
    fn copy_to(&mut self, uids: &[u32], target: &str) -> Result<(), MailError> {
        let mut server = self.server.lock().unwrap();
        let selected = self.selected.clone().unwrap();
        let uids = uids.to_vec();
        server.write(format!("COPY {selected} {uids:?} {target}"), |s| {
            for uid in &uids {
                if let Some(m) = s.folder(&selected).mail.get(uid).cloned() {
                    let target = s.folder(target);
                    target.mail.insert(target.next_uid, m);
                    target.next_uid += 1;
                }
            }
        })
    }
    fn expunge_uids(&mut self, uids: &[u32]) -> Result<(), MailError> {
        let mut server = self.server.lock().unwrap();
        if !server.caps.uidplus || server.reject_expunge {
            return Err(rejected());
        }
        let selected = self.selected.clone().unwrap();
        let uids = uids.to_vec();
        server.write(format!("UID EXPUNGE {selected} {uids:?}"), |s| {
            for uid in &uids {
                s.folder(&selected).mail.remove(uid);
            }
        })
    }
}

fn config() -> AccountConfig {
    AccountConfig {
        name: "Personal".into(),
        address: "mail@pigeon.test".into(),
        host: "imap.pigeon.test".into(),
        port: 993,
        username: "mail@pigeon.test".into(),
        security: TransportSecurity::Tls,
        ca_certificate: None,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    secrets: Arc<MemoryStore>,
    server: Arc<Mutex<Server>>,
    engine: MailEngine,
    account: String,
}

impl Fixture {
    fn new(server: Arc<Mutex<Server>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.db");
        let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
        let engine = MailEngine::with_connector(
            path.clone(),
            secrets.clone(),
            Arc::new(Connector(server.clone())),
        );
        let account = engine
            .add_account(config(), Zeroizing::new(b"test-password".to_vec()))
            .unwrap()
            .id;
        engine.sync_account(&account).unwrap();
        Self {
            _dir: dir,
            path,
            secrets,
            server,
            engine,
            account,
        }
    }
    fn restart(&mut self) {
        self.engine = MailEngine::with_connector(
            self.path.clone(),
            self.secrets.clone(),
            Arc::new(Connector(self.server.clone())),
        );
    }
    fn local(&self, folder: &str) -> Vec<MessageSummary> {
        let mut messages = self
            .engine
            .search(&SearchQuery {
                folder: Some(folder.into()),
                ..Default::default()
            })
            .unwrap()
            .messages;
        messages.sort_by(|a, b| a.subject.cmp(&b.subject));
        messages
    }
    fn id(&self, folder: &str, subject: &str) -> i64 {
        self.local(folder)
            .iter()
            .find(|m| m.subject == subject)
            .unwrap_or_else(|| panic!("{subject} not in local {folder}"))
            .id
    }
    fn subjects(&self, folder: &str) -> Vec<String> {
        self.local(folder).into_iter().map(|m| m.subject).collect()
    }
    fn folder_id(&self, name: &str) -> String {
        self.engine
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap()
            .id
    }
    fn server(&self) -> std::sync::MutexGuard<'_, Server> {
        self.server.lock().unwrap()
    }
}

fn standard() -> Fixture {
    let server = Server::new(&[
        ("INBOX", "inbox"),
        ("Archive", "archive"),
        ("Trash", "trash"),
    ]);
    {
        let mut s = server.lock().unwrap();
        for subject in ["Alpha", "Bravo", "Charlie"] {
            s.deliver("INBOX", subject);
        }
    }
    Fixture::new(server)
}

#[test]
fn offline_mark_read_shows_locally_survives_restart_and_reaches_the_server() {
    let mut f = standard();
    f.server().offline = true;
    let alpha = f.id("INBOX", "Alpha");
    assert_eq!(
        f.engine
            .queue_actions(&[alpha], ActionKind::MarkRead)
            .unwrap(),
        1
    );
    assert!(!f.local("INBOX")[0].unread, "optimistic local read state");
    assert!(matches!(
        f.engine.apply_actions(&f.account),
        Err(MailError::Connection)
    ));
    let pending = f.engine.pending_actions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "queued");
    assert!(pending[0].error.is_some(), "the failure is visible");
    assert_eq!(f.engine.action_counts().unwrap().pending, 1);

    f.restart();
    assert!(f.engine.sync_account(&f.account).is_err());
    assert_eq!(f.engine.pending_actions().unwrap().len(), 1);
    assert!(!f.local("INBOX")[0].unread);
    assert!(!f.server().find("INBOX", "Alpha").unwrap().seen);

    f.server().offline = false;
    f.engine.sync_account(&f.account).unwrap();
    assert!(f.server().find("INBOX", "Alpha").unwrap().seen);
    assert!(f.engine.pending_actions().unwrap().is_empty());
    assert!(!f.local("INBOX")[0].unread);
    // Already-read messages are not queued again.
    assert_eq!(
        f.engine
            .queue_actions(&[alpha], ActionKind::MarkRead)
            .unwrap(),
        0
    );
    f.engine
        .queue_actions(&[alpha], ActionKind::MarkUnread)
        .unwrap();
    f.engine.apply_actions(&f.account).unwrap();
    assert!(!f.server().find("INBOX", "Alpha").unwrap().seen);
}

#[test]
fn trash_and_archive_move_mail_on_the_server_and_it_arrives_after_sync() {
    let f = standard();
    let alpha = f.id("INBOX", "Alpha");
    let bravo = f.id("INBOX", "Bravo");
    f.engine.queue_actions(&[alpha], ActionKind::Trash).unwrap();
    f.engine
        .queue_actions(&[bravo], ActionKind::Archive)
        .unwrap();
    assert_eq!(f.subjects("INBOX"), ["Charlie"], "hidden while pending");
    assert_eq!(f.engine.pending_actions().unwrap().len(), 2);
    f.engine.sync_account(&f.account).unwrap();
    assert!(f.engine.pending_actions().unwrap().is_empty());
    assert_eq!(f.server().subjects("INBOX"), ["Charlie"]);
    assert_eq!(f.server().subjects("Trash"), ["Alpha"]);
    assert_eq!(f.server().subjects("Archive"), ["Bravo"]);
    assert_eq!(f.subjects("INBOX"), ["Charlie"]);
    assert_eq!(f.subjects("Trash"), ["Alpha"]);
    assert_eq!(f.subjects("Archive"), ["Bravo"]);
    assert!(
        !f.server().log.iter().any(|c| c.contains("EXPUNGE")),
        "a move never expunges"
    );
    // Moving from Trash to Trash is meaningless; the UI offers permanent delete.
    let trashed = f.id("Trash", "Alpha");
    assert!(matches!(
        f.engine.queue_actions(&[trashed], ActionKind::Trash),
        Err(MailError::InvalidAction(_))
    ));
    // An explicit move back to the inbox.
    f.engine
        .queue_actions(
            &[trashed],
            ActionKind::Move {
                folder_id: f.folder_id("INBOX"),
            },
        )
        .unwrap();
    f.engine.sync_account(&f.account).unwrap();
    assert_eq!(f.subjects("INBOX"), ["Alpha", "Charlie"]);
}

#[test]
fn without_trash_delete_is_refused_and_permanent_delete_spares_other_clients_mail() {
    let server = Server::new(&[("INBOX", "inbox")]);
    {
        let mut s = server.lock().unwrap();
        s.deliver("INBOX", "Keep");
        s.deliver("INBOX", "Remove");
        // Another client flagged this \Deleted without expunging it yet.
        let uid = s.deliver("INBOX", "Other client's pending deletion");
        s.folder("INBOX").mail.get_mut(&uid).unwrap().deleted = true;
    }
    let f = Fixture::new(server);
    let remove = f.id("INBOX", "Remove");
    assert!(matches!(
        f.engine.queue_actions(&[remove], ActionKind::Trash),
        Err(MailError::NoTrashFolder)
    ));
    assert!(f.engine.pending_actions().unwrap().is_empty());
    f.engine
        .queue_actions(&[remove], ActionKind::DeletePermanently)
        .unwrap();
    f.engine.apply_actions(&f.account).unwrap();
    assert_eq!(
        f.server().subjects("INBOX"),
        ["Keep", "Other client's pending deletion"]
    );
    assert_eq!(f.subjects("INBOX"), ["Keep"]);
    assert!(f.engine.pending_actions().unwrap().is_empty());
}

#[test]
fn permanent_delete_without_uidplus_fails_visibly_and_removes_nothing() {
    let f = standard();
    f.server().caps.uidplus = false;
    let alpha = f.id("INBOX", "Alpha");
    f.engine
        .queue_actions(&[alpha], ActionKind::DeletePermanently)
        .unwrap();
    assert_eq!(f.subjects("INBOX"), ["Bravo", "Charlie"]);
    f.engine.apply_actions(&f.account).unwrap();
    let pending = f.engine.pending_actions().unwrap();
    assert_eq!(pending[0].state, "failed");
    assert!(
        pending[0]
            .error
            .as_deref()
            .unwrap()
            .contains("does not support"),
        "{pending:?}"
    );
    assert_eq!(f.engine.action_counts().unwrap().failed, 1);
    assert_eq!(f.server().subjects("INBOX"), ["Alpha", "Bravo", "Charlie"]);
    assert!(f.server().log.is_empty(), "no write was sent");
    assert_eq!(
        f.subjects("INBOX"),
        ["Alpha", "Bravo", "Charlie"],
        "a failed change no longer hides the message"
    );
    // The server gains UIDPLUS; an explicit retry applies it.
    f.server().caps.uidplus = true;
    f.engine.retry_action(pending[0].id).unwrap();
    assert_eq!(f.subjects("INBOX"), ["Bravo", "Charlie"]);
    f.engine.apply_actions(&f.account).unwrap();
    assert_eq!(f.server().subjects("INBOX"), ["Bravo", "Charlie"]);
    assert!(f.engine.pending_actions().unwrap().is_empty());
}

#[test]
fn remote_deletion_before_apply_is_a_conflict_and_is_never_resurrected() {
    let f = standard();
    f.server().offline = true;
    let alpha = f.id("INBOX", "Alpha");
    let bravo = f.id("INBOX", "Bravo");
    f.engine
        .queue_actions(&[alpha], ActionKind::Archive)
        .unwrap();
    f.engine
        .queue_actions(&[bravo], ActionKind::MarkRead)
        .unwrap();
    {
        let mut s = f.server();
        s.remove("INBOX", "Alpha");
        s.remove("INBOX", "Bravo");
        s.offline = false;
    }
    f.engine.sync_account(&f.account).unwrap();
    let pending = f.engine.pending_actions().unwrap();
    assert_eq!(pending.len(), 1, "a read flag on deleted mail is dropped");
    assert_eq!(pending[0].state, "conflict");
    assert_eq!(pending[0].kind, "archive");
    assert_eq!(pending[0].conflicts, 1);
    assert!(pending[0].error.as_deref().unwrap().contains("server"));
    assert_eq!(f.engine.action_counts().unwrap().conflict, 1);
    assert!(f.server().subjects("Archive").is_empty());
    assert_eq!(f.server().subjects("INBOX"), ["Charlie"]);
    assert_eq!(f.subjects("INBOX"), ["Charlie"]);
    assert!(
        f.server().log.is_empty(),
        "nothing was written or recreated"
    );
    // Retrying keeps the conflict rather than guessing where the mail went.
    f.engine.retry_action(pending[0].id).unwrap();
    f.engine.apply_actions(&f.account).unwrap();
    assert_eq!(f.engine.pending_actions().unwrap()[0].state, "conflict");
    f.engine.discard_action(pending[0].id).unwrap();
    assert!(f.engine.pending_actions().unwrap().is_empty());
    assert_eq!(f.subjects("INBOX"), ["Charlie"]);
}

#[test]
fn a_changed_uidvalidity_turns_queued_changes_into_conflicts() {
    let f = standard();
    f.server().offline = true;
    let alpha = f.id("INBOX", "Alpha");
    f.engine.queue_actions(&[alpha], ActionKind::Trash).unwrap();
    {
        let mut s = f.server();
        s.folder("INBOX").validity = 2;
        s.offline = false;
    }
    f.engine.apply_actions(&f.account).unwrap();
    let pending = f.engine.pending_actions().unwrap();
    assert_eq!(pending[0].state, "conflict");
    assert!(f.server().log.is_empty());
    assert!(f.server().subjects("Trash").is_empty());
}

#[test]
fn a_connection_lost_around_a_write_neither_repeats_nor_duplicates_it() {
    let f = standard();
    let alpha = f.id("INBOX", "Alpha");
    f.server().fault = Some(Fault::DropAfter);
    f.engine
        .queue_actions(&[alpha], ActionKind::Archive)
        .unwrap();
    assert!(matches!(
        f.engine.apply_actions(&f.account),
        Err(MailError::Connection)
    ));
    assert_eq!(f.engine.pending_actions().unwrap()[0].state, "queued");
    f.engine.apply_actions(&f.account).unwrap();
    assert!(f.engine.pending_actions().unwrap().is_empty());
    assert_eq!(f.server().subjects("Archive"), ["Alpha"]);
    assert_eq!(
        f.server()
            .log
            .iter()
            .filter(|c| c.starts_with("MOVE"))
            .count(),
        1
    );

    let bravo = f.id("INBOX", "Bravo");
    f.server().fault = Some(Fault::DropBefore);
    f.engine
        .queue_actions(&[bravo], ActionKind::MarkRead)
        .unwrap();
    assert!(f.engine.apply_actions(&f.account).is_err());
    assert!(!f.server().find("INBOX", "Bravo").unwrap().seen);
    f.engine.apply_actions(&f.account).unwrap();
    assert!(f.server().find("INBOX", "Bravo").unwrap().seen);
}

#[test]
fn copy_fallback_moves_without_move_and_stops_on_an_uncertain_copy() {
    let f = standard();
    {
        let mut s = f.server();
        s.caps.moves = false;
        let uid = s.deliver("INBOX", "Other client's pending deletion");
        s.folder("INBOX").mail.get_mut(&uid).unwrap().deleted = true;
    }
    let alpha = f.id("INBOX", "Alpha");
    f.engine
        .queue_actions(&[alpha], ActionKind::Archive)
        .unwrap();
    f.engine.apply_actions(&f.account).unwrap();
    assert_eq!(f.server().subjects("Archive"), ["Alpha"]);
    assert_eq!(
        f.server().subjects("INBOX"),
        ["Bravo", "Charlie", "Other client's pending deletion"]
    );

    let bravo = f.id("INBOX", "Bravo");
    f.server().fault = Some(Fault::DropAfter);
    f.engine
        .queue_actions(&[bravo], ActionKind::Archive)
        .unwrap();
    assert!(f.engine.apply_actions(&f.account).is_err());
    f.engine.apply_actions(&f.account).unwrap();
    let pending = f.engine.pending_actions().unwrap();
    assert_eq!(pending[0].state, "conflict", "{pending:?}");
    assert!(pending[0].error.as_deref().unwrap().contains("copied"));
    assert_eq!(
        f.server().subjects("Archive"),
        ["Alpha", "Bravo"],
        "not copied twice"
    );
    assert_eq!(f.subjects("INBOX"), ["Bravo", "Charlie"]);
}

#[test]
fn an_interrupted_apply_resumes_after_restart() {
    let mut f = standard();
    f.server().offline = true;
    let alpha = f.id("INBOX", "Alpha");
    f.engine.queue_actions(&[alpha], ActionKind::Trash).unwrap();
    drop(std::mem::replace(
        &mut f.engine,
        MailEngine::new(f.path.clone(), f.secrets.clone()),
    ));
    // Model a crash while the change was being applied.
    {
        let UnlockOutcome::Opened { store, .. } = open_store(f.secrets.as_ref(), &f.path) else {
            panic!("store did not open");
        };
        store
            .connection()
            .execute("UPDATE mail_actions SET state='applying', attempted=1", [])
            .unwrap();
    }
    f.server().offline = false;
    f.restart();
    assert_eq!(f.engine.pending_actions().unwrap()[0].state, "queued");
    f.engine.sync_account(&f.account).unwrap();
    assert!(f.engine.pending_actions().unwrap().is_empty());
    assert_eq!(f.server().subjects("Trash"), ["Alpha"]);
}

#[test]
fn discard_restores_the_local_view_without_writing() {
    let f = standard();
    f.server().offline = true;
    let alpha = f.id("INBOX", "Alpha");
    let bravo = f.id("INBOX", "Bravo");
    f.engine
        .queue_actions(&[alpha, bravo], ActionKind::MarkRead)
        .unwrap();
    f.engine.queue_actions(&[alpha], ActionKind::Trash).unwrap();
    assert_eq!(f.subjects("INBOX"), ["Bravo", "Charlie"]);
    for action in f.engine.pending_actions().unwrap() {
        f.engine.discard_action(action.id).unwrap();
    }
    let inbox = f.local("INBOX");
    assert_eq!(inbox.len(), 3);
    assert!(inbox.iter().all(|m| m.unread), "prior read state restored");
    assert!(f.server().log.is_empty());
}

#[test]
fn queueing_needs_an_unlocked_store_and_validates_requests() {
    let f = standard();
    let alpha = f.id("INBOX", "Alpha");
    assert!(matches!(
        f.engine.queue_actions(&[], ActionKind::MarkRead),
        Err(MailError::InvalidAction(_))
    ));
    assert!(matches!(
        f.engine.queue_actions(&[i64::MAX], ActionKind::MarkRead),
        Err(MailError::NotFound)
    ));
    assert!(matches!(
        f.engine.queue_actions(
            &[alpha],
            ActionKind::Move {
                folder_id: "missing".into()
            }
        ),
        Err(MailError::NotFound)
    ));
    f.secrets.set_state(KeyringState::Locked);
    assert!(matches!(
        f.engine.queue_actions(&[alpha], ActionKind::MarkRead),
        Err(MailError::Locked)
    ));
    assert!(matches!(
        f.engine.apply_actions(&f.account),
        Err(MailError::Locked)
    ));
    f.secrets.set_state(KeyringState::Unlocked);
    assert!(f.server().log.is_empty());
}

#[test]
fn queued_changes_are_encrypted_at_rest() {
    let server = Server::new(&[("INBOX", "inbox"), ("CANARYqueueFOLDER", "other")]);
    server.lock().unwrap().deliver("INBOX", "Alpha");
    let f = Fixture::new(server);
    f.server().offline = true;
    let alpha = f.id("INBOX", "Alpha");
    f.engine
        .queue_actions(
            &[alpha],
            ActionKind::Move {
                folder_id: f.folder_id("CANARYqueueFOLDER"),
            },
        )
        .unwrap();
    let needle = b"CANARYqueueFOLDER";
    for name in ["mail.db", "mail.db-wal"] {
        let path = f.path.with_file_name(name);
        if let Ok(bytes) = std::fs::read(&path) {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "plaintext in {name}"
            );
        }
    }
}

#[test]
fn a_change_the_server_answers_unusably_fails_without_blocking_sync() {
    let f = standard();
    let alpha = f.id("INBOX", "Alpha");
    f.engine.queue_actions(&[alpha], ActionKind::Trash).unwrap();
    {
        let mut s = f.server();
        s.malformed = true;
        s.deliver("INBOX", "Delta");
    }
    f.engine.sync_account(&f.account).unwrap();
    assert_eq!(f.engine.pending_actions().unwrap()[0].state, "failed");
    assert_eq!(f.subjects("INBOX"), ["Alpha", "Bravo", "Charlie", "Delta"]);
    assert!(f.server().log.is_empty());
}

#[test]
fn retrying_after_a_refused_source_deletion_does_not_copy_again() {
    let f = standard();
    {
        let mut s = f.server();
        s.caps.moves = false;
        s.reject_expunge = true;
    }
    let alpha = f.id("INBOX", "Alpha");
    f.engine
        .queue_actions(&[alpha], ActionKind::Archive)
        .unwrap();
    f.engine.apply_actions(&f.account).unwrap();
    let pending = f.engine.pending_actions().unwrap();
    assert_eq!(pending[0].state, "failed", "{pending:?}");
    assert_eq!(
        f.server().subjects("Archive"),
        ["Alpha"],
        "the copy was confirmed"
    );
    for _ in 0..2 {
        f.engine.retry_action(pending[0].id).unwrap();
        f.engine.apply_actions(&f.account).unwrap();
    }
    f.server().reject_expunge = false;
    f.engine.retry_action(pending[0].id).unwrap();
    f.engine.apply_actions(&f.account).unwrap();
    assert!(f.engine.pending_actions().unwrap().is_empty());
    assert_eq!(
        f.server().subjects("Archive"),
        ["Alpha"],
        "never copied twice"
    );
    assert_eq!(f.server().subjects("INBOX"), ["Bravo", "Charlie"]);
    assert_eq!(
        f.server()
            .log
            .iter()
            .filter(|c| c.starts_with("COPY"))
            .count(),
        1
    );
}

#[test]
fn discarding_one_of_overlapping_read_changes_shows_the_remaining_one() {
    let f = standard();
    f.server().offline = true;
    let alpha = f.id("INBOX", "Alpha");
    let unread = |f: &Fixture| {
        f.local("INBOX")
            .iter()
            .find(|m| m.subject == "Alpha")
            .unwrap()
            .unread
    };
    f.engine
        .queue_actions(&[alpha], ActionKind::MarkRead)
        .unwrap();
    f.engine
        .queue_actions(&[alpha], ActionKind::MarkUnread)
        .unwrap();
    assert!(unread(&f));
    let pending = f.engine.pending_actions().unwrap();
    // Discard the later Mark unread: Mark read is what remains.
    f.engine.discard_action(pending[1].id).unwrap();
    assert!(!unread(&f), "the remaining Mark read shows locally");
    f.server().offline = false;
    f.engine.apply_actions(&f.account).unwrap();
    assert!(f.server().find("INBOX", "Alpha").unwrap().seen);
    assert!(!unread(&f));
    // The local state agrees with the server, so Mark unread is not skipped.
    assert_eq!(
        f.engine
            .queue_actions(&[alpha], ActionKind::MarkUnread)
            .unwrap(),
        1
    );
    f.engine.apply_actions(&f.account).unwrap();
    assert!(!f.server().find("INBOX", "Alpha").unwrap().seen);

    // Discarding both, earliest first, restores the server's last known state.
    let bravo = f.id("INBOX", "Bravo");
    f.engine
        .queue_actions(&[bravo], ActionKind::MarkRead)
        .unwrap();
    f.engine.apply_actions(&f.account).unwrap();
    f.server().offline = true;
    assert_eq!(
        f.engine
            .queue_actions(&[bravo], ActionKind::MarkUnread)
            .unwrap(),
        1
    );
    f.engine
        .queue_actions(&[bravo], ActionKind::MarkRead)
        .unwrap();
    let pending = f.engine.pending_actions().unwrap();
    f.engine.discard_action(pending[0].id).unwrap();
    assert!(
        !f.local("INBOX")
            .iter()
            .find(|m| m.subject == "Bravo")
            .unwrap()
            .unread,
        "the remaining Mark read shows locally"
    );
    f.engine.discard_action(pending[1].id).unwrap();
    assert!(
        !f.local("INBOX")
            .iter()
            .find(|m| m.subject == "Bravo")
            .unwrap()
            .unread,
        "Bravo was read on the server all along"
    );
}
