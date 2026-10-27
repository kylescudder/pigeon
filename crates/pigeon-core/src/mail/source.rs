//! External mail-server seam. Production uses verified TLS IMAP; tests can
//! supply a deterministic server at this same boundary for fault injection.

use super::{AccountConfig, MailError, MessageContent};
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
    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError>;
}

#[derive(Debug, Clone)]
pub struct RemoteFolder {
    pub name: String,
    pub kind: String,
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
