//! On-request attachment download, retention, and embedded images, against a
//! deterministic server at the connector seam.

use std::sync::{Arc, Mutex};

use pigeon_core::mail::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use pigeon_core::mail::{
    AccountConfig, Attachment, MailEngine, MailError, MessageContent, SearchQuery,
    TransportSecurity,
};
use pigeon_core::secrets::{KeyringState, MemoryStore, Secret};
use zeroize::Zeroizing;

const CANARY: &[u8] = b"ATTACHMENT-RETENTION-CANARY-7f3a";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic-image";

#[derive(Default)]
struct State {
    validity: u32,
    deleted: bool,
    offline: bool,
    fetches: Vec<String>,
    connects: usize,
}

#[derive(Clone)]
struct Server(Arc<Mutex<State>>);
struct Session(Arc<Mutex<State>>);

impl MailConnector for Server {
    fn connect(&self, _: &AccountConfig, _: &Secret) -> Result<Box<dyn MailSession>, MailError> {
        let mut state = self.0.lock().unwrap();
        if state.offline {
            return Err(MailError::Connection);
        }
        state.connects += 1;
        Ok(Box::new(Session(self.0.clone())))
    }
}

fn attachments() -> Vec<Attachment> {
    vec![
        Attachment {
            part: "2".into(),
            filename: "Unnamed attachment".into(),
            content_type: "image/png".into(),
            size: PNG.len() as u32,
            content_id: Some("logo@pigeon.test".into()),
        },
        Attachment {
            part: "3".into(),
            filename: "report.pdf".into(),
            content_type: "application/pdf".into(),
            size: CANARY.len() as u32,
            content_id: None,
        },
        Attachment {
            part: "4".into(),
            filename: "Unnamed attachment".into(),
            content_type: "image/png".into(),
            size: 10,
            content_id: Some("<broken@pigeon.test>".into()),
        },
    ]
}

impl MailSession for Session {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        Ok(vec![RemoteFolder {
            name: "INBOX".into(),
            kind: "inbox".into(),
            delimiter: Some("/".into()),
        }])
    }
    fn snapshot(&mut self, _: &str) -> Result<FolderSnapshot, MailError> {
        let state = self.0.lock().unwrap();
        Ok(FolderSnapshot {
            uid_validity: state.validity,
            messages: if state.deleted {
                vec![]
            } else {
                vec![RemoteFlag {
                    uid: 5,
                    unread: true,
                }]
            },
        })
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        Ok(uids
            .iter()
            .map(|uid| RemoteMessage {
                uid: *uid,
                content: MessageContent {
                    sender: "Alice <alice@pigeon.test>".into(),
                    recipients: "mail@pigeon.test".into(),
                    subject: "Newsletter".into(),
                    body_text: "Synthetic newsletter".into(),
                    body_html: Some(
                        "<p>Hello</p><img src=\"cid:logo@pigeon.test\" alt=\"logo\">\
                         <img src=\"cid:broken@pigeon.test\" alt=\"broken\">\
                         <img src=\"https://tracker.example/pixel.gif\" alt=\"remote\">"
                            .into(),
                    ),
                    date: 1793005200,
                    attachments: attachments(),
                    ..Default::default()
                },
            })
            .collect())
    }
    fn fetch_part(
        &mut self,
        uid: u32,
        attachment: &Attachment,
        max_bytes: usize,
    ) -> Result<Zeroizing<Vec<u8>>, MailError> {
        let mut state = self.0.lock().unwrap();
        assert_eq!(uid, 5);
        assert!(max_bytes >= CANARY.len());
        state.fetches.push(attachment.part.clone());
        match attachment.part.as_str() {
            "2" => Ok(Zeroizing::new(PNG.to_vec())),
            "3" => Ok(Zeroizing::new(CANARY.to_vec())),
            _ => Err(MailError::Imap {
                operation: "downloading an attachment",
                reason: "the server refused the request",
            }),
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    secrets: Arc<MemoryStore>,
    state: Arc<Mutex<State>>,
    engine: MailEngine,
    account: String,
    message: i64,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let state = Arc::new(Mutex::new(State {
        validity: 9,
        ..Default::default()
    }));
    let engine = MailEngine::with_connector(
        path.clone(),
        secrets.clone(),
        Arc::new(Server(state.clone())),
    );
    let account = engine
        .add_account(
            AccountConfig {
                name: "Personal".into(),
                address: "mail@pigeon.test".into(),
                host: "imap.pigeon.test".into(),
                port: 993,
                username: "mail@pigeon.test".into(),
                security: TransportSecurity::Tls,
                ca_certificate: None,
            },
            Zeroizing::new(b"test-password".to_vec()),
        )
        .unwrap()
        .id;
    engine.sync_account(&account).unwrap();
    let message = engine.search(&SearchQuery::default()).unwrap().messages[0].id;
    state.lock().unwrap().fetches.clear();
    Fixture {
        _dir: dir,
        path,
        secrets,
        state,
        engine,
        account,
        message,
    }
}

impl Fixture {
    fn reopen(&mut self) {
        self.engine = MailEngine::with_connector(
            self.path.clone(),
            self.secrets.clone(),
            Arc::new(Server(self.state.clone())),
        );
    }
    fn files_contain(&self, needle: &[u8]) -> bool {
        let mut found = false;
        for suffix in ["", "-wal"] {
            let path = format!("{}{suffix}", self.path.display());
            if let Ok(bytes) = std::fs::read(path) {
                found |= bytes.windows(needle.len()).any(|window| window == needle);
            }
        }
        found
    }
}

#[test]
fn download_is_session_only_by_default() {
    let mut f = fixture();
    let first = f.engine.attachment(f.message, "3", false).unwrap();
    assert_eq!(first.data.as_slice(), CANARY);
    assert_eq!(first.content_type, "application/pdf");
    assert_eq!(first.filename, "report.pdf");
    assert!(!first.kept);
    // A second read in the same session uses memory, not the server.
    f.engine.attachment(f.message, "3", false).unwrap();
    assert_eq!(f.state.lock().unwrap().fetches, ["3"]);
    assert!(!f.files_contain(CANARY));

    // After restart the payload is gone and needs the server again.
    f.reopen();
    f.state.lock().unwrap().offline = true;
    assert!(matches!(
        f.engine.attachment(f.message, "3", false),
        Err(MailError::Connection)
    ));
    f.state.lock().unwrap().offline = false;
    f.engine.attachment(f.message, "3", false).unwrap();
    assert_eq!(f.state.lock().unwrap().fetches, ["3", "3"]);
}

#[test]
fn kept_download_survives_restart_offline_and_stays_encrypted() {
    let mut f = fixture();
    // Keeping something already in the session cache needs no second fetch.
    f.engine.attachment(f.message, "3", false).unwrap();
    assert!(f.engine.attachment(f.message, "3", true).unwrap().kept);
    assert_eq!(f.state.lock().unwrap().fetches, ["3"]);

    f.reopen();
    f.state.lock().unwrap().offline = true;
    let kept = f.engine.attachment(f.message, "3", false).unwrap();
    assert_eq!(kept.data.as_slice(), CANARY);
    assert!(kept.kept);
    drop(f.engine);
    f.engine = MailEngine::with_connector(
        f.path.clone(),
        f.secrets.clone(),
        Arc::new(Server(f.state.clone())),
    );
    assert!(!f.files_contain(CANARY), "plaintext payload on disk");
}

#[test]
fn kept_payload_is_removed_with_its_message() {
    let f = fixture();
    f.engine.attachment(f.message, "3", true).unwrap();
    f.state.lock().unwrap().deleted = true;
    f.engine.sync_account(&f.account).unwrap();
    assert!(matches!(
        f.engine.attachment(f.message, "3", false),
        Err(MailError::NotFound)
    ));
    assert_eq!(f.engine.stored_attachment_bytes().unwrap(), 0);
}

#[test]
fn kept_payloads_go_with_the_account() {
    let f = fixture();
    f.engine.attachment(f.message, "3", true).unwrap();
    assert_eq!(
        f.engine.stored_attachment_bytes().unwrap(),
        CANARY.len() as u64
    );
    f.engine.remove_account(&f.account).unwrap();
    assert_eq!(f.engine.stored_attachment_bytes().unwrap(), 0);
}

#[test]
fn changed_folder_epoch_refuses_instead_of_fetching_another_message() {
    let f = fixture();
    f.state.lock().unwrap().validity = 10;
    assert!(matches!(
        f.engine.attachment(f.message, "3", false),
        Err(MailError::Protocol)
    ));
    assert!(f.state.lock().unwrap().fetches.is_empty());
}

#[test]
fn only_parts_listed_for_the_message_can_be_requested() {
    let f = fixture();
    for part in ["1", "9", "3.1", "", "BODY[]"] {
        assert!(
            matches!(
                f.engine.attachment(f.message, part, false),
                Err(MailError::NotFound)
            ),
            "{part}"
        );
    }
    assert!(matches!(
        f.engine.attachment(f.message + 100, "3", false),
        Err(MailError::NotFound)
    ));
    assert!(f.state.lock().unwrap().fetches.is_empty());
}

#[test]
fn locking_the_keyring_discards_session_downloads() {
    let f = fixture();
    f.engine.attachment(f.message, "3", false).unwrap();
    f.secrets.set_state(KeyringState::Locked);
    assert!(matches!(
        f.engine.attachment(f.message, "3", false),
        Err(MailError::Locked)
    ));
    f.secrets.set_state(KeyringState::Unlocked);
    f.engine.attachment(f.message, "3", false).unwrap();
    assert_eq!(f.state.lock().unwrap().fetches, ["3", "3"]);
}

#[test]
fn viewer_document_embeds_referenced_images_with_one_connection() {
    let f = fixture();
    let connects = f.state.lock().unwrap().connects;
    let document = f.engine.viewer_document(f.message).unwrap();
    assert_eq!(document.missing_inline, 1, "the broken part");
    assert!(document.html.contains("data:image/png;base64,"));
    assert!(!document.html.contains("cid:"));
    assert!(!document.html.contains("tracker.example"));
    assert!(document.html.contains("<img alt=\"broken\">"));
    let state = f.state.lock().unwrap();
    // Only referenced images are fetched; the PDF stays on the server.
    let mut fetched = state.fetches.clone();
    fetched.sort();
    assert_eq!(fetched, ["2", "4"]);
    assert_eq!(state.connects, connects + 1);
}

#[test]
fn viewer_document_opens_offline_without_embedded_images() {
    let f = fixture();
    f.state.lock().unwrap().offline = true;
    let document = f.engine.viewer_document(f.message).unwrap();
    assert_eq!(document.missing_inline, 2);
    assert!(document.html.contains("Hello"));
    assert!(!document.html.contains("cid:"));
}

#[test]
fn keep_setting_applies_to_embedded_images() {
    let mut f = fixture();
    assert!(!f.engine.keep_attachments().unwrap());
    f.engine.set_keep_attachments(true).unwrap();
    f.engine.viewer_document(f.message).unwrap();
    f.reopen();
    assert!(f.engine.keep_attachments().unwrap());
    f.state.lock().unwrap().offline = true;
    let document = f.engine.viewer_document(f.message).unwrap();
    assert_eq!(document.missing_inline, 1);
    assert!(document.html.contains("data:image/png;base64,"));
}
