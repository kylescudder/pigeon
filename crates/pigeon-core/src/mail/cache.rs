use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, Row, params, params_from_iter};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use super::source::{FolderSnapshot, RemoteFolder, RemoteMessage};
use super::{
    Folder, MailEngine, MailError, Message, MessageSummary, SavedSearch, SearchQuery,
    SearchResults, storage_error,
};

/// Ids remembered per account between notifications, and how many of the
/// newest messages are described. Beyond the first, arrivals are only counted.
const ARRIVAL_IDS: usize = 500;
const ARRIVAL_SAMPLE: usize = 10;

/// Unread Inbox mail that arrived, with the newest few described.
#[derive(Debug, Default)]
pub struct Arrivals {
    pub total: usize,
    pub newest: Vec<MessageSummary>,
}

#[derive(Default)]
pub(super) struct Arrived {
    total: usize,
    ids: Vec<i64>,
}

const SUMMARY: &str = "m.id,m.account,m.folder,m.sender,m.recipients,m.subject,substr(m.body_text,1,180),m.date,m.unread,m.has_attachments";
/// Current folder epoch, and not hidden by an outstanding move or deletion.
pub(super) const VISIBLE: &str =
    "(f.uid_validity IS NULL OR m.uid_validity=f.uid_validity) AND m.local_hidden=0";

fn summary(row: &Row<'_>) -> rusqlite::Result<MessageSummary> {
    Ok(MessageSummary {
        id: row.get(0)?,
        account_id: row.get(1)?,
        folder: row.get(2)?,
        sender: row.get(3)?,
        recipients: row.get(4)?,
        subject: row.get(5)?,
        preview: row.get(6)?,
        date: row.get(7)?,
        unread: row.get(8)?,
        has_attachments: row.get(9)?,
    })
}

impl MailEngine {
    /// Takes what newly arrived in an account's Inbox since the last call, for
    /// notifications: unread mail that sync stored, however its row id was
    /// chosen. Mail that has since been read, moved or deleted is left out.
    pub fn take_arrivals(&self, account: &str) -> Result<Arrivals, MailError> {
        let arrived = self
            .arrivals
            .lock()
            .map_err(|_| MailError::Busy)?
            .remove(account)
            .unwrap_or_default();
        if arrived.ids.is_empty() {
            return Ok(Arrivals::default());
        }
        let (found, newest) = self.with_store(|store| {
            let marks = vec!["?"; arrived.ids.len()].join(",");
            let sql = format!(
                "SELECT {SUMMARY} FROM messages m JOIN mail_folders f ON f.account=m.account AND f.name=m.folder WHERE m.account=? AND m.id IN ({marks}) AND m.unread=1 AND f.kind='inbox' AND {VISIBLE} ORDER BY m.date DESC"
            );
            let mut values: Vec<rusqlite::types::Value> = vec![account.to_owned().into()];
            values.extend(arrived.ids.iter().map(|id| (*id).into()));
            let all: Vec<MessageSummary> = store
                .connection()
                .prepare(&sql)
                .map_err(storage_error)?
                .query_map(params_from_iter(values), summary)
                .map_err(storage_error)?
                .collect::<rusqlite::Result<_>>()
                .map_err(storage_error)?;
            Ok((all.len(), all.into_iter().take(ARRIVAL_SAMPLE).collect::<Vec<_>>()))
        })?;
        // Arrivals beyond the remembered ids still count toward the total.
        let uncounted = arrived.total.saturating_sub(arrived.ids.len());
        Ok(Arrivals {
            total: found + uncounted,
            newest,
        })
    }

    pub fn folders(&self) -> Result<Vec<Folder>, MailError> {
        self.with_store(|store| {
            let mut statement = store.connection().prepare("SELECT f.account,f.name,f.kind,f.total,count(m.id),coalesce(sum(m.unread),0),f.id,f.delimiter FROM mail_folders f LEFT JOIN messages m INDEXED BY messages_folder_counts ON m.account=f.account AND m.folder=f.name AND m.uid_validity=f.uid_validity AND m.local_hidden=0 GROUP BY f.account,f.name ORDER BY f.account,CASE WHEN f.kind='inbox' THEN 0 ELSE 1 END,f.name").map_err(storage_error)?;
            statement.query_map([], |row| Ok(Folder { id: row.get(6)?, account_id: row.get(0)?, name: row.get(1)?, kind: row.get(2)?, delimiter: row.get(7)?, total: row.get::<_,i64>(3)? as u64, cached: row.get::<_,i64>(4)? as u64, unread: row.get::<_,i64>(5)? as u64 })).map_err(storage_error)?.collect::<rusqlite::Result<_>>().map_err(storage_error)
        })
    }
    pub(super) fn prepare_folder(
        &self,
        id: &str,
        folder: &RemoteFolder,
        snapshot: &FolderSnapshot,
    ) -> Result<HashSet<u32>, MailError> {
        self.with_store(|store| {
            let db = store.connection();
            db.execute("INSERT INTO mail_folders(account,name,kind,uid_validity,total,delimiter) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(account,name) DO UPDATE SET kind=excluded.kind,total=excluded.total,delimiter=excluded.delimiter", params![id,folder.name,folder.kind,snapshot.uid_validity,snapshot.messages.len() as i64,folder.delimiter]).map_err(storage_error)?;
            let known: HashSet<u32> = db.prepare("SELECT uid FROM messages WHERE account=?1 AND folder=?2 AND uid_validity=?3").map_err(storage_error)?.query_map(params![id,folder.name,snapshot.uid_validity], |row| row.get(0)).map_err(storage_error)?.collect::<rusqlite::Result<_>>().map_err(storage_error)?;
            let cached = snapshot.messages.iter().filter(|message| known.contains(&message.uid)).count();
            db.execute("UPDATE mail_accounts SET sync_folder=?2,sync_cached=?3,sync_total=?4 WHERE id=?1", params![id,folder.name,cached as i64,snapshot.messages.len() as i64]).map_err(storage_error)?;
            Ok(known)
        })
    }

    pub(super) fn persist_batch(
        &self,
        id: &str,
        folder: &str,
        validity: u32,
        messages: &[RemoteMessage],
        flags: &HashMap<u32, bool>,
        inbox: bool,
    ) -> Result<(), MailError> {
        let mut stored_unread = Vec::new();
        self.with_store(|store| {
            stored_unread.clear();
            let tx = store.connection().unchecked_transaction().map_err(storage_error)?;
            {
                let mut insert = tx.prepare("INSERT INTO messages(account,folder,uid_validity,uid,sender,recipients,subject,body_text,body_html,date,unread,has_attachments,message_id,attachments,reply_to,reply_refs,to_list,cc_list) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18) ON CONFLICT(account,folder,uid_validity,uid) DO NOTHING").map_err(storage_error)?;
                for message in messages {
                    let content = &message.content;
                    let attachments = serde_json::to_string(&content.attachments).map_err(|_| MailError::Protocol)?;
                    let unread = flags.get(&message.uid).copied().unwrap_or(true);
                    let stored = insert.execute(params![id,folder,validity,message.uid,content.sender,content.recipients,content.subject,content.body_text,content.body_html,content.date,unread,!content.attachments.is_empty(),content.message_id,attachments,content.reply_to,content.references,content.to_list,content.cc_list]).map_err(storage_error)?;
                    if stored == 1 && unread && inbox {
                        stored_unread.push(tx.last_insert_rowid());
                    }
                }
            }
            tx.execute("UPDATE mail_accounts SET sync_cached=sync_cached+?2 WHERE id=?1", params![id,messages.len() as i64]).map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        })?;
        if !stored_unread.is_empty()
            && let Ok(mut arrivals) = self.arrivals.lock()
        {
            let arrived = arrivals.entry(id.to_owned()).or_default();
            arrived.total += stored_unread.len();
            let room = ARRIVAL_IDS.saturating_sub(arrived.ids.len());
            arrived.ids.extend(stored_unread.into_iter().take(room));
        }
        Ok(())
    }

    pub(super) fn complete_folder(
        &self,
        id: &str,
        folder: &str,
        snapshot: &FolderSnapshot,
    ) -> Result<(), MailError> {
        self.with_store(|store| {
            let tx = store.connection().unchecked_transaction().map_err(storage_error)?;
            tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS sync_uids(uid INTEGER PRIMARY KEY); DELETE FROM sync_uids;").map_err(storage_error)?;
            {
                let mut insert = tx.prepare("INSERT INTO sync_uids(uid) VALUES (?1)").map_err(storage_error)?;
                // A queued read/unread change wins until it is applied or released.
                let mut update = tx.prepare("UPDATE messages SET unread=?5 WHERE account=?1 AND folder=?2 AND uid_validity=?3 AND uid=?4 AND unread!=?5 AND flags_pending=0").map_err(storage_error)?;
                for message in &snapshot.messages {
                    insert.execute([message.uid]).map_err(storage_error)?;
                    update.execute(params![id,folder,snapshot.uid_validity,message.uid,message.unread]).map_err(storage_error)?;
                }
            }
            tx.execute("DELETE FROM messages WHERE account=?1 AND folder=?2 AND (uid_validity!=?3 OR uid NOT IN (SELECT uid FROM sync_uids))", params![id,folder,snapshot.uid_validity]).map_err(storage_error)?;
            tx.execute("UPDATE mail_folders SET uid_validity=?3 WHERE account=?1 AND name=?2", params![id,folder,snapshot.uid_validity]).map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        })
    }

    pub(super) fn remove_absent_folders(
        &self,
        id: &str,
        folders: &[RemoteFolder],
    ) -> Result<(), MailError> {
        self.with_store(|store| {
            let tx = store.connection().unchecked_transaction().map_err(storage_error)?;
            tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS sync_folders(name TEXT PRIMARY KEY); DELETE FROM sync_folders;").map_err(storage_error)?;
            for folder in folders { tx.execute("INSERT INTO sync_folders(name) VALUES (?1)", [&folder.name]).map_err(storage_error)?; }
            tx.execute("DELETE FROM messages WHERE account=?1 AND folder NOT IN (SELECT name FROM sync_folders)", [id]).map_err(storage_error)?;
            tx.execute("DELETE FROM mail_folders WHERE account=?1 AND name NOT IN (SELECT name FROM sync_folders)", [id]).map_err(storage_error)?;
            tx.commit().map_err(storage_error)
        })
    }

    pub fn search(&self, query: &SearchQuery) -> Result<SearchResults, MailError> {
        self.search_request(
            &format!(
                "local-{}",
                super::NEXT_SEARCH.fetch_add(1, Ordering::Relaxed)
            ),
            query,
        )
    }

    pub fn search_request(
        &self,
        request_id: &str,
        query: &SearchQuery,
    ) -> Result<SearchResults, MailError> {
        if request_id.len() > 128 {
            return Err(MailError::InvalidSearch(
                "request identifier is too long".into(),
            ));
        }
        let plan = super::query::compile(query)?;
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut searches = self.searches.lock().map_err(|_| MailError::Busy)?;
            if let Some(existing) = searches.get(request_id) {
                if existing.load(Ordering::SeqCst) {
                    searches.remove(request_id);
                    return Err(MailError::Cancelled);
                }
                return Err(MailError::Busy);
            }
            searches.insert(request_id.to_owned(), cancel.clone());
        }
        let _lease = super::SyncLease {
            active: &self.searches,
            id: request_id.to_owned(),
            cancel: cancel.clone(),
        };
        self.with_store(|store| {
            super::check_cancel(&cancel)?;
            let deadline = Instant::now() + Duration::from_secs(10);
            store.connection().progress_handler(1000, Some(move || cancel.load(Ordering::Relaxed) || Instant::now() > deadline)).map_err(storage_error)?;
            let _progress = SearchProgress(store.connection());
            let mut conditions = vec![VISIBLE.to_owned()];
            let mut values = plan.values;
            conditions.extend(plan.filters);
            let fts_join = if !plan.text.is_empty() { conditions.push("messages_fts MATCH ?".into()); values.push(plan.text.into()); "JOIN messages_fts ON messages_fts.rowid=m.id" } else { "" };
            let from = |hint: &str| format!("FROM messages m {hint} JOIN mail_folders f ON f.account=m.account AND f.name=m.folder {fts_join} WHERE {}", conditions.join(" AND "));
            let count_from = from("INDEXED BY messages_search_metadata");
            let total = store.connection().query_row(&format!("SELECT count(*) {count_from}"), params_from_iter(values.iter()), |row| row.get::<_,i64>(0)).map_err(search_error)?;
            values.push(i64::from(query.limit.unwrap_or(50).clamp(1,100)).into());
            values.push(i64::from(query.offset.unwrap_or(0)).into());
            let page_from = from(if fts_join.is_empty() { "" } else { "INDEXED BY messages_search_metadata" });
            let page_sql = format!("WITH page(id) AS MATERIALIZED (SELECT m.id {page_from} ORDER BY m.date DESC,m.id DESC LIMIT ? OFFSET ?) SELECT {SUMMARY} FROM page JOIN messages m ON m.id=page.id ORDER BY m.date DESC,m.id DESC");
            let mut statement = store.connection().prepare(&page_sql).map_err(storage_error)?;
            let messages = statement.query_map(params_from_iter(values.iter()), summary).map_err(search_error)?.collect::<rusqlite::Result<_>>().map_err(search_error)?;
            Ok(SearchResults { messages, total: total as u64 })
        })
    }

    pub fn save_search(&self, name: &str, query: &SearchQuery) -> Result<SavedSearch, MailError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 100 {
            return Err(MailError::InvalidSearch(
                "name the search using up to 100 characters".into(),
            ));
        }
        let mut query = query.clone();
        query.offset = None;
        query.limit = None;
        self.search(&query)?;
        let mut id = [0u8; 16];
        getrandom::fill(&mut id)
            .map_err(|_| MailError::Storage("could not create search identifier".into()))?;
        let saved = SavedSearch {
            id: hex::encode(id),
            name: name.to_owned(),
            query,
        };
        self.with_store(|store| {
            let query = serde_json::to_string(&saved.query)
                .map_err(|_| MailError::InvalidSearch("could not save the query".into()))?;
            store
                .connection()
                .execute(
                    "INSERT INTO saved_searches(id,name,query) VALUES (?1,?2,?3)",
                    params![saved.id, saved.name, query],
                )
                .map_err(storage_error)?;
            Ok(())
        })?;
        Ok(saved)
    }

    pub fn saved_searches(&self) -> Result<Vec<SavedSearch>, MailError> {
        self.with_store(|store| {
            let mut statement = store
                .connection()
                .prepare("SELECT id,name,query FROM saved_searches ORDER BY rowid")
                .map_err(storage_error)?;
            statement
                .query_map([], |row| {
                    let query: String = row.get(2)?;
                    Ok(SavedSearch {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        query: serde_json::from_str(&query).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    })
                })
                .map_err(storage_error)?
                .collect::<rusqlite::Result<_>>()
                .map_err(storage_error)
        })
    }

    pub fn delete_saved_search(&self, id: &str) -> Result<(), MailError> {
        self.with_store(|store| {
            store
                .connection()
                .execute("DELETE FROM saved_searches WHERE id=?1", [id])
                .map_err(storage_error)?;
            Ok(())
        })
    }

    pub fn message(&self, id: i64) -> Result<Message, MailError> {
        self.with_store(|store| {
            store.connection().query_row(&format!("SELECT {SUMMARY},m.body_text,m.body_html,m.attachments FROM messages m JOIN mail_folders f ON f.account=m.account AND f.name=m.folder WHERE m.id=?1 AND {VISIBLE}"), [id], |row| {
                let html: Option<String> = row.get(11)?;
                let attachments: String = row.get(12)?;
                Ok(Message { summary: summary(row)?, body_text: row.get(10)?, has_html: html.is_some(), body_html: html, attachments: serde_json::from_str(&attachments).unwrap_or_default() })
            }).optional().map_err(storage_error)?.ok_or(MailError::NotFound)
        })
    }
}

struct SearchProgress<'a>(&'a rusqlite::Connection);
impl Drop for SearchProgress<'_> {
    fn drop(&mut self) {
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

fn search_error(error: rusqlite::Error) -> MailError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::OperationInterrupted =>
        {
            MailError::Cancelled
        }
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.contains("fts5")
                || message.contains("syntax error")
                || message.contains("unterminated") =>
        {
            MailError::InvalidSearch("check the search expression".into())
        }
        _ => storage_error(error),
    }
}
