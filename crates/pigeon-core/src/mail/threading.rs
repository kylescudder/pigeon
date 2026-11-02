//! Header-only, account-scoped connected components. Placeholder nodes retain
//! links through absent ancestors; subjects never create links. Recomputed from
//! visible rows so moves, epoch replacement and deletion cannot leave stale IDs.
use super::cache::{SUMMARY, VISIBLE, summary};
use super::{
    Conversation, MailEngine, MailError, MessageSummary, SearchQuery, SearchResults, storage_error,
};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

fn ids(value: &str) -> Vec<String> {
    let candidates: Vec<&str> = if value.contains('<') {
        value
            .split('<')
            .skip(1)
            .filter_map(|part| part.split_once('>').map(|(id, _)| id))
            .collect()
    } else {
        value.split_whitespace().collect()
    };
    candidates
        .into_iter()
        .filter_map(|id| {
            let (local, domain) = id.rsplit_once('@')?;
            if local.contains('@')
                || local.is_empty()
                || domain.is_empty()
                || id.len() > 998
                || id
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || "<>()\"".contains(c))
            {
                return None;
            }
            Some(format!("{local}@{}", domain.to_ascii_lowercase()))
        })
        .collect()
}

struct Graph {
    roots: HashMap<i64, usize>,
    incomplete: HashSet<usize>,
}
fn root(parents: &mut [usize], mut n: usize) -> usize {
    while parents[n] != n {
        parents[n] = parents[parents[n]];
        n = parents[n];
    }
    n
}
fn graph(db: &Connection, include_junk: bool, account: Option<&str>) -> Result<Graph, MailError> {
    let sql = format!(
        "SELECT m.id,m.account,m.message_id,m.reply_refs,m.in_reply_to FROM messages m JOIN mail_folders f ON f.account=m.account AND f.name=m.folder WHERE {VISIBLE} AND (?1 IS NULL OR m.account=?1) {} ORDER BY m.id",
        if include_junk {
            ""
        } else {
            "AND f.kind NOT IN ('junk','trash')"
        }
    );
    let rows = db
        .prepare(&sql)
        .map_err(storage_error)?
        .query_map([account], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(storage_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_error)?;
    let mut parents: Vec<usize> = (0..rows.len()).collect();
    let mut nodes = HashMap::new();
    let mut present = HashSet::new();
    let mut missing = Vec::new();
    for (index, (_, account, own, refs, reply)) in rows.iter().enumerate() {
        let own = ids(own);
        // Ambiguous multiple Message-IDs must not merge unrelated messages.
        if own.len() == 1 {
            present.insert((account.clone(), own[0].clone()));
        }
        let links = if own.len() == 1 { own } else { Vec::new() };
        for id in links.into_iter().chain(ids(refs)).chain(ids(reply)) {
            let key = (account.clone(), id);
            let node = *nodes.entry(key.clone()).or_insert_with(|| {
                let node = parents.len();
                parents.push(node);
                node
            });
            let a = root(&mut parents, index);
            let b = root(&mut parents, node);
            // Always attach the larger root: deterministic and cycle-safe.
            parents[a.max(b)] = a.min(b);
            missing.push((index, key));
        }
    }
    let roots: HashMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.0, root(&mut parents, i)))
        .collect();
    let incomplete = missing
        .into_iter()
        .filter(|(_, key)| !present.contains(key))
        .map(|(i, _)| root(&mut parents, i))
        .collect();
    Ok(Graph { roots, incomplete })
}

pub(super) fn group_results(
    db: &Connection,
    matched: Vec<MessageSummary>,
    query: &SearchQuery,
) -> Result<SearchResults, MailError> {
    // Explicit folder/search access to Trash or Junk already passed the search
    // compiler; include those rows in connectivity, but never in matching IDs.
    let graph = graph(db, true, query.account_id.as_deref())?;
    let mut positions = HashMap::new();
    let mut messages: Vec<MessageSummary> = Vec::new();
    let mut group_messages: HashMap<i64, Vec<MessageSummary>> = HashMap::new();
    let mut groups: HashMap<i64, Vec<i64>> = HashMap::new();
    for message in matched {
        let key = graph.roots[&message.id];
        let index = *positions.entry(key).or_insert_with(|| {
            messages.push(message.clone());
            messages.len() - 1
        });
        let representative = &mut messages[index];
        representative.unread |= message.unread;
        representative.has_attachments |= message.has_attachments;
        groups
            .entry(representative.id)
            .or_default()
            .push(message.id);
        group_messages
            .entry(representative.id)
            .or_default()
            .push(message);
    }
    let total = messages.len() as u64;
    let messages: Vec<_> = messages
        .into_iter()
        .skip(query.offset.unwrap_or(0) as usize)
        .take(query.limit.unwrap_or(50).clamp(1, 100) as usize)
        .collect();
    let page_ids: HashSet<_> = messages.iter().map(|m| m.id).collect();
    groups.retain(|id, _| page_ids.contains(id));
    group_messages.retain(|id, _| page_ids.contains(id));
    Ok(SearchResults {
        messages,
        total,
        groups,
        group_messages,
    })
}

impl MailEngine {
    /// Cached context across folders of this account only. Bodies are fetched
    /// separately when opened; this method never connects to a server.
    pub fn conversation(
        &self,
        id: i64,
        include_junk_trash: bool,
    ) -> Result<Conversation, MailError> {
        self.with_store(|store| {
            let db = store.connection();
            let account: String = db.query_row("SELECT account FROM messages WHERE id=?1", [id], |r| r.get(0)).map_err(|_| MailError::NotFound)?;
            let graph = graph(db, true, Some(&account))?;
            let key = graph.roots.get(&id).ok_or(MailError::NotFound)?;
            let sql = format!("SELECT {SUMMARY} FROM messages m JOIN mail_folders f ON f.account=m.account AND f.name=m.folder WHERE m.account=(SELECT account FROM messages WHERE id=?1) AND {VISIBLE} AND (?2 OR f.kind NOT IN ('junk','trash')) ORDER BY m.date,m.id");
            let messages = db.prepare(&sql).map_err(storage_error)?.query_map(rusqlite::params![id, include_junk_trash], summary).map_err(storage_error)?
                .collect::<rusqlite::Result<Vec<_>>>().map_err(storage_error)?
                .into_iter().filter(|m| graph.roots.get(&m.id) == Some(key)).collect();
            Ok(Conversation { messages, incomplete: graph.incomplete.contains(key) })
        })
    }
}
