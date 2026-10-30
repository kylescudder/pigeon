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
use pigeon_core::viewer::MAX_INLINE_DOCUMENT_BYTES;
use zeroize::Zeroizing;

const CANARY: &[u8] = b"ATTACHMENT-RETENTION-CANARY-7f3a";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic-image";

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct State {
    validity: u32,
    /// The UID the server lists; changing it models delete-then-new-mail.
    uid: u32,
    deleted: bool,
    offline: bool,
    fetches: Vec<String>,
    connects: usize,
    /// Extra embedded images in the message, each this many bytes.
    many_images: Option<(u32, usize)>,
    /// Runs once during the next part download, without the server lock.
    during_fetch: Option<Hook>,
}

const MANY_FIRST_PART: u32 = 10;

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
                    uid: state.uid,
                    unread: true,
                }]
            },
        })
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        let state = self.0.lock().unwrap();
        let mut attachments = attachments();
        let mut extra_html = String::new();
        if let Some((count, size)) = state.many_images {
            for index in 0..count {
                let part = MANY_FIRST_PART + index;
                attachments.push(Attachment {
                    part: part.to_string(),
                    filename: "Unnamed attachment".into(),
                    content_type: "image/png".into(),
                    size: size as u32,
                    content_id: Some(format!("many{part}@pigeon.test")),
                });
                extra_html.push_str(&format!("<img src=\"cid:many{part}@pigeon.test\">"));
            }
        }
        Ok(uids
            .iter()
            .map(|uid| RemoteMessage {
                uid: *uid,
                content: MessageContent {
                    sender: "Alice <alice@pigeon.test>".into(),
                    recipients: "mail@pigeon.test".into(),
                    subject: "Newsletter".into(),
                    body_text: "Synthetic newsletter".into(),
                    body_html: Some(format!(
                        "<p>Hello</p><img src=\"cid:logo@pigeon.test\" alt=\"logo\">\
                         <img src=\"cid:broken@pigeon.test\" alt=\"broken\">\
                         <img src=\"https://tracker.example/pixel.gif\" alt=\"remote\">{extra_html}"
                    )),
                    date: 1793005200,
                    attachments: attachments.clone(),
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
        let hook = self.0.lock().unwrap().during_fetch.take();
        if let Some(hook) = hook {
            hook();
        }
        let mut state = self.0.lock().unwrap();
        assert_eq!(uid, 5);
        state.fetches.push(attachment.part.clone());
        let many = attachment.part.parse::<u32>().unwrap_or(0) >= MANY_FIRST_PART;
        match attachment.part.as_str() {
            // Honest about its size, like the real adapter's metadata check.
            _ if many => {
                assert!(attachment.size as usize <= max_bytes);
                Ok(Zeroizing::new(vec![7; attachment.size as usize]))
            }
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
    engine: Arc<MailEngine>,
    account: String,
    message: i64,
}

fn fixture() -> Fixture {
    fixture_with(State::default())
}

fn fixture_with(initial: State) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let state = Arc::new(Mutex::new(State {
        validity: 9,
        uid: 5,
        ..initial
    }));
    let engine = Arc::new(MailEngine::with_connector(
        path.clone(),
        secrets.clone(),
        Arc::new(Server(state.clone())),
    ));
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
        self.engine = Arc::new(MailEngine::with_connector(
            self.path.clone(),
            self.secrets.clone(),
            Arc::new(Server(self.state.clone())),
        ));
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
    f.reopen();
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

#[test]
fn embedded_images_stop_downloading_at_the_document_budget() {
    // Twelve referenced 6 MiB images: far more than one document may hold.
    let size = 6 * 1024 * 1024;
    let f = fixture_with(State {
        many_images: Some((12, size)),
        ..Default::default()
    });
    let document = f.engine.viewer_document(f.message).unwrap();
    let state = f.state.lock().unwrap();
    let many: Vec<_> = state
        .fetches
        .iter()
        .filter(|part| part.parse::<u32>().unwrap_or(0) >= MANY_FIRST_PART)
        .collect();
    // Each download is checked against what is left, so the total fetched
    // never exceeds the budget, not just the total placed in the document.
    assert!(
        many.len() * size <= MAX_INLINE_DOCUMENT_BYTES,
        "{} images downloaded",
        many.len()
    );
    assert_eq!(many.len(), MAX_INLINE_DOCUMENT_BYTES / size);
    assert_eq!(
        document.html.matches("data:image/png").count(),
        many.len() + 1,
        "the budgeted images plus the small logo"
    );
    // Images left out for size were never attempted, so are not "missing".
    assert_eq!(document.missing_inline, 1, "only the broken part");
}

#[test]
fn kept_and_session_images_count_toward_the_document_budget() {
    let size = 6 * 1024 * 1024;
    let mut f = fixture_with(State {
        many_images: Some((12, size)),
        ..Default::default()
    });
    // Download all twelve individually and keep them, then reopen offline.
    for index in 0..12 {
        let part = (MANY_FIRST_PART + index).to_string();
        f.engine.attachment(f.message, &part, true).unwrap();
    }
    f.reopen();
    f.state.lock().unwrap().offline = true;
    let document = f.engine.viewer_document(f.message).unwrap();
    // The logo was never kept, so offline only the kept images can show.
    assert_eq!(
        document.html.matches("data:image/png").count(),
        MAX_INLINE_DOCUMENT_BYTES / size
    );
    assert_eq!(document.missing_inline, 2, "the logo and broken part");
}

#[test]
fn keeping_refuses_a_part_whose_message_row_was_reused_meanwhile() {
    let f = fixture();
    let engine = f.engine.clone();
    let state = f.state.clone();
    let account = f.account.clone();
    // While part 3 of UID 5 downloads, the server deletes UID 5 and delivers
    // UID 6. SQLite reuses the freed row id for the new message.
    f.state.lock().unwrap().during_fetch = Some(Box::new(move || {
        state.lock().unwrap().deleted = true;
        engine.sync_account(&account).unwrap();
        {
            let mut state = state.lock().unwrap();
            state.deleted = false;
            state.uid = 6;
        }
        engine.sync_account(&account).unwrap();
    }));
    let result = f.engine.attachment(f.message, "3", true);
    let reused = f.engine.search(&SearchQuery::default()).unwrap().messages[0].id;
    assert_eq!(reused, f.message, "scenario needs the row id to be reused");
    assert!(
        matches!(result, Err(MailError::NotFound)),
        "the old message's bytes must not be kept"
    );
    assert_eq!(f.engine.stored_attachment_bytes().unwrap(), 0);
    // The new message's own part 3 is fetched from the server, not the old bytes.
    f.state.lock().unwrap().fetches.clear();
    f.state.lock().unwrap().offline = true;
    assert!(matches!(
        f.engine.attachment(reused, "3", false),
        Err(MailError::Connection)
    ));
}
