//! Online-only folder writes. Persist intent before IMAP; LIST settles the
//! result before any further queued message writes are allowed.
use super::source::RemoteFolder;
use super::{MailEngine, MailError, storage_error};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FolderChange {
    Create { name: String },
    Rename { folder_id: String, name: String },
    Delete { folder_id: String, confirmed: bool },
}

pub(super) fn ensure_ready(db: &Connection, account: &str) -> Result<(), MailError> {
    let pending: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM folder_changes WHERE account=?1)",
            [account],
            |r| r.get(0),
        )
        .map_err(storage_error)?;
    if pending {
        return Err(MailError::Conflict("A folder change needs reconciliation. Sync this account before changing mail or folders.".into()));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), MailError> {
    // Until modified UTF-7 is supported by the adapter, reject non-ASCII rather
    // than silently creating a different server name. Ampersand needs encoding,
    // and LIST could not tell the wildcards from a folder's own name.
    if name.is_empty()
        || name.len() > 200
        || name.trim() != name
        || !name.is_ascii()
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '&' | '*' | '%'))
    {
        return Err(MailError::InvalidAction(
            "Use 1–200 ASCII characters, without control characters, &, *, % or leading/trailing spaces.",
        ));
    }
    if name.eq_ignore_ascii_case("inbox") {
        return Err(MailError::InvalidAction(
            "That folder already exists or is reserved.",
        ));
    }
    Ok(())
}

impl MailEngine {
    pub fn change_folder(&self, id: &str, change: FolderChange) -> Result<(), MailError> {
        // Checks that need no server come first, so they read the same offline.
        let (source_id, target) = match change {
            FolderChange::Create { name } => (None, Some(name)),
            FolderChange::Rename { folder_id, name } => (Some(folder_id), Some(name)),
            FolderChange::Delete {
                folder_id,
                confirmed,
            } => {
                if !confirmed {
                    return Err(MailError::InvalidAction(
                        "Confirm permanent folder deletion first.",
                    ));
                }
                (Some(folder_id), None)
            }
        };
        if let Some(name) = &target {
            validate_name(name)?;
        }
        let _lease = self.lease(id)?;
        let account = self
            .accounts()?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or(MailError::NotFound)?;
        self.with_store(|s| ensure_ready(s.connection(), id))?;
        let source = source_id
            .map(|fid| {
                self.folders()?
                    .into_iter()
                    .find(|f| f.id == fid && f.account_id == id)
                    .ok_or(MailError::NotFound)
            })
            .transpose()?;
        let password = self
            .secrets
            .get(&super::password_name(id))
            .map_err(|_| MailError::Locked)?
            .ok_or(MailError::PasswordNeeded)?;
        // Connect and obtain fresh hierarchy before recording any intent.
        let mut session = self.connector.connect(&account.config, &password)?;
        let remote = session.folder_hierarchy()?;
        let delimiter = source
            .as_ref()
            .and_then(|f| f.delimiter.as_deref())
            .or_else(|| remote.iter().find_map(|f| f.delimiter.as_deref()));
        if let Some(name) = &target {
            if remote.iter().any(|f| f.name.eq_ignore_ascii_case(name)) {
                return Err(MailError::InvalidAction(
                    "That folder already exists or is reserved.",
                ));
            }
            if let Some(d) = delimiter {
                if name
                    .split(d)
                    .any(|part| part.is_empty() || part.trim() != part)
                {
                    return Err(MailError::InvalidAction(
                        "Folder path components must not be empty or padded with spaces.",
                    ));
                }
                // Creation accepts full paths; renaming keeps the same parent.
                if let Some(from) = &source
                    && from.name.rsplit_once(d).map(|v| v.0) != name.rsplit_once(d).map(|v| v.0)
                {
                    return Err(MailError::InvalidAction(
                        "Rename within the same parent folder.",
                    ));
                }
            }
        }
        if let Some(from) = &source {
            let current = remote
                .iter()
                .find(|f| f.name == from.name)
                .ok_or(MailError::NotFound)?;
            if from.kind != "other"
                || current.kind != "other"
                || from.name.eq_ignore_ascii_case("inbox")
            {
                return Err(MailError::InvalidAction(
                    "Special folders cannot be renamed or deleted.",
                ));
            }
            if delimiter.is_some_and(|d| {
                remote
                    .iter()
                    .any(|f| f.name.starts_with(&format!("{}{d}", from.name)))
            }) {
                return Err(MailError::InvalidAction(
                    "Manage child folders first; only folders without children can be renamed or deleted.",
                ));
            }
        }
        let source = source.map(|f| f.name);
        self.with_store(|s| {
            let tx = s.connection().unchecked_transaction().map_err(storage_error)?;
            ensure_ready(&tx, id)?;
            let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM mail_actions WHERE account=?1) OR EXISTS(SELECT 1 FROM operations WHERE account=?1 AND state NOT IN ('done','cancelled'))", [id], |r| r.get(0)).map_err(storage_error)?;
            if busy { return Err(MailError::Conflict("Finish or discard this account’s pending mail operations before managing folders.".into())); }
            tx.execute("INSERT INTO folder_changes(account,source,target) VALUES (?1,?2,?3)", params![id,source,target]).map_err(storage_error)?;
            tx.execute("UPDATE mail_accounts SET sync_status='error',last_error='Folder change awaiting reconciliation; sync this account before further changes.' WHERE id=?1", [id]).map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        })?;
        let outcome = match (&source, &target) {
            (None, Some(to)) => session.create_folder(to),
            (Some(from), Some(to)) => session.rename_folder(from, to),
            (Some(from), None) => session.delete_folder(from),
            _ => unreachable!(),
        };
        // Neither a refusal nor a lost reply says what the server did; its
        // folder list does. Without one the next sync settles the change, and
        // an interrupted destructive command is never retried automatically.
        let Ok(listed) = session.folder_hierarchy() else {
            return Err(MailError::Conflict("The server did not confirm this folder change. Sync this account to check the result; mail changes are paused until then.".into()));
        };
        self.settle_folder_change(id, source.as_deref(), target.as_deref(), &listed)?;
        self.with_store(|s| {
            s.connection()
                .execute(
                    "UPDATE mail_accounts SET sync_status='idle',last_error=NULL WHERE id=?1",
                    [id],
                )
                .map_err(storage_error)?;
            Ok(())
        })?;
        let listed = |name: &String| listed.iter().any(|f| &f.name == name);
        let applied = !source.as_ref().is_some_and(listed) && target.as_ref().is_none_or(listed);
        match outcome {
            _ if applied => Ok(()),
            Err(error) => Err(error),
            Ok(()) => Err(MailError::Conflict(
                "The server accepted this folder change, but its folder list does not show it. Pigeon kept the server's list.".into(),
            )),
        }
    }

    pub(super) fn reconcile_folder_change(&self, id: &str) -> Result<(), MailError> {
        let pending: Option<(Option<String>, Option<String>)> = self.with_store(|s| {
            s.connection()
                .query_row(
                    "SELECT source,target FROM folder_changes WHERE account=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(storage_error)
        })?;
        let Some((source, target)) = pending else {
            return Ok(());
        };
        let account = self
            .accounts()?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or(MailError::NotFound)?;
        let password = self
            .secrets
            .get(&super::password_name(id))
            .map_err(|_| MailError::Locked)?
            .ok_or(MailError::PasswordNeeded)?;
        let mut session = self.connector.connect(&account.config, &password)?;
        let remote = session.folder_hierarchy()?;
        self.settle_folder_change(id, source.as_deref(), target.as_deref(), &remote)
    }

    /// Makes the cache match the server's folder list and clears the intent.
    /// Both names existing needs no special case: the source keeps its cache,
    /// and sync's UIDVALIDITY check discards it if that is a recreated folder.
    fn settle_folder_change(
        &self,
        id: &str,
        source: Option<&str>,
        target: Option<&str>,
        remote: &[RemoteFolder],
    ) -> Result<(), MailError> {
        let listed = |name: &&str| remote.iter().any(|f| f.name == *name);
        let moved = source.filter(|name| !listed(name));
        // Discarded mail leaves first, in batches, so a large folder does not
        // freeze other views. The intent outlives this, so an interruption
        // repeats it. The server had no folder under a rename's new name when
        // the change began, so anything cached there is stale.
        if let Some(from) = moved {
            self.remove_mail_in_batches(id, Some(target.filter(listed).unwrap_or(from)))?;
        }
        self.with_store(|s| {
            let tx = s.connection().unchecked_transaction().map_err(storage_error)?;
            if let Some(from) = moved {
                if let Some(to) = target.filter(listed) {
                    tx.execute("DELETE FROM mail_folders WHERE account=?1 AND name=?2", params![id,to]).map_err(storage_error)?;
                    tx.execute("UPDATE mail_folders SET name=?3 WHERE account=?1 AND name=?2", params![id,from,to]).map_err(storage_error)?;
                    tx.execute("UPDATE messages SET folder=?3 WHERE account=?1 AND folder=?2", params![id,from,to]).map_err(storage_error)?;
                    tx.execute("UPDATE saved_searches SET query=json_set(query,'$.folder',?3) WHERE json_extract(query,'$.account_id')=?1 AND json_extract(query,'$.folder')=?2", params![id,from,to]).map_err(storage_error)?;
                } else {
                    tx.execute("DELETE FROM mail_folders WHERE account=?1 AND name=?2", params![id,from]).map_err(storage_error)?;
                }
            }
            if let Some(to) = target
                && let Some(folder) = remote.iter().find(|f| f.name == to && f.kind != "container")
            {
                tx.execute("INSERT INTO mail_folders(account,name,kind,delimiter) VALUES (?1,?2,?3,?4) ON CONFLICT(account,name) DO UPDATE SET kind=excluded.kind,delimiter=excluded.delimiter", params![id,to,folder.kind,folder.delimiter]).map_err(storage_error)?;
            }
            tx.execute("DELETE FROM folder_changes WHERE account=?1", [id]).map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        })
    }
}
