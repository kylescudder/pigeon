//! Starting a reply or a forward from a stored message. The draft is made here
//! so the quoting, recipients and threading headers are tested without an
//! interface. Replies carry `In-Reply-To` and `References` so other clients
//! thread them; a forward starts a new conversation.

use chrono::DateTime;
use rusqlite::params;
use serde::Deserialize;

use super::sending::{Draft, DraftInput, invalid, parse_recipients, single_line, upsert_draft};
use super::{MailEngine, MailError, storage_error};

/// How much of the original is quoted, so a huge message cannot fill a draft.
const MAX_QUOTED: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
}

/// The parts of a stored message a reply needs.
#[derive(Debug, Clone)]
pub struct Original {
    pub account: String,
    pub sender: String,
    pub recipients: String,
    pub subject: String,
    pub body: String,
    pub date: i64,
    pub message_id: String,
}

/// The draft for `kind`, written from `own_address`'s account.
pub fn compose(original: &Original, kind: ReplyKind, own_address: &str) -> DraftInput {
    let subject = single_line(&original.subject);
    let when = DateTime::from_timestamp(original.date, 0)
        .map(|date| date.format("%a, %d %b %Y %H:%M UTC").to_string())
        .unwrap_or_default();
    let mut draft = DraftInput {
        account_id: Some(original.account.clone()),
        ..DraftInput::default()
    };
    match kind {
        ReplyKind::Forward => {
            draft.subject = prefixed(&subject, "Fwd: ", &["fwd:", "fw:"]);
            draft.body = format!(
                "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {when}\nSubject: {subject}\nTo: {}\n\n{}\n",
                single_line(&original.sender),
                single_line(&original.recipients),
                clip(&original.body),
            );
        }
        ReplyKind::Reply | ReplyKind::ReplyAll => {
            draft.subject = prefixed(&subject, "Re: ", &["re:"]);
            draft.to = single_line(&original.sender);
            if kind == ReplyKind::ReplyAll {
                draft.cc = others(&original.recipients, &original.sender, own_address);
            }
            let id = bare_id(&original.message_id);
            draft.in_reply_to = id.clone();
            draft.references = id;
            let who = parse_recipients(&original.sender, "From")
                .ok()
                .and_then(|list| list.into_iter().next())
                .map(|(name, address)| name.unwrap_or(address))
                .unwrap_or_else(|| single_line(&original.sender));
            draft.body = format!(
                "\n\nOn {when}, {who} wrote:\n{}\n",
                quote(&clip(&original.body))
            );
        }
    }
    draft
}

/// The other people on the message: everyone except the replier and the sender.
fn others(recipients: &str, sender: &str, own_address: &str) -> String {
    let sender_address = parse_recipients(sender, "From")
        .ok()
        .and_then(|list| list.into_iter().next())
        .map(|(_, address)| address);
    let Ok(list) = parse_recipients(recipients, "To") else {
        return String::new();
    };
    let mut kept: Vec<(Option<String>, String)> = Vec::new();
    for (name, address) in list {
        let skip = address.eq_ignore_ascii_case(own_address)
            || sender_address
                .as_deref()
                .is_some_and(|sender| sender.eq_ignore_ascii_case(&address))
            || kept
                .iter()
                .any(|(_, known)| known.eq_ignore_ascii_case(&address));
        if !skip {
            kept.push((name, address));
        }
    }
    kept.iter()
        .map(|(name, address)| match name {
            Some(name) => format!("\"{}\" <{address}>", name.replace(['\\', '"'], "")),
            None => address.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn prefixed(subject: &str, prefix: &str, existing: &[&str]) -> String {
    let lower = subject.trim_start().to_lowercase();
    if existing.iter().any(|known| lower.starts_with(known)) {
        subject.trim().to_owned()
    } else {
        format!("{prefix}{}", subject.trim())
    }
}

/// A Message-ID without its angle brackets, as the header builder wants it.
fn bare_id(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).trim().to_owned()
}

fn clip(body: &str) -> String {
    let body = body.replace("\r\n", "\n");
    let body = body.trim_end();
    if body.len() <= MAX_QUOTED {
        return body.to_owned();
    }
    let mut end = MAX_QUOTED;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[…]", &body[..end])
}

fn quote(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl MailEngine {
    /// Saves a draft that replies to, or forwards, a stored message.
    pub fn start_reply(&self, message: i64, kind: ReplyKind) -> Result<Draft, MailError> {
        let original = self.with_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT account,sender,recipients,subject,body_text,date,message_id FROM messages WHERE id=?1",
                    params![message],
                    |row| {
                        Ok(Original {
                            account: row.get(0)?,
                            sender: row.get(1)?,
                            recipients: row.get(2)?,
                            subject: row.get(3)?,
                            body: row.get(4)?,
                            date: row.get(5)?,
                            message_id: row.get(6)?,
                        })
                    },
                )
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => MailError::NotFound,
                    other => storage_error(other),
                })
        })?;
        let account = self.account(&original.account)?;
        if original.sender.trim().is_empty() && kind != ReplyKind::Forward {
            return Err(invalid("this message has no sender to reply to"));
        }
        let draft = compose(&original, kind, &account.config.address);
        self.with_store(|store| upsert_draft(store.connection(), &draft))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original() -> Original {
        Original {
            account: "acc".into(),
            sender: "Alice Example <alice@pigeon.test>".into(),
            recipients:
                "Me <me@pigeon.test>, Bob <bob@pigeon.test>, carol@pigeon.test, ME@pigeon.test"
                    .into(),
            subject: "Quarterly receipts".into(),
            body: "Line one\n\nLine three".into(),
            date: 1793005200,
            message_id: "<abc123@pigeon.test>".into(),
        }
    }

    #[test]
    fn reply_goes_to_the_sender_quotes_and_threads() {
        let draft = compose(&original(), ReplyKind::Reply, "me@pigeon.test");
        assert_eq!(draft.to, "Alice Example <alice@pigeon.test>");
        assert_eq!(draft.cc, "");
        assert_eq!(draft.subject, "Re: Quarterly receipts");
        assert_eq!(draft.account_id.as_deref(), Some("acc"));
        assert_eq!(draft.in_reply_to, "abc123@pigeon.test");
        assert_eq!(draft.references, "abc123@pigeon.test");
        assert!(draft.body.starts_with("\n\nOn "));
        assert!(
            draft
                .body
                .contains("Alice Example wrote:\n> Line one\n>\n> Line three")
        );
    }

    #[test]
    fn reply_all_adds_the_others_but_not_me_or_the_sender() {
        let mut message = original();
        message.recipients =
            "Me <me@pigeon.test>, Bob <bob@pigeon.test>, carol@pigeon.test, ME@pigeon.test, alice@pigeon.test, bob@pigeon.test"
                .into();
        let draft = compose(&message, ReplyKind::ReplyAll, "me@pigeon.test");
        assert_eq!(draft.to, "Alice Example <alice@pigeon.test>");
        assert_eq!(draft.cc, "\"Bob\" <bob@pigeon.test>, carol@pigeon.test");
    }

    #[test]
    fn an_existing_prefix_is_not_doubled() {
        let mut message = original();
        message.subject = "RE: Quarterly receipts".into();
        assert_eq!(
            compose(&message, ReplyKind::Reply, "me@pigeon.test").subject,
            "RE: Quarterly receipts"
        );
        message.subject = "Fwd: Quarterly receipts".into();
        assert_eq!(
            compose(&message, ReplyKind::Forward, "me@pigeon.test").subject,
            "Fwd: Quarterly receipts"
        );
    }

    #[test]
    fn forward_has_no_recipient_or_threading_and_carries_the_headers() {
        let draft = compose(&original(), ReplyKind::Forward, "me@pigeon.test");
        assert_eq!(draft.subject, "Fwd: Quarterly receipts");
        assert_eq!((draft.to.as_str(), draft.cc.as_str()), ("", ""));
        assert_eq!(
            (draft.in_reply_to.as_str(), draft.references.as_str()),
            ("", "")
        );
        assert!(
            draft
                .body
                .contains("---------- Forwarded message ----------")
        );
        assert!(
            draft
                .body
                .contains("From: Alice Example <alice@pigeon.test>")
        );
        assert!(draft.body.contains("Subject: Quarterly receipts"));
        assert!(draft.body.ends_with("Line one\n\nLine three\n"));
    }

    #[test]
    fn a_missing_message_id_means_no_threading_headers() {
        let mut message = original();
        message.message_id = String::new();
        let draft = compose(&message, ReplyKind::Reply, "me@pigeon.test");
        assert_eq!(
            (draft.in_reply_to.as_str(), draft.references.as_str()),
            ("", "")
        );
    }

    #[test]
    fn newlines_in_headers_are_flattened_and_long_bodies_are_cut() {
        let mut message = original();
        message.subject = "Hello\r\nBcc: evil@x.test".into();
        message.body = "x".repeat(MAX_QUOTED + 1000);
        let draft = compose(&message, ReplyKind::Reply, "me@pigeon.test");
        assert!(!draft.subject.contains('\n'));
        assert!(draft.body.contains("> […]"));
        assert!(draft.body.len() < MAX_QUOTED + 500);
    }
}
