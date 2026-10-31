//! What a new-mail notification says. The desktop shell shows it; this decides
//! the words so the privacy rules are tested without a desktop.
//!
//! A notification may reveal a sender and subject, only a count, or nothing.
//! It never carries message text or previews. Notification servers render a
//! small markup subset, so everything taken from mail is escaped.

use crate::desktop::NotificationPolicy;
use crate::mail::MessageSummary;

const MAX_LINES: usize = 3;
const MAX_SENDER: usize = 60;
const MAX_SUBJECT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub summary: String,
    pub body: String,
}

/// The notification for newly arrived unread mail, or none.
pub fn compose(policy: NotificationPolicy, new: &[MessageSummary]) -> Option<Notification> {
    if new.is_empty() || policy == NotificationPolicy::Off {
        return None;
    }
    let count = new.len();
    let plural = |n: usize| {
        if n == 1 {
            "1 new message".to_owned()
        } else {
            format!("{n} new messages")
        }
    };
    if policy == NotificationPolicy::Count {
        return Some(Notification {
            summary: plural(count),
            body: String::new(),
        });
    }
    if let [only] = new {
        return Some(Notification {
            summary: escape(&clip(&display_name(&only.sender), MAX_SENDER)),
            body: escape(&clip(&only.subject, MAX_SUBJECT)),
        });
    }
    let mut lines: Vec<String> = new
        .iter()
        .take(MAX_LINES)
        .map(|message| {
            format!(
                "{}: {}",
                escape(&clip(&display_name(&message.sender), MAX_SENDER)),
                escape(&clip(&message.subject, MAX_SUBJECT)),
            )
        })
        .collect();
    if count > MAX_LINES {
        lines.push(format!("and {} more", count - MAX_LINES));
    }
    Some(Notification {
        summary: plural(count),
        body: lines.join("\n"),
    })
}

/// "Alice <alice@example.com>" becomes "Alice"; a bare address stays as is.
fn display_name(sender: &str) -> String {
    let sender = sender.trim();
    let name = match sender.rsplit_once('<') {
        Some((name, _)) => name.trim().trim_matches('"').trim(),
        None => "",
    };
    if name.is_empty() {
        sender.trim_matches(['<', '>']).to_owned()
    } else {
        name.to_owned()
    }
}

/// Drops control characters and cuts to `max` characters.
fn clip(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() <= max {
        clean
    } else {
        let mut cut: String = clean.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(sender: &str, subject: &str) -> MessageSummary {
        MessageSummary {
            id: 1,
            account_id: "a".into(),
            folder: "INBOX".into(),
            sender: sender.into(),
            recipients: "me@example.test".into(),
            subject: subject.into(),
            preview: "SECRET BODY TEXT".into(),
            date: 0,
            unread: true,
            has_attachments: false,
        }
    }

    #[test]
    fn nothing_when_off_or_empty() {
        let new = [message("A <a@x.test>", "Hi")];
        assert_eq!(compose(NotificationPolicy::Off, &new), None);
        assert_eq!(compose(NotificationPolicy::SenderSubject, &[]), None);
        assert_eq!(compose(NotificationPolicy::Count, &[]), None);
    }

    #[test]
    fn count_reveals_only_how_many() {
        let two = [message("A <a@x.test>", "Secret"), message("B", "Plans")];
        let note = compose(NotificationPolicy::Count, &two).unwrap();
        assert_eq!(note.summary, "2 new messages");
        assert_eq!(note.body, "");
        let one = compose(NotificationPolicy::Count, &two[..1]).unwrap();
        assert_eq!(one.summary, "1 new message");
    }

    #[test]
    fn one_message_shows_sender_name_and_subject_never_the_body() {
        let note = compose(
            NotificationPolicy::SenderSubject,
            &[message("\"Alice Example\" <alice@x.test>", "Invoice 4471")],
        )
        .unwrap();
        assert_eq!(note.summary, "Alice Example");
        assert_eq!(note.body, "Invoice 4471");
        assert!(!format!("{note:?}").contains("SECRET"));
    }

    #[test]
    fn a_bare_address_is_used_when_there_is_no_name() {
        let note = compose(
            NotificationPolicy::SenderSubject,
            &[message("<bob@x.test>", "Hi")],
        )
        .unwrap();
        assert_eq!(note.summary, "bob@x.test");
    }

    #[test]
    fn several_messages_list_a_few_and_count_the_rest() {
        let new: Vec<_> = (0..5)
            .map(|n| message(&format!("S{n} <s{n}@x.test>"), &format!("Subject {n}")))
            .collect();
        let note = compose(NotificationPolicy::SenderSubject, &new).unwrap();
        assert_eq!(note.summary, "5 new messages");
        assert_eq!(
            note.body,
            "S0: Subject 0\nS1: Subject 1\nS2: Subject 2\nand 2 more"
        );
    }

    #[test]
    fn markup_and_control_characters_from_mail_cannot_reach_the_server() {
        let note = compose(
            NotificationPolicy::SenderSubject,
            &[message(
                "<b>Evil</b> <e@x.test>",
                "<a href=\"http://x.test\">click</a> & \n\u{7}more",
            )],
        )
        .unwrap();
        assert_eq!(note.summary, "&lt;b&gt;Evil&lt;/b&gt;");
        assert!(!note.body.contains('<') && !note.body.contains('\n'));
        assert!(note.body.contains("&amp;") && note.body.contains("&lt;a href"));
    }

    #[test]
    fn long_text_is_cut() {
        let long = "x".repeat(500);
        let note = compose(NotificationPolicy::SenderSubject, &[message(&long, &long)]).unwrap();
        assert_eq!(note.summary.chars().count(), MAX_SENDER);
        assert_eq!(note.body.chars().count(), MAX_SUBJECT);
    }
}
