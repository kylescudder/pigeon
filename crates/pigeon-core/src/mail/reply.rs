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
const MAX_REFERENCES: usize = 20;

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
    pub reply_to: String,
    pub references: String,
    pub to_list: String,
    pub cc_list: String,
    /// Sent and Drafts mail can carry a Bcc line that `recipients` includes.
    pub from_own_folder: bool,
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
            let own_message = first_address(&original.sender)
                .is_some_and(|address| address.eq_ignore_ascii_case(own_address));
            // Replying to a message you sent goes to the people you sent it to.
            let to_header = original.to();
            draft.to = if own_message {
                single_line(&to_header)
            } else if original.reply_to.trim().is_empty() {
                single_line(&original.sender)
            } else {
                single_line(&original.reply_to)
            };
            if kind == ReplyKind::ReplyAll {
                let others = if own_message {
                    original.cc()
                } else {
                    format!("{}, {}", original.to(), original.cc())
                };
                draft.cc = others_of(&others, &draft.to, own_address);
            }
            let id = bare_id(&original.message_id);
            draft.in_reply_to = id.clone();
            draft.references = chain(&original.references, &id);
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

impl Original {
    fn has_lists(&self) -> bool {
        !self.to_list.is_empty() || !self.cc_list.is_empty()
    }

    /// The To header, never including Bcc. Messages stored before the header
    /// facts were kept have none: incoming mail then falls back to
    /// `recipients`, which holds Bcc only for mail you sent, so mail in Sent
    /// and Drafts gets nothing rather than risk exposing a blind recipient.
    fn to(&self) -> String {
        if self.has_lists() {
            self.to_list.clone()
        } else if self.from_own_folder {
            String::new()
        } else {
            self.recipients.clone()
        }
    }

    fn cc(&self) -> String {
        self.cc_list.clone()
    }
}

fn first_address(field: &str) -> Option<String> {
    parse_recipients(field, "From")
        .ok()
        .and_then(|list| list.into_iter().next())
        .map(|(_, address)| address)
}

/// The people in `candidates` other than the replier and those already in `to`.
fn others_of(candidates: &str, to: &str, own_address: &str) -> String {
    let Ok(list) = parse_recipients(candidates.trim_matches([',', ' ']), "To") else {
        return String::new();
    };
    let already: Vec<String> = parse_recipients(to, "To")
        .map(|list| list.into_iter().map(|(_, address)| address).collect())
        .unwrap_or_default();
    let mut kept: Vec<(Option<String>, String)> = Vec::new();
    for (name, address) in list {
        let skip = address.eq_ignore_ascii_case(own_address)
            || already
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&address))
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

/// The earlier Message-IDs of the conversation with this one added, so a
/// recipient who has only part of the thread can still place the reply.
fn chain(earlier: &str, id: &str) -> String {
    let mut ids: Vec<String> = earlier
        .split_whitespace()
        .map(bare_id)
        .filter(|known| !known.is_empty())
        .collect();
    if !id.is_empty() && ids.last().map(String::as_str) != Some(id) {
        ids.push(id.to_owned());
    }
    // Long threads keep the first message and the most recent ones.
    if ids.len() > MAX_REFERENCES {
        let tail = ids.split_off(ids.len() - (MAX_REFERENCES - 1));
        ids.truncate(1);
        ids.extend(tail);
    }
    ids.join(" ")
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
                    "SELECT m.account,m.sender,m.recipients,m.subject,m.body_text,m.date,m.message_id,m.reply_to,m.reply_refs,m.to_list,m.cc_list,coalesce(f.kind,'other') FROM messages m LEFT JOIN mail_folders f ON f.account=m.account AND f.name=m.folder WHERE m.id=?1",
                    params![message],
                    |row| {
                        let kind: String = row.get(11)?;
                        Ok(Original {
                            account: row.get(0)?,
                            sender: row.get(1)?,
                            recipients: row.get(2)?,
                            subject: row.get(3)?,
                            body: row.get(4)?,
                            date: row.get(5)?,
                            message_id: row.get(6)?,
                            reply_to: row.get(7)?,
                            references: row.get(8)?,
                            to_list: row.get(9)?,
                            cc_list: row.get(10)?,
                            from_own_folder: matches!(kind.as_str(), "sent" | "drafts"),
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
            reply_to: String::new(),
            references: String::new(),
            to_list: "Me <me@pigeon.test>, Bob <bob@pigeon.test>".into(),
            cc_list: "carol@pigeon.test, ME@pigeon.test".into(),
            from_own_folder: false,
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
        message.to_list = "Me <me@pigeon.test>, Bob <bob@pigeon.test>, alice@pigeon.test".into();
        message.cc_list = "carol@pigeon.test, ME@pigeon.test, bob@pigeon.test".into();
        let draft = compose(&message, ReplyKind::ReplyAll, "me@pigeon.test");
        assert_eq!(draft.to, "Alice Example <alice@pigeon.test>");
        // The sender is already in To, and neither I nor a duplicate is added.
        assert_eq!(draft.cc, "\"Bob\" <bob@pigeon.test>, carol@pigeon.test");
    }

    #[test]
    fn reply_all_never_copies_a_blind_recipient() {
        let mut message = original();
        message.sender = "Me <me@pigeon.test>".into();
        message.from_own_folder = true;
        // `recipients` also holds the Bcc line, as stored by sync.
        message.recipients = "alice@pigeon.test, carol@pigeon.test, secret@pigeon.test".into();
        message.to_list = "alice@pigeon.test".into();
        message.cc_list = "carol@pigeon.test".into();
        let draft = compose(&message, ReplyKind::ReplyAll, "me@pigeon.test");
        assert_eq!(draft.to, "alice@pigeon.test");
        assert_eq!(draft.cc, "carol@pigeon.test");
        assert!(!format!("{draft:?}").contains("secret"));
    }

    #[test]
    fn mail_stored_before_the_lists_never_exposes_bcc_from_sent() {
        let mut message = original();
        message.sender = "Me <me@pigeon.test>".into();
        message.from_own_folder = true;
        message.recipients = "alice@pigeon.test, secret@pigeon.test".into();
        message.to_list = String::new();
        message.cc_list = String::new();
        let draft = compose(&message, ReplyKind::ReplyAll, "me@pigeon.test");
        assert_eq!((draft.to.as_str(), draft.cc.as_str()), ("", ""));
        // Incoming old mail has no Bcc line, so its recipients are safe to use.
        let mut incoming = original();
        incoming.to_list = String::new();
        incoming.cc_list = String::new();
        incoming.recipients = "Me <me@pigeon.test>, Bob <bob@pigeon.test>".into();
        let draft = compose(&incoming, ReplyKind::ReplyAll, "me@pigeon.test");
        assert_eq!(draft.cc, "\"Bob\" <bob@pigeon.test>");
    }

    #[test]
    fn replying_to_my_own_message_goes_to_who_i_wrote_to() {
        let mut message = original();
        message.sender = "Me <me@pigeon.test>".into();
        message.to_list = "Alice <alice@pigeon.test>".into();
        message.cc_list = "carol@pigeon.test".into();
        let reply = compose(&message, ReplyKind::Reply, "me@pigeon.test");
        assert_eq!(reply.to, "Alice <alice@pigeon.test>");
        assert_eq!(reply.cc, "");
        let all = compose(&message, ReplyKind::ReplyAll, "me@pigeon.test");
        assert_eq!(all.to, "Alice <alice@pigeon.test>");
        assert_eq!(all.cc, "carol@pigeon.test");
    }

    #[test]
    fn reply_to_is_preferred_over_from() {
        let mut message = original();
        message.sender = "Mailing List <bounces@list.test>".into();
        message.reply_to = "Discussion <list@list.test>".into();
        let draft = compose(&message, ReplyKind::Reply, "me@pigeon.test");
        assert_eq!(draft.to, "Discussion <list@list.test>");
        message.reply_to = String::new();
        assert_eq!(
            compose(&message, ReplyKind::Reply, "me@pigeon.test").to,
            "Mailing List <bounces@list.test>"
        );
    }

    #[test]
    fn the_references_chain_is_kept_and_extended() {
        let mut message = original();
        message.references = "<root@pigeon.test> <mid@pigeon.test>".into();
        let draft = compose(&message, ReplyKind::Reply, "me@pigeon.test");
        assert_eq!(
            draft.references,
            "root@pigeon.test mid@pigeon.test abc123@pigeon.test"
        );
        assert_eq!(draft.in_reply_to, "abc123@pigeon.test");
        // A very long thread keeps its first message and the latest ones.
        message.references = (0..40)
            .map(|n| format!("<m{n}@x.test>"))
            .collect::<Vec<_>>()
            .join(" ");
        let ids: Vec<_> = compose(&message, ReplyKind::Reply, "me@pigeon.test")
            .references
            .split(' ')
            .map(str::to_owned)
            .collect();
        assert_eq!(ids.len(), MAX_REFERENCES);
        assert_eq!(ids[0], "m0@x.test");
        assert_eq!(ids.last().unwrap(), "abc123@pigeon.test");
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
