//! Attachment payloads, downloaded only when asked for. A download is held in
//! memory for the session unless the user keeps it, in which case it is
//! stored inside the encrypted database alongside its message.

use std::collections::{HashSet, VecDeque};

use rusqlite::{OptionalExtension, params};
use zeroize::Zeroizing;

use super::cache::VISIBLE;
use super::{Attachment, MailEngine, MailError, now, storage_error};
use crate::viewer::{self, InlineImage, InlineImages, RemoteContent};

/// Largest decoded attachment Pigeon downloads.
pub const MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;
/// Upper bound for session-only payloads held in memory at once.
const SESSION_CACHE_BYTES: usize = 256 * 1024 * 1024;
const KEEP_SETTING: &str = "keep_attachments";

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
        if let Some(data) = self.kept_part(message, part)? {
            return Ok(result(data, true));
        }
        let data = match self.session_part(&located.key(part)) {
            Some(data) => data,
            None => self
                .download(&located, &[attachment], MAX_ATTACHMENT_BYTES)?
                .pop()
                .ok_or(MailError::Protocol)??,
        };
        if keep {
            self.keep_part(message, attachment, &data)?;
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
            let mut wanted = Vec::new();
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
                let cached = match self.kept_part(id, &attachment.part)? {
                    Some(data) => Some(data),
                    None => self.session_part(&located.key(&attachment.part)),
                };
                match cached {
                    Some(data) => {
                        inline.insert(cid, inline_image(attachment, &data));
                    }
                    None => wanted.push((cid, attachment)),
                }
            }
            if !wanted.is_empty() {
                let parts: Vec<&Attachment> = wanted.iter().map(|(_, a)| *a).collect();
                match self.download(&located, &parts, viewer::MAX_INLINE_IMAGE_BYTES) {
                    Ok(results) => {
                        let keep = self.keep_attachments()?;
                        for ((cid, attachment), result) in wanted.into_iter().zip(results) {
                            match result {
                                Ok(data) => {
                                    if keep {
                                        self.keep_part(id, attachment, &data)?;
                                    }
                                    inline.insert(cid, inline_image(attachment, &data));
                                }
                                Err(MailError::Locked) => return Err(MailError::Locked),
                                Err(_) => missing += 1,
                            }
                        }
                    }
                    Err(MailError::Locked) => return Err(MailError::Locked),
                    // Offline or refused: show the message without its images.
                    Err(_) => missing += wanted.len() as u32,
                }
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

    fn kept_part(&self, message: i64, part: &str) -> Result<Option<Zeroizing<Vec<u8>>>, MailError> {
        self.with_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT data FROM attachment_blobs WHERE message=?1 AND part=?2",
                    params![message, part],
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

    fn keep_part(
        &self,
        message: i64,
        attachment: &Attachment,
        data: &[u8],
    ) -> Result<(), MailError> {
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO attachment_blobs(message,part,content_type,data,stored_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(message,part) DO NOTHING",
                    params![message, attachment.part, attachment.content_type, data, now()],
                )
                .map_err(|error| match error {
                    // The message was removed while its attachment downloaded.
                    rusqlite::Error::SqliteFailure(code, _)
                        if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        MailError::NotFound
                    }
                    error => storage_error(error),
                })?;
            Ok(())
        })
    }

    /// Fetches parts over one connection, outside the store mutex. The outer
    /// error is the connection; each part then succeeds or fails on its own.
    /// The folder epoch is checked first, so a reused UID is never fetched.
    #[allow(clippy::type_complexity)]
    fn download(
        &self,
        located: &Located,
        parts: &[&Attachment],
        max_bytes: usize,
    ) -> Result<Vec<Result<Zeroizing<Vec<u8>>, MailError>>, MailError> {
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
        Ok(parts
            .iter()
            .map(|attachment| {
                let data = session.fetch_part(located.uid, attachment, max_bytes)?;
                if data.len() > max_bytes {
                    return Err(MailError::Protocol);
                }
                self.parts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(located.key(&attachment.part), data.clone());
                Ok(data)
            })
            .collect())
    }
}

fn inline_image(attachment: &Attachment, data: &[u8]) -> InlineImage {
    InlineImage {
        content_type: attachment.content_type.clone(),
        data: data.to_vec(),
    }
}
