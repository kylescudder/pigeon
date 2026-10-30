//! External mail-server seam. Production uses verified TLS IMAP; tests can
//! supply a deterministic server at this same boundary for fault injection.

use zeroize::Zeroizing;

use super::{AccountConfig, Attachment, MailError, MessageContent};
use crate::secrets::Secret;

pub trait MailConnector: Send + Sync {
    fn connect(
        &self,
        config: &AccountConfig,
        password: &Secret,
    ) -> Result<Box<dyn MailSession>, MailError>;
}

pub trait MailSession: Send {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError>;
    fn snapshot(&mut self, folder: &str) -> Result<FolderSnapshot, MailError>;
    /// Restore selection on a new connection without trusting old sequence state.
    fn reselect(&mut self, folder: &str, expected_validity: u32) -> Result<(), MailError> {
        if self.snapshot(folder)?.uid_validity != expected_validity {
            return Err(MailError::Protocol);
        }
        Ok(())
    }
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError>;
    /// Downloads and decodes one MIME part of a message in the selected
    /// folder, without changing its read state. `attachment` is the cached
    /// metadata; an adapter should refuse if the server's structure no longer
    /// matches it, or if the part is larger than `max_bytes`.
    fn fetch_part(
        &mut self,
        _uid: u32,
        _attachment: &Attachment,
        _max_bytes: usize,
    ) -> Result<Zeroizing<Vec<u8>>, MailError> {
        Err(MailError::Imap {
            operation: "downloading an attachment",
            reason: "this connection cannot download attachments",
        })
    }
}

#[derive(Debug, Clone)]
pub struct RemoteFolder {
    pub name: String,
    pub kind: String,
    /// The server's hierarchy separator, such as "/" or ".". None for a flat
    /// mailbox or a server that does not report one.
    pub delimiter: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteFlag {
    pub uid: u32,
    pub unread: bool,
}

#[derive(Debug, Clone)]
pub struct FolderSnapshot {
    pub uid_validity: u32,
    pub messages: Vec<RemoteFlag>,
}

#[derive(Debug, Clone)]
pub struct RemoteMessage {
    pub uid: u32,
    pub content: MessageContent,
}
