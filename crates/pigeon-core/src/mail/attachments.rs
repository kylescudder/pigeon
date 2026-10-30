//! Attachment payloads, downloaded only when asked for. A download is held in
//! memory for the session unless the user keeps it, in which case it is
//! stored inside the encrypted database alongside its message.

use std::collections::{HashSet, VecDeque};

use rusqlite::{OptionalExtension, params};
use zeroize::Zeroizing;

use super::cache::VISIBLE;
use super::source::MailSession;
use super::{Attachment, MailEngine, MailError, now, storage_error};
use crate::viewer::{self, InlineImage, InlineImages, RemoteContent};

/// Largest decoded attachment Pigeon downloads.
pub const MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;
/// Upper bound for session-only payloads held in memory at once.
const SESSION_CACHE_BYTES: usize = 256 * 1024 * 1024;
const KEEP_SETTING: &str = "keep_attachments";

/// The largest transfer size that can decode to at most `decoded` bytes:
/// base64 is the largest common encoding, 4 bytes per 3 plus line breaks.
pub(super) fn encoded_limit(decoded: usize) -> usize {
    decoded / 3 * 4 + decoded / 38 + 4096
}

/// SQL matching a message row to the server identity captured before a
/// download. Row ids can be reused once a message is removed.
const SAME_MESSAGE: &str =
    "m.id=?1 AND m.account=?2 AND m.folder=?3 AND m.uid_validity=?4 AND m.uid=?5";

pub struct AttachmentData {
    pub filename: String,
    pub content_type: String,
    pub data: Zeroizing<Vec<u8>>,
    /// Whether the payload is stored in the encrypted database.
    pub kept: bool,
}

/// A complete viewer document for a message's formatted view.
pub struct ViewerDocument {
    pub html: String,
    /// Embedded images the message refers to that could not be downloaded.
    pub missing_inline: u32,
}

/// Where a cached message lives on the server, as last synchronised.
struct Located {
    message: i64,
    account: String,
    folder: String,
    validity: u32,
    uid: u32,
    attachments: Vec<Attachment>,
}

/// Session entries are keyed by server identity as well as row id, so a
/// reused row id can never be served another message's payload.
#[derive(Clone, PartialEq, Eq)]
struct PartKey {
    message: i64,
    account: String,
    folder: String,
    validity: u32,
    uid: u32,
    part: String,
}

impl Located {
    fn key(&self, part: &str) -> PartKey {
        PartKey {
            message: self.message,
            account: self.account.clone(),
            folder: self.folder.clone(),
            validity: self.validity,
            uid: self.uid,
            part: part.to_owned(),
        }
    }
}

#[derive(Default)]
pub(super) struct PartCache {
    entries: VecDeque<(PartKey, Zeroizing<Vec<u8>>)>,
    bytes: usize,
}

impl PartCache {
    fn get(&mut self, key: &PartKey) -> Option<Zeroizing<Vec<u8>>> {
        let index = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(index)?;
        let data = entry.1.clone();
        self.entries.push_back(entry);
        Some(data)
    }

    fn insert(&mut self, key: PartKey, data: Zeroizing<Vec<u8>>) {
        self.retain(|k| *k != key);
        if data.len() > SESSION_CACHE_BYTES {
            return;
        }
        while self.bytes + data.len() > SESSION_CACHE_BYTES {
            let Some((_, old)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.len();
        }
        self.bytes += data.len();
        self.entries.push_back((key, data));
    }

    fn retain(&mut self, keep: impl Fn(&PartKey) -> bool) {
        self.entries.retain(|(key, _)| keep(key));
        self.bytes = self.entries.iter().map(|(_, data)| data.len()).sum();
    }
}

impl MailEngine {
    /// Returns one attachment of a cached message. The payload comes from the
    /// encrypted store if kept, else this session's memory, else the server.
    /// With `keep`, it is then stored in the encrypted database.
    pub fn attachment(
        &self,
        message: i64,
        part: &str,
        keep: bool,
    ) -> Result<AttachmentData, MailError> {
        let located = self.locate(message)?;
        let attachment = located
            .attachments
            .iter()
            .find(|attachment| attachment.part == part)
            .ok_or(MailError::NotFound)?;
        let result = |data, kept| AttachmentData {
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            data,
            kept,
        };
        if let Some(data) = self.kept_part(&located, part)? {
            return Ok(result(data, true));
        }
        let data = match self.session_part(&located.key(part)) {
            Some(data) => data,
            None => {
                let mut session = self.part_session(&located)?;
                self.fetch_part(&mut session, &located, attachment, MAX_ATTACHMENT_BYTES)?
            }
        };
        if keep {
            self.keep_part(&located, attachment, &data)?;
        }
        Ok(result(data, keep))
    }

    /// Builds the sanitised formatted view of a message. Embedded images it
    /// refers to are downloaded over the mail connection and inlined; the
    /// document itself never loads anything from the network.
    pub fn viewer_document(&self, id: i64) -> Result<ViewerDocument, MailError> {
        let message = self.message(id)?;
        let html = message.body_html.unwrap_or_else(|| {
            format!(
                "<pre>{}</pre>",
                message
                    .body_text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            )
        });
        let referenced: HashSet<String> = viewer::referenced_cids(&html).into_iter().collect();
        let mut inline = InlineImages::new();
        let mut missing = 0;
        // A message without a server identity still opens, just without images.
        let located = match referenced.is_empty() {
            true => None,
            false => match self.locate(id) {
                Ok(located) => Some(located),
                Err(MailError::NotFound) => None,
                Err(error) => return Err(error),
            },
        };
        if let Some(located) = located {
            let keep = self.keep_attachments()?;
            // The document budget bounds what is read or downloaded, not only
            // what is finally placed in the document.
            let mut left = viewer::MAX_INLINE_DOCUMENT_BYTES;
            // One connection for all of a message's images, opened on demand.
            let mut session: Option<Box<dyn MailSession>> = None;
            let mut unreachable = false;
            for attachment in &located.attachments {
                let Some(cid) = attachment
                    .content_id
                    .as_deref()
                    .map(viewer::normalise_cid)
                    .filter(|cid| referenced.contains(cid))
                else {
                    continue;
                };
                // Non-image parts are never shown inline, so never fetched for it.
                if !attachment.content_type.starts_with("image/") {
                    continue;
                }
                let max = left.min(viewer::MAX_INLINE_IMAGE_BYTES);
                // Too large for what is left: shown by its alt text, unfetched.
                if max == 0 || attachment.size as usize > encoded_limit(max) {
                    continue;
                }
                let cached = match self.kept_part(&located, &attachment.part)? {
                    Some(data) => Some(data),
                    None => self.session_part(&located.key(&attachment.part)),
                };
                let data = match cached {
                    Some(data) => data,
                    None if unreachable => {
                        missing += 1;
                        continue;
                    }
                    None => {
                        let fetched = match session.as_mut() {
                            Some(session) => self.fetch_part(session, &located, attachment, max),
                            None => self.part_session(&located).and_then(|opened| {
                                let session = session.insert(opened);
                                self.fetch_part(session, &located, attachment, max)
                            }),
                        };
                        match fetched {
                            Ok(data) => {
                                if keep {
                                    self.keep_part(&located, attachment, &data)?;
                                }
                                data
                            }
                            Err(MailError::Locked) => return Err(MailError::Locked),
                            // Offline or refused: show the message without it.
                            Err(error) => {
                                // Stop trying once the connection itself is gone.
                                unreachable |= session.is_none()
                                    || matches!(
                                        error,
                                        MailError::Connection
                                            | MailError::Tls
                                            | MailError::Imap {
                                                reason: "the server closed the connection"
                                                    | "the server response timed out",
                                                ..
                                            }
                                    );
                                missing += 1;
                                continue;
                            }
                        }
                    }
                };
                if data.len() > max {
                    continue;
                }
                left -= data.len();
                inline.insert(cid, inline_image(attachment, &data));
            }
        }
        Ok(ViewerDocument {
            html: viewer::document(
                &viewer::sanitise_inline(&html, RemoteContent::Blocked, &inline),
                RemoteContent::Blocked,
            ),
            missing_inline: missing,
        })
    }

    /// Whether downloads are kept in the encrypted store automatically.
    pub fn keep_attachments(&self) -> Result<bool, MailError> {
        self.with_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT value FROM settings WHERE key=?1",
                    [KEEP_SETTING],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map(|value| value.as_deref() == Some("true"))
                .map_err(storage_error)
        })
    }

    pub fn set_keep_attachments(&self, keep: bool) -> Result<(), MailError> {
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO settings(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![KEEP_SETTING, if keep { "true" } else { "false" }],
                )
                .map_err(storage_error)?;
            Ok(())
        })
    }

    /// Total size of kept attachment payloads.
    pub fn stored_attachment_bytes(&self) -> Result<u64, MailError> {
        self.with_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT coalesce(sum(length(data)),0) FROM attachment_blobs",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|bytes| bytes as u64)
                .map_err(storage_error)
        })
    }

    /// Drops session-only payloads, for one account or all of them.
    pub(super) fn forget_session_parts(&self, account: Option<&str>) {
        let mut parts = self.parts.lock().unwrap_or_else(|e| e.into_inner());
        match account {
            Some(account) => parts.retain(|key| key.account != account),
            None => *parts = PartCache::default(),
        }
    }

    fn locate(&self, message: i64) -> Result<Located, MailError> {
        self.with_store(|store| {
            store
                .connection()
                .query_row(
                    &format!(
                        "SELECT m.account,m.folder,m.uid_validity,m.uid,m.attachments FROM messages m JOIN mail_folders f ON f.account=m.account AND f.name=m.folder WHERE m.id=?1 AND {VISIBLE}"
                    ),
                    [message],
                    |row| {
                        let attachments: String = row.get(4)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<u32>>(2)?,
                            row.get::<_, Option<u32>>(3)?,
                            attachments,
                        ))
                    },
                )
                .optional()
                .map_err(storage_error)
        })?
        .and_then(|(account, folder, validity, uid, attachments)| {
            Some(Located {
                message,
                account,
                folder,
                validity: validity?,
                uid: uid?,
                attachments: serde_json::from_str(&attachments).ok()?,
            })
        })
        .ok_or(MailError::NotFound)
    }

    fn kept_part(
        &self,
        located: &Located,
        part: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, MailError> {
        self.with_store(|store| {
            store
                .connection()
                .query_row(
                    &format!(
                        "SELECT b.data FROM attachment_blobs b JOIN messages m ON m.id=b.message WHERE {SAME_MESSAGE} AND b.part=?6"
                    ),
                    params![
                        located.message,
                        located.account,
                        located.folder,
                        located.validity,
                        located.uid,
                        part
                    ],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map(|data| data.map(Zeroizing::new))
                .map_err(storage_error)
        })
    }

    fn session_part(&self, key: &PartKey) -> Option<Zeroizing<Vec<u8>>> {
        self.parts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
    }

    /// Stores a downloaded part, but only if the row still belongs to the
    /// message it was downloaded from; checked in the same statement.
    fn keep_part(
        &self,
        located: &Located,
        attachment: &Attachment,
        data: &[u8],
    ) -> Result<(), MailError> {
        self.with_store(|store| {
            let db = store.connection();
            let identity = params![
                located.message,
                located.account,
                located.folder,
                located.validity,
                located.uid
            ];
            let inserted = db
                .execute(
                    &format!(
                        "INSERT INTO attachment_blobs(message,part,content_type,data,stored_at) SELECT m.id,?6,?7,?8,?9 FROM messages m WHERE {SAME_MESSAGE} ON CONFLICT(message,part) DO NOTHING"
                    ),
                    params![
                        located.message,
                        located.account,
                        located.folder,
                        located.validity,
                        located.uid,
                        attachment.part,
                        attachment.content_type,
                        data,
                        now()
                    ],
                )
                .map_err(storage_error)?;
            // Nothing inserted: either already kept, or the message is gone.
            if inserted == 0
                && !db
                    .query_row(
                        &format!("SELECT EXISTS(SELECT 1 FROM messages m WHERE {SAME_MESSAGE})"),
                        identity,
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(storage_error)?
            {
                return Err(MailError::NotFound);
            }
            Ok(())
        })
    }

    /// Connects for part downloads, outside the store mutex. The folder epoch
    /// is checked first, so a reused UID is never fetched.
    fn part_session(&self, located: &Located) -> Result<Box<dyn MailSession>, MailError> {
        let account = self
            .accounts()?
            .into_iter()
            .find(|account| account.id == located.account)
            .ok_or(MailError::NotFound)?;
        let password = self
            .secrets
            .get(&format!("account:{}:password", account.id))
            .map_err(|_| MailError::Locked)?
            .ok_or(MailError::Authentication)?;
        let mut session = self.connector.connect(&account.config, &password)?;
        session.reselect(&located.folder, located.validity)?;
        Ok(session)
    }

    /// Downloads one part and holds it in the session cache.
    fn fetch_part(
        &self,
        session: &mut Box<dyn MailSession>,
        located: &Located,
        attachment: &Attachment,
        max_bytes: usize,
    ) -> Result<Zeroizing<Vec<u8>>, MailError> {
        let data = session.fetch_part(located.uid, attachment, max_bytes)?;
        if data.len() > max_bytes {
            return Err(MailError::Protocol);
        }
        self.parts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(located.key(&attachment.part), data.clone());
        Ok(data)
    }
}

fn inline_image(attachment: &Attachment, data: &[u8]) -> InlineImage {
    InlineImage {
        content_type: attachment.content_type.clone(),
        data: data.to_vec(),
    }
}
